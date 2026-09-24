//! Explicit-device, opt-in microphone processing and headphone preview.
//! No default device, driver, endpoint or Sonar setting is changed.
use crate::resample::LinearResampler;
use crate::voice::{VoiceProcessor, VoiceSettings};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, FromSample, SampleFormat, SizedSample, Stream};
use ringbuf::{HeapCons, HeapProd, HeapRb, traits::*};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct AudioDevices {
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
}

pub fn list_devices() -> Result<AudioDevices, String> {
    let host = cpal::default_host();
    Ok(AudioDevices {
        inputs: names(host.input_devices().map_err(|e| e.to_string())?),
        outputs: names(host.output_devices().map_err(|e| e.to_string())?),
    })
}

fn names(devices: impl Iterator<Item = Device>) -> Vec<String> {
    devices
        .filter_map(|device| device.description().ok().map(|d| d.name().to_owned()))
        .collect()
}

#[derive(Clone, Debug)]
pub struct MonitorStats {
    pub peak: f32,
    pub raw_peak: f32,
    pub buffered_ms: u32,
    pub overflow_samples: u64,
    pub underflow_samples: u64,
    pub device_xruns: u64,
    pub failed: bool,
    pub sample_rate: u32,
    pub output_sample_rate: u32,
    pub error: Option<String>,
}

struct SharedStats {
    peak_bits: AtomicU32,
    raw_peak_bits: AtomicU32,
    buffered_ms: AtomicU32,
    overflows: AtomicU64,
    underflows: AtomicU64,
    device_xruns: AtomicU64,
    failed: AtomicBool,
    error: Mutex<Option<String>>,
}

/// Values are published by the UI thread and sampled between audio blocks.
/// The audio callback never blocks on a UI lock or allocates a settings message.
struct LiveControls {
    high_pass_hz: AtomicU32,
    gate_threshold_db: AtomicU32,
    compressor_threshold_db: AtomicU32,
    compressor_ratio: AtomicU32,
    makeup_db: AtomicU32,
    bypass: AtomicBool,
    revision: AtomicU64,
}

impl LiveControls {
    fn new(settings: VoiceSettings) -> Self {
        Self {
            high_pass_hz: AtomicU32::new(settings.high_pass_hz.to_bits()),
            gate_threshold_db: AtomicU32::new(settings.gate_threshold_db.to_bits()),
            compressor_threshold_db: AtomicU32::new(settings.compressor_threshold_db.to_bits()),
            compressor_ratio: AtomicU32::new(settings.compressor_ratio.to_bits()),
            makeup_db: AtomicU32::new(settings.makeup_db.to_bits()),
            bypass: AtomicBool::new(false),
            revision: AtomicU64::new(2),
        }
    }

    fn update(&self, settings: VoiceSettings, bypass: bool) {
        // Mark the write in progress so the audio thread never accepts a partial snapshot.
        self.revision.fetch_add(1, Ordering::AcqRel);
        self.high_pass_hz
            .store(settings.high_pass_hz.to_bits(), Ordering::Relaxed);
        self.gate_threshold_db
            .store(settings.gate_threshold_db.to_bits(), Ordering::Relaxed);
        self.compressor_threshold_db.store(
            settings.compressor_threshold_db.to_bits(),
            Ordering::Relaxed,
        );
        self.compressor_ratio
            .store(settings.compressor_ratio.to_bits(), Ordering::Relaxed);
        self.makeup_db
            .store(settings.makeup_db.to_bits(), Ordering::Relaxed);
        self.bypass.store(bypass, Ordering::Relaxed);
        self.revision.fetch_add(1, Ordering::Release);
    }

    fn changed(&self, seen: &mut u64) -> Option<(VoiceSettings, bool)> {
        let revision = self.revision.load(Ordering::Acquire);
        if revision == *seen || !revision.is_multiple_of(2) {
            return None;
        }
        let settings = VoiceSettings {
            high_pass_hz: f32::from_bits(self.high_pass_hz.load(Ordering::Relaxed)),
            gate_threshold_db: f32::from_bits(self.gate_threshold_db.load(Ordering::Relaxed)),
            compressor_threshold_db: f32::from_bits(
                self.compressor_threshold_db.load(Ordering::Relaxed),
            ),
            compressor_ratio: f32::from_bits(self.compressor_ratio.load(Ordering::Relaxed)),
            makeup_db: f32::from_bits(self.makeup_db.load(Ordering::Relaxed)),
        };
        let bypass = self.bypass.load(Ordering::Relaxed);
        // A concurrent change is retried on the next audio block.
        if self.revision.load(Ordering::Acquire) != revision {
            return None;
        }
        *seen = revision;
        Some((settings, bypass))
    }
}

pub struct VoiceMonitor {
    input_stream: Stream,
    output_stream: Stream,
    shared: Arc<SharedStats>,
    controls: Arc<LiveControls>,
    sample_rate: u32,
    output_sample_rate: u32,
}

impl VoiceMonitor {
    pub fn start(
        input_name: &str,
        output_name: &str,
        settings: VoiceSettings,
    ) -> Result<Self, String> {
        let host = cpal::default_host();
        let input = find_exact(host.input_devices().map_err(|e| e.to_string())?, input_name)?;
        let output = find_exact(
            host.output_devices().map_err(|e| e.to_string())?,
            output_name,
        )?;
        let input_supported = input.default_input_config().map_err(|e| e.to_string())?;
        let output_supported = output.default_output_config().map_err(|e| e.to_string())?;
        let input_format = input_supported.sample_format();
        let output_format = output_supported.sample_format();
        let input_config: cpal::StreamConfig = input_supported.into();
        let output_config: cpal::StreamConfig = output_supported.into();
        let channels_in = input_config.channels as usize;
        let channels_out = output_config.channels as usize;
        if channels_in == 0 || channels_out == 0 {
            return Err("Invalid channel count".into());
        }
        let rate = input_config.sample_rate;
        let output_rate = output_config.sample_rate;
        let processor = VoiceProcessor::new(rate, settings).map_err(str::to_owned)?;
        let resampler = LinearResampler::new(rate, output_rate).map_err(str::to_owned)?;
        let rb = HeapRb::<f32>::new((rate as usize / 10).max(1024));
        let (mut producer, consumer) = rb.split();
        for _ in 0..rate / 50 {
            let _ = producer.try_push(0.0);
        }
        let controls = Arc::new(LiveControls::new(settings));
        let shared = Arc::new(SharedStats {
            peak_bits: AtomicU32::new(0),
            raw_peak_bits: AtomicU32::new(0),
            buffered_ms: AtomicU32::new(0),
            overflows: AtomicU64::new(0),
            underflows: AtomicU64::new(0),
            device_xruns: AtomicU64::new(0),
            failed: AtomicBool::new(false),
            error: Mutex::new(None),
        });
        let input_ready = Arc::new(AtomicBool::new(false));
        macro_rules! input_stream {
            ($sample:ty) => {
                build_input::<$sample>(
                    &input,
                    input_config,
                    channels_in,
                    processor,
                    producer,
                    InputContext {
                        shared: Arc::clone(&shared),
                        ready: Arc::clone(&input_ready),
                        controls: Arc::clone(&controls),
                    },
                )
            };
        }
        let input_stream = match input_format {
            SampleFormat::F32 => input_stream!(f32),
            SampleFormat::F64 => input_stream!(f64),
            SampleFormat::I8 => input_stream!(i8),
            SampleFormat::U8 => input_stream!(u8),
            SampleFormat::I16 => input_stream!(i16),
            SampleFormat::U16 => input_stream!(u16),
            SampleFormat::I24 => input_stream!(cpal::I24),
            SampleFormat::U24 => input_stream!(cpal::U24),
            SampleFormat::I32 => input_stream!(i32),
            SampleFormat::U32 => input_stream!(u32),
            SampleFormat::I64 => input_stream!(i64),
            SampleFormat::U64 => input_stream!(u64),
            other => Err(format!("Unsupported input sample format: {other:?}")),
        }?;
        macro_rules! output_stream {
            ($sample:ty) => {
                build_output::<$sample>(
                    &output,
                    output_config,
                    channels_out,
                    rate,
                    consumer,
                    resampler,
                    Arc::clone(&shared),
                )
            };
        }
        let output_stream = match output_format {
            SampleFormat::F32 => output_stream!(f32),
            SampleFormat::F64 => output_stream!(f64),
            SampleFormat::I8 => output_stream!(i8),
            SampleFormat::U8 => output_stream!(u8),
            SampleFormat::I16 => output_stream!(i16),
            SampleFormat::U16 => output_stream!(u16),
            SampleFormat::I24 => output_stream!(cpal::I24),
            SampleFormat::U24 => output_stream!(cpal::U24),
            SampleFormat::I32 => output_stream!(i32),
            SampleFormat::U32 => output_stream!(u32),
            SampleFormat::I64 => output_stream!(i64),
            SampleFormat::U64 => output_stream!(u64),
            other => Err(format!("Unsupported output sample format: {other:?}")),
        }?;
        input_stream.play().map_err(|e| e.to_string())?;
        // Wait for captured frames instead of assuming a fixed device startup time.
        let deadline = Instant::now() + Duration::from_millis(250);
        while !input_ready.load(Ordering::Acquire) && Instant::now() < deadline {
            if shared.failed.load(Ordering::Acquire) {
                return Err("Input stream failed".into());
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        if !input_ready.load(Ordering::Acquire) {
            return Err("Microphone delivered no audio".into());
        }
        output_stream.play().map_err(|e| e.to_string())?;
        Ok(Self {
            input_stream,
            output_stream,
            shared,
            controls,
            sample_rate: rate,
            output_sample_rate: output_rate,
        })
    }

    pub fn stats(&self) -> MonitorStats {
        MonitorStats {
            peak: f32::from_bits(self.shared.peak_bits.load(Ordering::Relaxed)),
            raw_peak: f32::from_bits(self.shared.raw_peak_bits.load(Ordering::Relaxed)),
            buffered_ms: self.shared.buffered_ms.load(Ordering::Relaxed),
            overflow_samples: self.shared.overflows.load(Ordering::Relaxed),
            underflow_samples: self.shared.underflows.load(Ordering::Relaxed),
            device_xruns: self.shared.device_xruns.load(Ordering::Relaxed),
            failed: self.shared.failed.load(Ordering::Acquire),
            sample_rate: self.sample_rate,
            output_sample_rate: self.output_sample_rate,
            error: self
                .shared
                .error
                .lock()
                .ok()
                .and_then(|guard| guard.clone()),
        }
    }

    /// Update DSP/bypass while the stream runs, without restarting a device.
    pub fn update_settings(&self, settings: VoiceSettings, bypass: bool) {
        self.controls.update(settings, bypass);
    }

    pub fn stop(self) {
        drop(self.input_stream);
        drop(self.output_stream);
    }
}

struct InputContext {
    shared: Arc<SharedStats>,
    ready: Arc<AtomicBool>,
    controls: Arc<LiveControls>,
}

fn build_input<T>(
    device: &Device,
    config: cpal::StreamConfig,
    channels: usize,
    mut processor: VoiceProcessor,
    mut producer: HeapProd<f32>,
    context: InputContext,
) -> Result<Stream, String>
where
    T: SizedSample + Copy + Send + 'static,
    f32: FromSample<T>,
{
    let InputContext {
        shared,
        ready,
        controls,
    } = context;
    let error_stats = Arc::clone(&shared);
    let mut seen_revision = 0;
    let mut bypass = false;
    device
        .build_input_stream(
            config,
            move |data: &[T], _| {
                let mut mono = [0.0_f32; 2048];
                for frames in data.chunks(channels * mono.len()) {
                    if let Some((settings, next_bypass)) = controls.changed(&mut seen_revision) {
                        if bypass != next_bypass {
                            processor.reset();
                        }
                        bypass = next_bypass;
                        processor.set_settings(settings);
                    }
                    let frame_count = frames.len() / channels;
                    for (dst, frame) in mono[..frame_count]
                        .iter_mut()
                        .zip(frames.chunks_exact(channels))
                    {
                        *dst = frame
                            .iter()
                            .copied()
                            .map(|sample| {
                                let value = sample.to_sample::<f32>();
                                if value.is_finite() { value } else { 0.0 }
                            })
                            .sum::<f32>()
                            / channels as f32;
                    }
                    let raw_peak = mono[..frame_count]
                        .iter()
                        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
                    shared
                        .raw_peak_bits
                        .store(raw_peak.to_bits(), Ordering::Relaxed);
                    if bypass {
                        for sample in &mut mono[..frame_count] {
                            *sample = sample.clamp(-1.0, 1.0);
                        }
                    } else {
                        processor.process_in_place(&mut mono[..frame_count]);
                    }
                    let mut peak = 0.0_f32;
                    let mut lost = 0_u64;
                    for &sample in &mono[..frame_count] {
                        peak = peak.max(sample.abs());
                        if producer.try_push(sample).is_err() {
                            lost += 1;
                        }
                    }
                    if lost != 0 {
                        shared.overflows.fetch_add(lost, Ordering::Relaxed);
                    }
                    shared.peak_bits.store(peak.to_bits(), Ordering::Relaxed);
                }
                ready.store(true, Ordering::Release);
            },
            move |error| record_stream_error(&error_stats, "input", error),
            Some(Duration::from_secs(3)),
        )
        .map_err(|e| e.to_string())
}

fn build_output<T>(
    device: &Device,
    config: cpal::StreamConfig,
    channels: usize,
    input_rate: u32,
    mut consumer: HeapCons<f32>,
    mut resampler: LinearResampler,
    shared: Arc<SharedStats>,
) -> Result<Stream, String>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    let error_stats = Arc::clone(&shared);
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _| {
                let queued = consumer.occupied_len() as f64;
                shared.buffered_ms.store(
                    ((queued * 1000.0) / input_rate as f64).round() as u32,
                    Ordering::Relaxed,
                );
                let target = input_rate as f64 * 0.03;
                let correction = ((queued - target) / input_rate as f64 * 0.1).clamp(-0.003, 0.003);
                let mut missing = 0_u64;
                for frame in data.chunks_exact_mut(channels) {
                    let (sample, lost) = resampler.next(|| consumer.try_pop(), 1.0 + correction);
                    missing += lost as u64;
                    frame.fill(T::from_sample(sample));
                }
                if missing != 0 {
                    shared.underflows.fetch_add(missing, Ordering::Relaxed);
                }
            },
            move |error| record_stream_error(&error_stats, "output", error),
            Some(Duration::from_secs(3)),
        )
        .map_err(|e| e.to_string())
}

fn record_stream_error(stats: &SharedStats, source: &str, error: cpal::Error) {
    if error.kind() == cpal::ErrorKind::Xrun {
        stats.device_xruns.fetch_add(1, Ordering::Relaxed);
        return;
    }
    if let Ok(mut guard) = stats.error.lock() {
        *guard = Some(format!("{source}: {error}"));
    }
    stats.failed.store(true, Ordering::Release);
}

fn find_exact(devices: impl Iterator<Item = Device>, name: &str) -> Result<Device, String> {
    let mut matches = devices.filter(|device| device.description().is_ok_and(|d| d.name() == name));
    let first = matches
        .next()
        .ok_or_else(|| "Device not found; refresh the list".to_owned())?;
    if matches.next().is_some() {
        return Err("Duplicate device names; selection is ambiguous".into());
    }
    Ok(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_controls_publish_new_settings_and_bypass() {
        let controls = LiveControls::new(VoiceSettings::default());
        let mut revision = 0;
        let (_, bypass) = controls.changed(&mut revision).unwrap();
        assert!(!bypass);
        assert!(controls.changed(&mut revision).is_none());
        let new_settings = VoiceSettings {
            high_pass_hz: 125.0,
            makeup_db: -3.0,
            ..VoiceSettings::default()
        };
        controls.update(new_settings, true);
        let (updated, bypass) = controls.changed(&mut revision).unwrap();
        assert_eq!(updated.high_pass_hz, 125.0);
        assert_eq!(updated.makeup_db, -3.0);
        assert!(bypass);
    }

    #[test]
    fn xrun_is_counted_without_stopping_stream() {
        let stats = SharedStats {
            peak_bits: AtomicU32::new(0),
            raw_peak_bits: AtomicU32::new(0),
            buffered_ms: AtomicU32::new(0),
            overflows: AtomicU64::new(0),
            underflows: AtomicU64::new(0),
            device_xruns: AtomicU64::new(0),
            failed: AtomicBool::new(false),
            error: Mutex::new(None),
        };
        record_stream_error(&stats, "input", cpal::ErrorKind::Xrun.into());
        assert_eq!(stats.device_xruns.load(Ordering::Relaxed), 1);
        assert!(!stats.failed.load(Ordering::Acquire));
        record_stream_error(&stats, "input", cpal::ErrorKind::StreamInvalidated.into());
        assert!(stats.failed.load(Ordering::Acquire));
        assert!(
            stats
                .error
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .starts_with("input:")
        );
    }
}

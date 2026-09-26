//! Real neural suppression preview. Input/output device callbacks only move samples;
//! a dedicated bounded worker performs resampling, RNNoise and voice DSP.
//! No system defaults, Sonar endpoints or virtual devices are modified.
use crate::clarity::VoiceClarity;
use crate::monitor::MonitorStats;
use crate::noise::{FRAME_SIZE, NeuralSuppressor, SAMPLE_RATE};
use crate::resample::LinearResampler;
use crate::voice::{VoiceProcessor, VoiceSettings};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, FromSample, SampleFormat, SizedSample, Stream};
use ringbuf::{HeapCons, HeapProd, HeapRb, traits::*};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub use crate::monitor::list_devices;

#[derive(Clone, Copy)]
struct Controls {
    settings: VoiceSettings,
    strength: u8,
    impact_strength: u8,
    clarity_strength: u8,
    bypass: bool,
}

struct WorkerContext {
    clarity: VoiceClarity,
    running: Arc<AtomicBool>,
    shared: Arc<Shared>,
    controls: Arc<Mutex<Controls>>,
    diagnostic: Arc<Mutex<DiagnosticCapture>>,
}

const DIAGNOSTIC_FRAMES: usize = 500; // 5 seconds, 48 kHz, 10 ms frames.
#[derive(Default)]
struct DiagnosticCapture {
    remaining_frames: usize,
    original: Vec<i16>,
    neural: Vec<i16>,
    before_impact: Vec<i16>,
    transient: Vec<i16>,
    processed: Vec<i16>,
    ready: bool,
}

/// All five tracks share the identical 48 kHz timeline and capture window.
pub struct DiagnosticTracks {
    pub original: Vec<i16>,
    pub neural: Vec<i16>,
    pub before_impact: Vec<i16>,
    pub transient: Vec<i16>,
    pub processed: Vec<i16>,
}

#[inline]
fn pcm16(sample: f32) -> i16 {
    if sample.is_finite() {
        (sample.clamp(-1.0, 1.0) * 32767.0).round() as i16
    } else {
        0
    }
}

struct Shared {
    raw_peak: AtomicU32,
    peak: AtomicU32,
    buffered_ms: AtomicU32,
    inference_us: AtomicU32,
    vad_bits: AtomicU32,
    overflows: AtomicU64,
    underflows: AtomicU64,
    device_xruns: AtomicU64,
    impact_events: AtomicU64,
    monitor_enabled: AtomicBool,
    failed: AtomicBool,
    error: Mutex<Option<String>>,
}

pub struct NeuralVoiceMonitor {
    _input_stream: Stream,
    _output_stream: Stream,
    worker: Option<JoinHandle<()>>,
    running: Arc<AtomicBool>,
    shared: Arc<Shared>,
    controls: Arc<Mutex<Controls>>,
    diagnostic: Arc<Mutex<DiagnosticCapture>>,
    input_rate: u32,
    output_rate: u32,
}

impl NeuralVoiceMonitor {
    #[allow(clippy::too_many_arguments)] // Explicit independent user-controlled DSP and monitor settings.
    pub fn start(
        input_name: &str,
        output_name: &str,
        settings: VoiceSettings,
        strength: u8,
        impact_strength: u8,
        clarity_strength: u8,
        bypass: bool,
        monitor_enabled: bool,
    ) -> Result<Self, String> {
        let host = cpal::default_host();
        let input = exact_device(host.input_devices().map_err(|e| e.to_string())?, input_name)?;
        let output = exact_device(
            host.output_devices().map_err(|e| e.to_string())?,
            output_name,
        )?;
        let in_supported = input.default_input_config().map_err(|e| e.to_string())?;
        let out_supported = output.default_output_config().map_err(|e| e.to_string())?;
        let in_format = in_supported.sample_format();
        let out_format = out_supported.sample_format();
        let in_config: cpal::StreamConfig = in_supported.into();
        let out_config: cpal::StreamConfig = out_supported.into();
        let in_channels = usize::from(in_config.channels);
        let out_channels = usize::from(out_config.channels);
        if in_channels == 0 || out_channels == 0 {
            return Err("Invalid channel count".into());
        }
        let input_rate = in_config.sample_rate;
        let output_rate = out_config.sample_rate;
        let input_resampler =
            LinearResampler::new(input_rate, SAMPLE_RATE).map_err(str::to_owned)?;
        let output_resampler =
            LinearResampler::new(SAMPLE_RATE, output_rate).map_err(str::to_owned)?;
        let voice = VoiceProcessor::new(SAMPLE_RATE, settings).map_err(str::to_owned)?;
        let noise = NeuralSuppressor::new(); // Model construction is outside callbacks.
        let clarity = VoiceClarity::new(SAMPLE_RATE);
        let input_rb = HeapRb::<f32>::new(((input_rate as usize) / 4).max(2048));
        let output_rb = HeapRb::<f32>::new((SAMPLE_RATE as usize / 5).max(2048));
        let (input_producer, input_consumer) = input_rb.split();
        let (mut output_producer, output_consumer) = output_rb.split();
        // Startup headroom for inference + input/output clock scheduling.
        for _ in 0..(SAMPLE_RATE / 40) {
            let _ = output_producer.try_push(0.0);
        }
        let shared = Arc::new(Shared {
            raw_peak: AtomicU32::new(0),
            peak: AtomicU32::new(0),
            buffered_ms: AtomicU32::new(0),
            inference_us: AtomicU32::new(0),
            vad_bits: AtomicU32::new(0),
            overflows: AtomicU64::new(0),
            underflows: AtomicU64::new(0),
            device_xruns: AtomicU64::new(0),
            impact_events: AtomicU64::new(0),
            monitor_enabled: AtomicBool::new(monitor_enabled),
            failed: AtomicBool::new(false),
            error: Mutex::new(None),
        });
        let controls = Arc::new(Mutex::new(Controls {
            settings,
            strength: strength.min(100),
            impact_strength: impact_strength.min(100),
            clarity_strength: clarity_strength.min(100),
            bypass,
        }));
        let diagnostic = Arc::new(Mutex::new(DiagnosticCapture::default()));
        let running = Arc::new(AtomicBool::new(true));
        let input_ready = Arc::new(AtomicBool::new(false));
        let notify_worker = Arc::new(OnceLock::<thread::Thread>::new());
        macro_rules! input_stream {
            ($sample:ty) => {
                build_input::<$sample>(
                    &input,
                    in_config,
                    in_channels,
                    input_producer,
                    Arc::clone(&shared),
                    Arc::clone(&input_ready),
                    Arc::clone(&notify_worker),
                )
            };
        }
        let input_stream = match in_format {
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
                    out_config,
                    out_channels,
                    output_consumer,
                    output_resampler,
                    Arc::clone(&shared),
                )
            };
        }
        let output_stream = match out_format {
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
        let worker_running = Arc::clone(&running);
        let worker_shared = Arc::clone(&shared);
        let worker_controls = Arc::clone(&controls);
        let worker_diagnostic = Arc::clone(&diagnostic);
        let worker = thread::Builder::new()
            .name("AsySounds-RNNoise".into())
            .spawn(move || {
                worker_loop(
                    input_consumer,
                    output_producer,
                    input_resampler,
                    voice,
                    noise,
                    input_rate,
                    WorkerContext {
                        clarity,
                        running: worker_running,
                        shared: worker_shared,
                        controls: worker_controls,
                        diagnostic: worker_diagnostic,
                    },
                )
            })
            .map_err(|e| format!("Cannot start neural audio worker: {e}"))?;
        let _ = notify_worker.set(worker.thread().clone());

        if let Err(error) = input_stream.play() {
            running.store(false, Ordering::Release);
            let _ = worker.join();
            return Err(error.to_string());
        }
        let deadline = Instant::now() + Duration::from_millis(500);
        while !input_ready.load(Ordering::Acquire) && Instant::now() < deadline {
            if shared.failed.load(Ordering::Acquire) {
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }
        if !input_ready.load(Ordering::Acquire) {
            running.store(false, Ordering::Release);
            let _ = worker.join();
            return Err("Microphone delivered no audio; refresh devices".into());
        }
        if let Err(error) = output_stream.play() {
            running.store(false, Ordering::Release);
            let _ = worker.join();
            return Err(error.to_string());
        }
        Ok(Self {
            _input_stream: input_stream,
            _output_stream: output_stream,
            worker: Some(worker),
            running,
            shared,
            controls,
            diagnostic,
            input_rate,
            output_rate,
        })
    }

    pub fn update_settings(
        &self,
        settings: VoiceSettings,
        bypass: bool,
        strength: u8,
        impact_strength: u8,
        clarity_strength: u8,
    ) {
        if let Ok(mut guard) = self.controls.lock() {
            *guard = Controls {
                settings,
                bypass,
                strength: strength.min(100),
                impact_strength: impact_strength.min(100),
                clarity_strength: clarity_strength.min(100),
            };
        }
    }

    /// Mutes only the preview output; input capture, DSP and A/B recording continue.
    /// The callback keeps draining the ring, so unmuting never plays stale audio.
    pub fn set_monitor_enabled(&self, enabled: bool) {
        self.shared
            .monitor_enabled
            .store(enabled, Ordering::Release);
    }

    pub fn monitor_enabled(&self) -> bool {
        self.shared.monitor_enabled.load(Ordering::Acquire)
    }

    /// Records both sides of the same stream only after an explicit UI action.
    /// Audio stays in memory and is never saved or transmitted by the core.
    pub fn begin_diagnostic(&self) -> Result<(), String> {
        if !self.running.load(Ordering::Acquire) || self.shared.failed.load(Ordering::Acquire) {
            return Err("Start a healthy microphone preview first".into());
        }
        let mut diagnostic = self.diagnostic.lock().map_err(|_| "Recorder unavailable")?;
        if diagnostic.remaining_frames != 0 {
            return Err("A comparison is already recording".into());
        }
        // Avoid the test playback leaking into the room or adding a second
        // sidetone path while capturing the original and processed signals.
        self.set_monitor_enabled(false);
        *diagnostic = DiagnosticCapture {
            remaining_frames: DIAGNOSTIC_FRAMES,
            original: Vec::with_capacity(DIAGNOSTIC_FRAMES * FRAME_SIZE),
            neural: Vec::with_capacity(DIAGNOSTIC_FRAMES * FRAME_SIZE),
            before_impact: Vec::with_capacity(DIAGNOSTIC_FRAMES * FRAME_SIZE),
            transient: Vec::with_capacity(DIAGNOSTIC_FRAMES * FRAME_SIZE),
            processed: Vec::with_capacity(DIAGNOSTIC_FRAMES * FRAME_SIZE),
            ready: false,
        };
        Ok(())
    }

    pub fn diagnostic_status(&self) -> (u32, bool) {
        self.diagnostic
            .lock()
            .map(|state| (state.remaining_frames as u32 * 10, state.ready))
            .unwrap_or((0, false))
    }

    pub fn take_diagnostic(&self) -> Result<DiagnosticTracks, String> {
        let mut state = self.diagnostic.lock().map_err(|_| "Recorder unavailable")?;
        if !state.ready || state.remaining_frames != 0 {
            return Err("The five-second comparison is not ready".into());
        }
        state.ready = false;
        Ok(DiagnosticTracks {
            original: std::mem::take(&mut state.original),
            neural: std::mem::take(&mut state.neural),
            before_impact: std::mem::take(&mut state.before_impact),
            transient: std::mem::take(&mut state.transient),
            processed: std::mem::take(&mut state.processed),
        })
    }

    pub fn stats(&self) -> MonitorStats {
        MonitorStats {
            peak: f32::from_bits(self.shared.peak.load(Ordering::Relaxed)),
            raw_peak: f32::from_bits(self.shared.raw_peak.load(Ordering::Relaxed)),
            buffered_ms: self.shared.buffered_ms.load(Ordering::Relaxed),
            overflow_samples: self.shared.overflows.load(Ordering::Relaxed),
            underflow_samples: self.shared.underflows.load(Ordering::Relaxed),
            device_xruns: self.shared.device_xruns.load(Ordering::Relaxed),
            failed: self.shared.failed.load(Ordering::Acquire),
            sample_rate: self.input_rate,
            output_sample_rate: self.output_rate,
            error: self
                .shared
                .error
                .lock()
                .ok()
                .and_then(|guard| guard.clone()),
        }
    }

    pub fn impact_events(&self) -> u64 {
        self.shared.impact_events.load(Ordering::Relaxed)
    }

    pub fn inference_us(&self) -> u32 {
        self.shared.inference_us.load(Ordering::Relaxed)
    }
    pub fn voice_probability(&self) -> f32 {
        f32::from_bits(self.shared.vad_bits.load(Ordering::Relaxed))
    }

    pub fn stop(mut self) {
        self.running.store(false, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        // Streams are then dropped by Rust; Windows audio defaults remain untouched.
    }
}

impl Drop for NeuralVoiceMonitor {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn worker_loop(
    mut source: HeapCons<f32>,
    mut sink: HeapProd<f32>,
    mut resampler: LinearResampler,
    mut voice: VoiceProcessor,
    mut noise: NeuralSuppressor,
    input_rate: u32,
    context: WorkerContext,
) {
    let WorkerContext {
        mut clarity,
        running,
        shared,
        controls,
        diagnostic,
    } = context;
    let mut aligned_raw = [0.0_f32; FRAME_SIZE];
    let mut frame = [0.0_f32; FRAME_SIZE];
    let mut filtered = [0.0_f32; FRAME_SIZE];
    let mut bypass_previous = false;
    // EQ coefficients and makeup gain only need recalculation on actual changes,
    // not on every 10 ms audio block.
    let mut applied_settings: Option<VoiceSettings> = None;
    let estimated_source_frame = ((FRAME_SIZE as f64 * input_rate as f64 / SAMPLE_RATE as f64)
        .ceil() as usize)
        .saturating_add(3);
    while running.load(Ordering::Acquire) {
        if source.occupied_len() < estimated_source_frame {
            // The input callback wakes the worker without audio-thread blocking.
            thread::park_timeout(Duration::from_millis(5));
            continue;
        }
        // Discard stale data if the worker fell behind rather than accumulating latency.
        if source.occupied_len() > (input_rate as usize / 8) {
            let discard_to = input_rate as usize / 30;
            let mut discarded = 0_u64;
            while source.occupied_len() > discard_to {
                if source.try_pop().is_none() {
                    break;
                }
                discarded += 1;
            }
            shared.overflows.fetch_add(discarded, Ordering::Relaxed);
        }
        for sample in &mut frame {
            *sample = resampler.next(|| source.try_pop(), 1.0).0;
        }
        let controls_now = match controls.lock() {
            Ok(guard) => *guard,
            Err(_) => {
                thread::yield_now();
                continue;
            }
        };
        let start = Instant::now();
        let vad = noise.process_frame(
            &frame,
            &mut filtered,
            controls_now.strength,
            controls_now.impact_strength,
            controls_now.bypass,
        );
        shared
            .impact_events
            .store(noise.impact_events(), Ordering::Relaxed);
        // Snapshot after RNNoise/VAD/impact, before the voice compressor and EQ.
        let transient = filtered;
        if bypass_previous != controls_now.bypass {
            voice.reset();
            clarity.reset();
            bypass_previous = controls_now.bypass;
        }
        if !controls_now.bypass {
            if applied_settings != Some(controls_now.settings) {
                voice.set_settings(controls_now.settings);
                applied_settings = Some(controls_now.settings);
            }
            voice.process_in_place(&mut filtered);
            clarity.process_in_place(&mut filtered, controls_now.clarity_strength);
        }
        // This opt-in copy runs on the DSP worker, not in either audio callback.
        // The raw frame is the delayed frame corresponding to RNNoise output.
        if let Ok(mut clip) = diagnostic.lock()
            && clip.remaining_frames > 0
        {
            clip.original.extend(aligned_raw.iter().map(|&x| pcm16(x)));
            clip.neural
                .extend(noise.neural_only().iter().map(|&x| pcm16(x)));
            clip.before_impact
                .extend(noise.before_impact().iter().map(|&x| pcm16(x)));
            clip.transient.extend(transient.iter().map(|&x| pcm16(x)));
            clip.processed.extend(filtered.iter().map(|&x| pcm16(x)));
            clip.remaining_frames -= 1;
            if clip.remaining_frames == 0 {
                clip.ready = true;
            }
        }
        aligned_raw = frame;
        shared.inference_us.store(
            start.elapsed().as_micros().min(u128::from(u32::MAX)) as u32,
            Ordering::Relaxed,
        );
        shared.vad_bits.store(vad.to_bits(), Ordering::Relaxed);
        let mut peak = 0.0_f32;
        let mut lost = 0_u64;
        for sample in filtered {
            peak = peak.max(sample.abs());
            if sink.try_push(sample).is_err() {
                lost += 1;
            }
        }
        if lost != 0 {
            shared.overflows.fetch_add(lost, Ordering::Relaxed);
        }
        shared.peak.store(peak.to_bits(), Ordering::Relaxed);
    }
}

fn build_input<T>(
    device: &Device,
    config: cpal::StreamConfig,
    channels: usize,
    mut producer: HeapProd<f32>,
    shared: Arc<Shared>,
    ready: Arc<AtomicBool>,
    notify_worker: Arc<OnceLock<thread::Thread>>,
) -> Result<Stream, String>
where
    T: SizedSample + Copy + Send + 'static,
    f32: FromSample<T>,
{
    let error_stats = Arc::clone(&shared);
    device
        .build_input_stream(
            config,
            move |data: &[T], _| {
                let mut peak = 0.0_f32;
                let mut lost = 0_u64;
                for frame in data.chunks_exact(channels) {
                    let mono = frame
                        .iter()
                        .copied()
                        .map(|sample| {
                            let value = sample.to_sample::<f32>();
                            if value.is_finite() { value } else { 0.0 }
                        })
                        .sum::<f32>()
                        / channels as f32;
                    peak = peak.max(mono.abs());
                    if producer.try_push(mono).is_err() {
                        lost += 1;
                    }
                }
                if lost != 0 {
                    shared.overflows.fetch_add(lost, Ordering::Relaxed);
                }
                shared.raw_peak.store(peak.to_bits(), Ordering::Relaxed);
                ready.store(true, Ordering::Release);
                if let Some(worker_thread) = notify_worker.get() {
                    worker_thread.unpark();
                }
            },
            move |error| record_error(&error_stats, "input", error),
            Some(Duration::from_secs(3)),
        )
        .map_err(|e| e.to_string())
}

fn build_output<T>(
    device: &Device,
    config: cpal::StreamConfig,
    channels: usize,
    mut consumer: HeapCons<f32>,
    mut resampler: LinearResampler,
    shared: Arc<Shared>,
) -> Result<Stream, String>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    let error_stats = Arc::clone(&shared);
    let mut monitor_gain = if shared.monitor_enabled.load(Ordering::Acquire) {
        1.0_f32
    } else {
        0.0_f32
    };
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _| {
                let queued = consumer.occupied_len() as f64;
                shared.buffered_ms.store(
                    ((queued * 1000.0) / SAMPLE_RATE as f64).round() as u32,
                    Ordering::Relaxed,
                );
                let correction = ((queued - SAMPLE_RATE as f64 * 0.03) / SAMPLE_RATE as f64 * 0.1)
                    .clamp(-0.003, 0.003);
                let mut missing = 0_u64;
                // A single atomic read per callback avoids a shared-memory read
                // per sample. The gain still ramps smoothly for ~10 ms.
                let target = if shared.monitor_enabled.load(Ordering::Acquire) {
                    1.0
                } else {
                    0.0
                };
                for frame in data.chunks_exact_mut(channels) {
                    let (sample, lost) = resampler.next(|| consumer.try_pop(), 1.0 + correction);
                    missing += u64::from(lost);
                    // Keep the output clock running and drain the buffer even
                    // while monitoring is off, without touching Windows volume.
                    monitor_gain += (target - monitor_gain) * 0.006;
                    frame.fill(T::from_sample(sample * monitor_gain));
                }
                if missing != 0 {
                    shared.underflows.fetch_add(missing, Ordering::Relaxed);
                }
            },
            move |error| record_error(&error_stats, "output", error),
            Some(Duration::from_secs(3)),
        )
        .map_err(|e| e.to_string())
}

fn record_error(shared: &Shared, source: &str, error: cpal::Error) {
    if error.kind() == cpal::ErrorKind::Xrun {
        shared.device_xruns.fetch_add(1, Ordering::Relaxed);
        return;
    }
    if let Ok(mut guard) = shared.error.lock() {
        *guard = Some(format!("{source}: {error}"));
    }
    shared.failed.store(true, Ordering::Release);
}

fn exact_device(devices: impl Iterator<Item = Device>, name: &str) -> Result<Device, String> {
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
    fn neural_pipeline_uses_native_sample_rate() {
        assert_eq!(FRAME_SIZE, 480);
        assert_eq!(SAMPLE_RATE, 48_000);
        assert!(LinearResampler::new(44_100, SAMPLE_RATE).is_ok());
        assert!(LinearResampler::new(SAMPLE_RATE, 44_100).is_ok());
    }
}

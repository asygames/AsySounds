use asysounds_core::audio_sessions::{list_audio_sessions, set_audio_session};
use asysounds_core::neural_monitor::{NeuralVoiceMonitor, list_devices};
use asysounds_core::voice::VoiceSettings;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use tauri::State;

struct PreviewState(Mutex<Option<NeuralVoiceMonitor>>);

#[derive(Serialize)]
struct SessionInfo {
    id: String,
    pid: u32,
    name: String,
    volume: f32,
    muted: bool,
    active: bool,
}

// Core Audio enumeration can block. Keep COM work off the WebView event loop.
#[tauri::command]
async fn audio_sessions() -> Result<Vec<SessionInfo>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        Ok(list_audio_sessions()?
            .into_iter()
            .map(|s| SessionInfo {
                id: s.id,
                pid: s.pid,
                name: s.name,
                volume: s.volume,
                muted: s.muted,
                active: s.active,
            })
            .collect())
    })
    .await
    .map_err(|e| format!("Audio session worker failed: {e}"))?
}

#[tauri::command]
async fn update_audio_session(
    id: String,
    volume: Option<f32>,
    muted: Option<bool>,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || set_audio_session(&id, volume, muted))
        .await
        .map_err(|e| format!("Audio session worker failed: {e}"))?
}

#[derive(Serialize)]
struct DeviceList {
    inputs: Vec<String>,
    outputs: Vec<String>,
    default_input: Option<String>,
    default_output: Option<String>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreviewSettings {
    high_pass_hz: f32,
    gate_threshold_db: f32,
    compressor_threshold_db: f32,
    compressor_ratio: f32,
    makeup_db: f32,
}

impl From<PreviewSettings> for VoiceSettings {
    fn from(settings: PreviewSettings) -> Self {
        Self {
            high_pass_hz: settings.high_pass_hz,
            gate_threshold_db: settings.gate_threshold_db,
            compressor_threshold_db: settings.compressor_threshold_db,
            compressor_ratio: settings.compressor_ratio,
            makeup_db: settings.makeup_db,
        }
    }
}

#[derive(Serialize)]
struct PreviewStatus {
    running: bool,
    neural_enabled: bool,
    monitor_enabled: bool,
    inference_us: u32,
    voice_probability: f32,
    impact_events: u64,
    diagnostic_remaining_ms: u32,
    diagnostic_ready: bool,
    peak: f32,
    raw_peak: f32,
    buffered_ms: u32,
    overflow_samples: u64,
    underflow_samples: u64,
    device_xruns: u64,
    failed: bool,
    sample_rate: u32,
    output_sample_rate: u32,
    error: Option<String>,
}

/// Small, explicit, in-memory A/B capture; no recordings are written to disk.
#[derive(Serialize)]
struct DiagnosticAudio {
    original_wav: String,
    processed_wav: String,
    metrics: DiagnosticMetrics,
}

/// Paired level measurements, not a subjective quality or noise-removal score.
#[derive(Serialize)]
struct DiagnosticMetrics {
    original_rms_dbfs: f32,
    processed_rms_dbfs: f32,
    original_peak_dbfs: f32,
    processed_peak_dbfs: f32,
    rms_change_db: f32,
    peak_change_db: f32,
}

fn pcm16_levels(samples: &[i16]) -> (f32, f32) {
    if samples.is_empty() {
        return (-96.0, -96.0);
    }
    let sum: f64 = samples
        .iter()
        .map(|&x| (f64::from(x) / 32768.0).powi(2))
        .sum();
    let peak = samples
        .iter()
        .map(|&x| i32::from(x).abs())
        .max()
        .unwrap_or(0) as f64
        / 32768.0;
    let rms = (sum / samples.len() as f64).sqrt();
    (
        (20.0 * rms.max(1e-6).log10()).max(-96.0) as f32,
        (20.0 * peak.max(1e-6).log10()).max(-96.0) as f32,
    )
}

fn diagnostic_metrics(original: &[i16], processed: &[i16]) -> DiagnosticMetrics {
    let (original_rms_dbfs, original_peak_dbfs) = pcm16_levels(original);
    let (processed_rms_dbfs, processed_peak_dbfs) = pcm16_levels(processed);
    DiagnosticMetrics {
        original_rms_dbfs,
        processed_rms_dbfs,
        original_peak_dbfs,
        processed_peak_dbfs,
        rms_change_db: processed_rms_dbfs - original_rms_dbfs,
        peak_change_db: processed_peak_dbfs - original_peak_dbfs,
    }
}

fn wav_data_url(samples: &[i16]) -> String {
    let payload_size = (samples.len() * 2) as u32;
    let mut bytes = Vec::with_capacity(44 + payload_size as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + payload_size).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes()); // PCM fmt chunk
    bytes.extend_from_slice(&1_u16.to_le_bytes()); // uncompressed PCM
    bytes.extend_from_slice(&1_u16.to_le_bytes()); // mono
    bytes.extend_from_slice(&48_000_u32.to_le_bytes());
    bytes.extend_from_slice(&96_000_u32.to_le_bytes());
    bytes.extend_from_slice(&2_u16.to_le_bytes()); // block alignment
    bytes.extend_from_slice(&16_u16.to_le_bytes()); // sample bits
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&payload_size.to_le_bytes());
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    format!("data:audio/wav;base64,{}", STANDARD.encode(bytes))
}

#[tauri::command]
fn begin_diagnostic(state: State<'_, PreviewState>) -> Result<(), String> {
    let guard = state
        .0
        .lock()
        .map_err(|_| "Preview state unavailable".to_owned())?;
    guard
        .as_ref()
        .ok_or_else(|| "Start microphone preview first".to_owned())?
        .begin_diagnostic()
}

#[tauri::command]
fn take_diagnostic(state: State<'_, PreviewState>) -> Result<DiagnosticAudio, String> {
    let guard = state
        .0
        .lock()
        .map_err(|_| "Preview state unavailable".to_owned())?;
    let (original, processed) = guard
        .as_ref()
        .ok_or_else(|| "Start microphone preview first".to_owned())?
        .take_diagnostic()?;
    if original.len() != processed.len() {
        return Err("Diagnostic signals were not time-aligned".into());
    }
    Ok(DiagnosticAudio {
        metrics: diagnostic_metrics(&original, &processed),
        original_wav: wav_data_url(&original),
        processed_wav: wav_data_url(&processed),
    })
}

#[tauri::command]
fn audio_devices() -> Result<DeviceList, String> {
    let devices = list_devices()?;
    Ok(DeviceList {
        inputs: devices.inputs,
        outputs: devices.outputs,
        default_input: devices.default_input,
        default_output: devices.default_output,
    })
}

// Tauri exposes these as individually named command parameters for UI compatibility.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
fn start_preview(
    input: String,
    output: String,
    settings: PreviewSettings,
    bypass: bool,
    noise_strength: u8,
    impact_strength: u8,
    clarity_strength: u8,
    monitor_enabled: bool,
    state: State<'_, PreviewState>,
) -> Result<(), String> {
    let mut guard = state
        .0
        .lock()
        .map_err(|_| "Preview state unavailable".to_owned())?;
    if guard.is_some() {
        return Err("Preview is already running".into());
    }
    let monitor = NeuralVoiceMonitor::start(
        &input,
        &output,
        settings.into(),
        noise_strength,
        impact_strength,
        clarity_strength,
        bypass,
        monitor_enabled,
    )?;
    *guard = Some(monitor);
    Ok(())
}

#[tauri::command]
fn update_preview_settings(
    settings: PreviewSettings,
    bypass: bool,
    noise_strength: u8,
    impact_strength: u8,
    clarity_strength: u8,
    state: State<'_, PreviewState>,
) -> Result<(), String> {
    let guard = state
        .0
        .lock()
        .map_err(|_| "Preview state unavailable".to_owned())?;
    if let Some(monitor) = guard.as_ref() {
        monitor.update_settings(
            settings.into(),
            bypass,
            noise_strength,
            impact_strength,
            clarity_strength,
        );
    }
    Ok(())
}

#[tauri::command]
fn set_monitor_enabled(enabled: bool, state: State<'_, PreviewState>) -> Result<(), String> {
    let guard = state
        .0
        .lock()
        .map_err(|_| "Preview state unavailable".to_owned())?;
    if let Some(monitor) = guard.as_ref() {
        monitor.set_monitor_enabled(enabled);
    }
    Ok(())
}

#[tauri::command]
fn stop_preview(state: State<'_, PreviewState>) -> Result<(), String> {
    let monitor = state
        .0
        .lock()
        .map_err(|_| "Preview state unavailable".to_owned())?
        .take();
    if let Some(monitor) = monitor {
        monitor.stop();
    }
    Ok(())
}

#[tauri::command]
fn preview_status(state: State<'_, PreviewState>) -> Result<PreviewStatus, String> {
    let guard = state
        .0
        .lock()
        .map_err(|_| "Preview state unavailable".to_owned())?;
    Ok(match guard.as_ref() {
        Some(monitor) => {
            let stats = monitor.stats();
            let (diagnostic_remaining_ms, diagnostic_ready) = monitor.diagnostic_status();
            PreviewStatus {
                running: true,
                neural_enabled: true,
                monitor_enabled: monitor.monitor_enabled(),
                inference_us: monitor.inference_us(),
                voice_probability: monitor.voice_probability(),
                impact_events: monitor.impact_events(),
                diagnostic_remaining_ms,
                diagnostic_ready,
                peak: stats.peak,
                raw_peak: stats.raw_peak,
                buffered_ms: stats.buffered_ms,
                overflow_samples: stats.overflow_samples,
                underflow_samples: stats.underflow_samples,
                device_xruns: stats.device_xruns,
                failed: stats.failed,
                sample_rate: stats.sample_rate,
                output_sample_rate: stats.output_sample_rate,
                error: stats.error,
            }
        }
        None => PreviewStatus {
            running: false,
            neural_enabled: false,
            monitor_enabled: false,
            inference_us: 0,
            voice_probability: 0.0,
            impact_events: 0,
            diagnostic_remaining_ms: 0,
            diagnostic_ready: false,
            peak: 0.0,
            raw_peak: 0.0,
            buffered_ms: 0,
            overflow_samples: 0,
            underflow_samples: 0,
            device_xruns: 0,
            failed: false,
            sample_rate: 0,
            output_sample_rate: 0,
            error: None,
        },
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(PreviewState(Mutex::new(None)))
        .invoke_handler(tauri::generate_handler![
            audio_devices,
            audio_sessions,
            update_audio_session,
            start_preview,
            update_preview_settings,
            set_monitor_enabled,
            stop_preview,
            preview_status,
            begin_diagnostic,
            take_diagnostic
        ])
        .run(tauri::generate_context!())
        .expect("AsySounds application error");
}

#[cfg(test)]
mod diagnostic_tests {
    use super::*;

    #[test]
    fn diagnostic_level_metrics_compare_same_time_aligned_signal() {
        let original = [16384_i16; 480];
        let processed = [8192_i16; 480];
        let m = diagnostic_metrics(&original, &processed);
        assert!((m.original_rms_dbfs + 6.02).abs() < 0.03);
        assert!((m.rms_change_db + 6.02).abs() < 0.03);
        assert!((m.peak_change_db + 6.02).abs() < 0.03);
        let empty = diagnostic_metrics(&[], &[]);
        assert!(empty.rms_change_db.is_finite());
    }

    #[test]
    fn wav_data_url_contains_valid_mono_pcm16_header_and_samples() {
        let data = wav_data_url(&[0, 32767, -32768]);
        let bytes = STANDARD
            .decode(data.strip_prefix("data:audio/wav;base64,").unwrap())
            .unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 42);
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(&bytes[12..16], b"fmt ");
        assert_eq!(u16::from_le_bytes(bytes[20..22].try_into().unwrap()), 1);
        assert_eq!(u16::from_le_bytes(bytes[22..24].try_into().unwrap()), 1);
        assert_eq!(
            u32::from_le_bytes(bytes[24..28].try_into().unwrap()),
            48_000
        );
        assert_eq!(u16::from_le_bytes(bytes[34..36].try_into().unwrap()), 16);
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 6);
        assert_eq!(&bytes[44..], &[0, 0, 255, 127, 0, 128]);
    }
}

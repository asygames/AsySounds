use asysounds_core::neural_monitor::{NeuralVoiceMonitor, list_devices};
use asysounds_core::voice::VoiceSettings;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use tauri::State;

struct PreviewState(Mutex<Option<NeuralVoiceMonitor>>);

#[derive(Serialize)]
struct DeviceList {
    inputs: Vec<String>,
    outputs: Vec<String>,
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
    inference_us: u32,
    voice_probability: f32,
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

#[tauri::command]
fn audio_devices() -> Result<DeviceList, String> {
    let devices = list_devices()?;
    Ok(DeviceList {
        inputs: devices.inputs,
        outputs: devices.outputs,
    })
}

#[tauri::command]
fn start_preview(
    input: String,
    output: String,
    settings: PreviewSettings,
    bypass: bool,
    noise_strength: u8,
    state: State<'_, PreviewState>,
) -> Result<(), String> {
    let mut guard = state
        .0
        .lock()
        .map_err(|_| "Preview state unavailable".to_owned())?;
    if guard.is_some() {
        return Err("Preview is already running".into());
    }
    let monitor = NeuralVoiceMonitor::start(&input, &output, settings.into(), noise_strength, bypass)?;
    *guard = Some(monitor);
    Ok(())
}

#[tauri::command]
fn update_preview_settings(
    settings: PreviewSettings,
    bypass: bool,
    noise_strength: u8,
    state: State<'_, PreviewState>,
) -> Result<(), String> {
    let guard = state
        .0
        .lock()
        .map_err(|_| "Preview state unavailable".to_owned())?;
    if let Some(monitor) = guard.as_ref() {
        monitor.update_settings(settings.into(), bypass, noise_strength);
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
            PreviewStatus {
                running: true,
                neural_enabled: true,
                inference_us: monitor.inference_us(),
                voice_probability: monitor.voice_probability(),
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
            inference_us: 0,
            voice_probability: 0.0,
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
            start_preview,
            update_preview_settings,
            stop_preview,
            preview_status
        ])
        .run(tauri::generate_context!())
        .expect("AsySounds application error");
}

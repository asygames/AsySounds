use asysounds_core::monitor::{VoiceMonitor, list_devices};
use asysounds_core::voice::VoiceSettings;
use serde::Serialize;
use std::sync::Mutex;
use tauri::State;

struct PreviewState(Mutex<Option<VoiceMonitor>>);

#[derive(Serialize)]
struct DeviceList {
    inputs: Vec<String>,
    outputs: Vec<String>,
}

#[derive(Serialize)]
struct PreviewStatus {
    running: bool,
    peak: f32,
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
    Ok(DeviceList { inputs: devices.inputs, outputs: devices.outputs })
}

#[tauri::command]
fn start_preview(input: String, output: String, state: State<'_, PreviewState>) -> Result<(), String> {
    let mut guard = state.0.lock().map_err(|_| "Preview state unavailable".to_owned())?;
    if guard.is_some() { return Err("Preview is already running".into()); }
    *guard = Some(VoiceMonitor::start(&input, &output, VoiceSettings::default())?);
    Ok(())
}

#[tauri::command]
fn stop_preview(state: State<'_, PreviewState>) -> Result<(), String> {
    let monitor = state.0.lock().map_err(|_| "Preview state unavailable".to_owned())?.take();
    if let Some(monitor) = monitor { monitor.stop(); }
    Ok(())
}

#[tauri::command]
fn preview_status(state: State<'_, PreviewState>) -> Result<PreviewStatus, String> {
    let guard = state.0.lock().map_err(|_| "Preview state unavailable".to_owned())?;
    Ok(match guard.as_ref() {
        Some(monitor) => {
            let stats = monitor.stats();
            PreviewStatus {
                running: true, peak: stats.peak, overflow_samples: stats.overflow_samples,
                underflow_samples: stats.underflow_samples, device_xruns: stats.device_xruns, failed: stats.failed,
                sample_rate: stats.sample_rate,
                output_sample_rate: stats.output_sample_rate,
                error: stats.error,
            }
        }
        None => PreviewStatus {
            running: false, peak: 0.0, overflow_samples: 0,
            underflow_samples: 0, device_xruns: 0, failed: false, sample_rate: 0, output_sample_rate: 0, error: None,
        },
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(PreviewState(Mutex::new(None)))
        .invoke_handler(tauri::generate_handler![audio_devices, start_preview, stop_preview, preview_status])
        .run(tauri::generate_context!())
        .expect("AsySounds application error");
}

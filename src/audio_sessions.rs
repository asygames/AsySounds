//! Windows Core Audio session control. No endpoint defaults, drivers or routing changes.
use windows::Win32::Foundation::{CloseHandle, RPC_E_CHANGED_MODE};
use windows::Win32::Media::Audio::{
    AudioSessionStateActive, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator,
    ISimpleAudioVolume, MMDeviceEnumerator, eMultimedia, eRender,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::core::Interface;
use windows::core::PWSTR;

#[derive(Clone, Debug)]
pub struct AudioSession {
    pub id: String,
    pub pid: u32,
    pub name: String,
    pub volume: f32,
    pub muted: bool,
    pub active: bool,
}

// Existing STA is also usable; RPC_E_CHANGED_MODE must NOT be balanced with CoUninitialize.
struct ComScope(bool);
impl ComScope {
    fn enter() -> Result<Self, String> {
        match unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok() } {
            Ok(()) => Ok(Self(true)),
            Err(e) if e.code() == RPC_E_CHANGED_MODE => Ok(Self(false)),
            Err(e) => Err(format!("COM initialization failed: {e}")),
        }
    }
}
impl Drop for ComScope {
    fn drop(&mut self) {
        if self.0 {
            unsafe {
                CoUninitialize();
            }
        }
    }
}

fn process_name(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()? };
    let mut path = [0u16; 32768];
    let mut length = path.len() as u32;
    let outcome = unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(path.as_mut_ptr()),
            &mut length,
        )
    };
    let _ = unsafe { CloseHandle(handle) };
    outcome.ok()?;
    let path = String::from_utf16_lossy(&path[..length as usize]);
    std::path::Path::new(&path)
        .file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .filter(|value| !value.is_empty())
}

fn string_from_com(ptr: windows::core::PWSTR) -> String {
    if ptr.is_null() {
        return String::new();
    }
    let text = unsafe { ptr.to_string().unwrap_or_default() };
    unsafe {
        CoTaskMemFree(Some(ptr.0.cast()));
    }
    text
}

fn sessions() -> Result<
    (
        IAudioSessionManager2,
        windows::Win32::Media::Audio::IAudioSessionEnumerator,
    ),
    String,
> {
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
            .map_err(|e| format!("Cannot open Windows audio endpoints: {e}"))?;
    let endpoint = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eMultimedia) }
        .map_err(|e| format!("No default playback endpoint: {e}"))?;
    let manager: IAudioSessionManager2 = unsafe { endpoint.Activate(CLSCTX_ALL, None) }
        .map_err(|e| format!("Cannot read playback sessions: {e}"))?;
    let list = unsafe { manager.GetSessionEnumerator() }
        .map_err(|e| format!("Cannot enumerate playback sessions: {e}"))?;
    Ok((manager, list))
}

pub fn list_audio_sessions() -> Result<Vec<AudioSession>, String> {
    let _com = ComScope::enter()?;
    let (_manager, list) = sessions()?;
    let count = unsafe { list.GetCount() }.map_err(|e| e.to_string())?;
    let mut found = Vec::new();
    for index in 0..count {
        let Ok(control) = (unsafe { list.GetSession(index) }) else {
            continue;
        };
        let Ok(control2) = control.cast::<IAudioSessionControl2>() else {
            continue;
        };
        let Ok(volume) = control.cast::<ISimpleAudioVolume>() else {
            continue;
        };
        let Ok(pid) = (unsafe { control2.GetProcessId() }) else {
            continue;
        };
        // Session instance ID is unique on this endpoint; PID alone is not sufficient.
        let Ok(identity) = (unsafe { control2.GetSessionInstanceIdentifier() }) else {
            continue;
        };
        let id = string_from_com(identity);
        if id.is_empty() {
            continue;
        }
        let name = unsafe { control.GetDisplayName() }
            .map(string_from_com)
            .unwrap_or_default();
        let value = unsafe { volume.GetMasterVolume() }.unwrap_or(1.0);
        let muted = unsafe { volume.GetMute() }.is_ok_and(|v| v.as_bool());
        let active =
            unsafe { control.GetState() }.is_ok_and(|state| state == AudioSessionStateActive);
        found.push(AudioSession {
            id,
            pid,
            name: if pid == 0 {
                "System sounds".into()
            } else if name.trim().is_empty() {
                process_name(pid).unwrap_or_else(|| format!("Application (PID {pid})"))
            } else {
                name
            },
            volume: value.clamp(0.0, 1.0),
            muted,
            active,
        });
    }
    found.sort_by(|a, b| b.active.cmp(&a.active).then_with(|| a.name.cmp(&b.name)));
    Ok(found)
}

pub fn set_audio_session(id: &str, level: Option<f32>, muted: Option<bool>) -> Result<(), String> {
    if id.is_empty() || id.len() > 4096 {
        return Err("Invalid session identity".into());
    }
    if let Some(value) = level
        && (!value.is_finite() || !(0.0..=1.0).contains(&value))
    {
        return Err("Volume must be between 0 and 1".into());
    }
    if level.is_none() && muted.is_none() {
        return Err("No change requested".into());
    }
    let _com = ComScope::enter()?;
    let (_manager, list) = sessions()?;
    let count = unsafe { list.GetCount() }.map_err(|e| e.to_string())?;
    for index in 0..count {
        let Ok(control) = (unsafe { list.GetSession(index) }) else {
            continue;
        };
        let Ok(control2) = control.cast::<IAudioSessionControl2>() else {
            continue;
        };
        let Ok(identity) = (unsafe { control2.GetSessionInstanceIdentifier() }) else {
            continue;
        };
        if string_from_com(identity) != id {
            continue;
        }
        let volume: ISimpleAudioVolume = control.cast().map_err(|e| e.to_string())?;
        if let Some(value) = level {
            unsafe { volume.SetMasterVolume(value, std::ptr::null()) }
                .map_err(|e| format!("Cannot set session volume: {e}"))?;
        }
        if let Some(value) = muted {
            unsafe { volume.SetMute(value, std::ptr::null()) }
                .map_err(|e| format!("Cannot set session mute: {e}"))?;
        }
        return Ok(());
    }
    Err("Audio session ended or moved to another output; refresh the list".into())
}

#[cfg(test)]
mod tests {
    #[test]
    fn running_process_has_a_readable_application_name() {
        assert_eq!(super::process_name(0), None);
        let name =
            super::process_name(std::process::id()).expect("current process must be queryable");
        assert!(name.to_ascii_lowercase().ends_with(".exe"));
    }

    #[test]
    fn invalid_requests_are_rejected_before_touching_windows() {
        assert!(super::set_audio_session("", Some(0.3), None).is_err());
        assert!(super::set_audio_session("id", Some(f32::NAN), None).is_err());
        assert!(super::set_audio_session("id", Some(1.1), None).is_err());
        assert!(super::set_audio_session("id", None, None).is_err());
    }
}

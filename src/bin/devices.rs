//! Read-only Windows Core Audio endpoint inventory. Never changes default devices.
#[cfg(windows)]
fn main() -> windows::core::Result<()> {
    use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
    use windows::Win32::Media::Audio::*;
    use windows::Win32::System::Com::{
        CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
    };
    unsafe {
        let initialized = match CoInitializeEx(None, COINIT_MULTITHREADED).ok() {
            Ok(()) => true,
            Err(e) if e.code() == RPC_E_CHANGED_MODE => false,
            Err(e) => return Err(e),
        };
        let result = (|| {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            for (flow, name) in [(eRender, "OUTPUT"), (eCapture, "INPUT")] {
                let collection = enumerator.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE)?;
                let count = collection.GetCount()?;
                println!("{name}: {count} active endpoint(s)");
                for i in 0..count {
                    let device = collection.Item(i)?;
                    let id = device.GetId()?;
                    let id_text = id.to_string()?;
                    windows::Win32::System::Com::CoTaskMemFree(Some(id.0 as _));
                    let default = [eConsole, eMultimedia, eCommunications]
                        .iter()
                        .enumerate()
                        .filter_map(|(idx, role)| {
                            enumerator
                                .GetDefaultAudioEndpoint(flow, *role)
                                .ok()
                                .and_then(|d| {
                                    d.GetId().ok().map(|ptr| {
                                        let matches =
                                            ptr.to_string().map(|s| s == id_text).unwrap_or(false);
                                        windows::Win32::System::Com::CoTaskMemFree(Some(
                                            ptr.0 as _,
                                        ));
                                        (idx, matches)
                                    })
                                })
                        })
                        .filter(|(_, matches)| *matches)
                        .map(|(idx, _)| ["console", "multimedia", "communications"][idx])
                        .collect::<Vec<_>>();
                    println!(
                        "  [{i}] {id_text}{}",
                        if default.is_empty() {
                            String::new()
                        } else {
                            format!(" [default: {}]", default.join(","))
                        }
                    );
                }
            }
            Ok(())
        })();
        if initialized {
            CoUninitialize();
        }
        result
    }
}
#[cfg(not(windows))]
fn main() {
    eprintln!("AsySounds endpoint inventory requires Windows.");
}

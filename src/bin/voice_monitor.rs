//! Opt-in microphone -> processed headphone preview. Does not change Windows routing.
use asysounds_core::monitor::{VoiceMonitor, list_devices};
use asysounds_core::voice::VoiceSettings;
use cpal::traits::{DeviceTrait, HostTrait};
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() == 2 && args[1] == "--list" {
        let devices = list_devices()?;
        println!("INPUT DEVICES");
        for name in devices.inputs {
            println!("  {name}");
        }
        println!("OUTPUT DEVICES");
        for name in devices.outputs {
            println!("  {name}");
        }
        return Ok(());
    }
    if args.len() == 2 && args[1] == "--formats" {
        let host = cpal::default_host();
        for device in host.input_devices()? {
            let config = device.default_input_config()?;
            println!(
                "IN  {} | {:?} | {} Hz | {} ch",
                device.description()?.name(),
                config.sample_format(),
                config.sample_rate(),
                config.channels()
            );
        }
        for device in host.output_devices()? {
            let config = device.default_output_config()?;
            println!(
                "OUT {} | {:?} | {} Hz | {} ch",
                device.description()?.name(),
                config.sample_format(),
                config.sample_rate(),
                config.channels()
            );
        }
        return Ok(());
    }
    if args.len() != 4 {
        eprintln!(
            "Usage: voice_monitor --list | --formats | <exact input name> <exact output name> <seconds 1..60>"
        );
        eprintln!(
            "Use headphones to avoid acoustic feedback. This only previews the selected devices."
        );
        std::process::exit(2);
    }
    let seconds: u64 = args[3].parse()?;
    if !(1..=60).contains(&seconds) {
        return Err("seconds must be 1..60".into());
    }
    let monitor = VoiceMonitor::start(&args[1], &args[2], VoiceSettings::default())?;
    let status = monitor.stats();
    println!(
        "Preview running for {seconds}s (input {} Hz, output {} Hz). No system routing changed.",
        status.sample_rate, status.output_sample_rate
    );
    let until = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < until {
        if monitor.stats().failed {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let stats = monitor.stats();
    monitor.stop();
    println!(
        "Preview stopped. Overflow samples: {}; underflow samples: {}; device glitches: {}",
        stats.overflow_samples, stats.underflow_samples, stats.device_xruns
    );
    if stats.failed {
        return Err(stats
            .error
            .unwrap_or_else(|| "device stream failed during preview".into())
            .into());
    }
    Ok(())
}

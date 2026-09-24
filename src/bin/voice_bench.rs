//! Offline throughput smoke test; does not claim end-to-end device latency.
use asysounds_core::voice::{VoiceProcessor, VoiceSettings};
use std::hint::black_box;
use std::time::Instant;

fn main() {
    const RATE: u32 = 48_000;
    const FRAMES: usize = 480;
    const BUFFERS: usize = 10_000;
    let mut processor = VoiceProcessor::new(RATE, VoiceSettings::default()).unwrap();
    let mut samples = [0.0_f32; FRAMES];
    let start = Instant::now();
    for i in 0..BUFFERS {
        for (n, sample) in samples.iter_mut().enumerate() {
            *sample = ((i * FRAMES + n) as f32 * 0.02618).sin() * 0.25;
        }
        processor.process_in_place(black_box(&mut samples));
        black_box(&samples);
    }
    let elapsed = start.elapsed();
    let audio_seconds = (BUFFERS * FRAMES) as f64 / RATE as f64;
    println!(
        "Processed {audio_seconds:.1} audio seconds in {:.3} wall seconds ({:.1}x real time)",
        elapsed.as_secs_f64(),
        audio_seconds / elapsed.as_secs_f64()
    );
    println!("Offline DSP only; no capture, routing, driver or device latency included.");
}

//! Offline end-to-end DSP throughput smoke test; no microphone, cable or playback opened.
//! Does not measure Windows driver latency, CPU utilization under streaming load, or sound quality.
use asysounds_core::clarity::VoiceClarity;
use asysounds_core::noise::{FRAME_SIZE, NeuralSuppressor, SAMPLE_RATE};
use asysounds_core::voice::{VoiceProcessor, VoiceSettings};
use std::hint::black_box;
use std::time::Instant;

fn main() {
    const FRAMES: usize = 6_000; // 60 seconds of 48 kHz audio.
    let mut denoiser = NeuralSuppressor::new();
    let mut voice = VoiceProcessor::new(
        SAMPLE_RATE,
        VoiceSettings {
            gate_threshold_db: -80.0,
            ..VoiceSettings::default()
        },
    )
    .expect("valid sample rate");
    let mut clarity = VoiceClarity::new(SAMPLE_RATE);
    let mut input = [0.0_f32; FRAME_SIZE];
    let mut output = [0.0_f32; FRAME_SIZE];
    let mut times = Vec::with_capacity(FRAMES);
    let mut seed = 0x1A2B_3C4D_u32;
    let start = Instant::now();
    for frame in 0..FRAMES {
        for (i, sample) in input.iter_mut().enumerate() {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let noise = (seed as f32 / u32::MAX as f32 - 0.5) * 0.015;
            let t = (frame * FRAME_SIZE + i) as f32 / SAMPLE_RATE as f32;
            let speech = if frame % 300 < 180 {
                0.11 * (std::f32::consts::TAU * 190.0 * t).sin()
            } else {
                0.0
            };
            *sample = speech + noise + if frame % 93 == 0 && i == 50 { 0.4 } else { 0.0 };
        }
        let before = Instant::now();
        black_box(denoiser.process_frame(&input, &mut output, 65, 85, false));
        voice.process_in_place(&mut output);
        clarity.process_in_place(&mut output, 55);
        black_box(&output);
        times.push(before.elapsed().as_micros() as u64);
        assert!(output.iter().all(|x| x.is_finite() && x.abs() <= 1.0));
    }
    let wall = start.elapsed().as_secs_f64();
    times.sort_unstable();
    let avg = times.iter().sum::<u64>() as f64 / FRAMES as f64;
    let p95 = times[FRAMES * 95 / 100];
    let p99 = times[FRAMES * 99 / 100];
    let max = times[FRAMES - 1];
    println!(
        "Full DSP: {FRAMES} frames / 60 audio seconds in {wall:.3} wall seconds ({:.1}x real time)",
        60.0 / wall
    );
    println!("Per 10 ms frame: mean {avg:.0} us, p95 {p95} us, p99 {p99} us, max {max} us");
    println!(
        "Frame budget 10000 us; p99 {} budget",
        if p99 < 10_000 { "within" } else { "OVER" }
    );
    println!("Synthetic offline test only; excludes audio-device latency, WebView and routing.");
}

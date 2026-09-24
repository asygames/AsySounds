//! Synthetic offline stress test. Does not open the microphone or change Windows audio.
use asysounds_core::noise::{FRAME_SIZE, NeuralSuppressor, SAMPLE_RATE};
use std::hint::black_box;
use std::time::Instant;

fn main() {
    let mut denoiser = NeuralSuppressor::new();
    let mut input = [0.0_f32; FRAME_SIZE];
    let mut output = [0.0_f32; FRAME_SIZE];
    let mut rng = 0x1234_5678_u32;
    let mut times = Vec::with_capacity(600);
    let mut raw_noise_power = 0.0_f64;
    let mut cleaned_noise_power = 0.0_f64;
    let mut vad_noise_sum = 0.0_f64;
    let mut vad_voice_sum = 0.0_f64;
    let mut count_noise = 0_u64;
    let mut count_voice = 0_u64;
    let start = Instant::now();
    for frame in 0..600 {
        let speech = (120..210).contains(&frame) || (340..430).contains(&frame);
        for (i, sample) in input.iter_mut().enumerate() {
            rng = rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let hiss = ((rng >> 8) as f32 / ((1_u32 << 24) as f32) - 0.5) * 0.025;
            let t = ((frame * FRAME_SIZE + i) as f32) / SAMPLE_RATE as f32;
            let voice = if speech {
                0.13 * (2.0 * std::f32::consts::PI * 155.0 * t).sin()
                    + 0.04 * (2.0 * std::f32::consts::PI * 310.0 * t).sin()
            } else {
                0.0
            };
            let click = if frame % 38 == 0 && (i == 20 || i == 21) {
                0.3
            } else {
                0.0
            };
            *sample = hiss + voice + click;
        }
        let before = Instant::now();
        let vad = denoiser.process_frame(&input, &mut output, 100, false);
        black_box(vad);
        if speech {
            vad_voice_sum += f64::from(vad);
            count_voice += 1;
        } else {
            vad_noise_sum += f64::from(vad);
            count_noise += 1;
        }
        times.push(before.elapsed().as_micros() as u64);
        assert!(output.iter().all(|s| s.is_finite()));
        // Exclude the first frame and the synthetic "speech" intervals.
        if frame > 2 && !speech && frame % 38 != 0 {
            raw_noise_power += input
                .iter()
                .map(|v| f64::from(*v) * f64::from(*v))
                .sum::<f64>();
            cleaned_noise_power += output
                .iter()
                .map(|v| f64::from(*v) * f64::from(*v))
                .sum::<f64>();
        }
    }
    times.sort_unstable();
    let total_ms = start.elapsed().as_secs_f64() * 1000.0;
    let mean_us = times.iter().sum::<u64>() as f64 / times.len() as f64;
    let p95_us = times[(times.len() * 95) / 100];
    let max_us = times[times.len() - 1];
    let noise_reduction_db =
        10.0 * (raw_noise_power / cleaned_noise_power.max(f64::MIN_POSITIVE)).log10();
    println!(
        "RNNoise: {} frames, {} ms audio; wall time {:.1} ms",
        times.len(),
        times.len() * 10,
        total_ms
    );
    println!(
        "Per 10ms frame: average {:.0} us, p95 {} us, maximum {} us",
        mean_us, p95_us, max_us
    );
    println!(
        "Synthetic stationary noise-only attenuation: {:.1} dB (not a real-world speech-quality test)",
        noise_reduction_db
    );
    println!(
        "VAD probabilities (synthetic): noise {:.3}; harmonic voice-like {:.3}",
        vad_noise_sum / count_noise as f64,
        vad_voice_sum / count_voice as f64
    );
    println!(
        "Frame budget: 10000 us; {}",
        if p95_us < 10_000 {
            "p95 under budget"
        } else {
            "p95 OVER budget"
        }
    );
}

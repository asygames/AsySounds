//! Local neural noise suppression, derived from RNNoise (nnnoiseless, BSD-3-Clause).
//! Fixed 480-sample / 48 kHz frames. All model allocation happens before the DSP loop.
//! The dry path is delayed by one frame to line up with RNNoise's algorithmic latency.
use crate::impact::ImpactSuppressor;
use nnnoiseless::DenoiseState;

pub const FRAME_SIZE: usize = DenoiseState::FRAME_SIZE;
pub const SAMPLE_RATE: u32 = 48_000;

pub struct NeuralSuppressor {
    model: Box<DenoiseState<'static>>,
    input_pcm: [f32; FRAME_SIZE],
    wet_pcm: [f32; FRAME_SIZE],
    previous_dry: [f32; FRAME_SIZE],
    neural_only: [f32; FRAME_SIZE], // same-timeline blend before VAD and impact
    before_impact: [f32; FRAME_SIZE], // after VAD, before transient attenuation
    primed: bool,
    vad_gain: f32,
    previous_vad: f32,
    impact: ImpactSuppressor,
}

impl NeuralSuppressor {
    pub fn new() -> Self {
        Self {
            model: DenoiseState::new(),
            input_pcm: [0.0; FRAME_SIZE],
            wet_pcm: [0.0; FRAME_SIZE],
            previous_dry: [0.0; FRAME_SIZE],
            neural_only: [0.0; FRAME_SIZE],
            before_impact: [0.0; FRAME_SIZE],
            primed: false,
            vad_gain: 1.0,
            previous_vad: 0.0,
            impact: ImpactSuppressor::new(),
        }
    }

    pub fn impact_events(&self) -> u64 {
        self.impact.events()
    }

    /// Diagnostic reference from the last processed frame; never modifies the live route.
    pub fn neural_only(&self) -> &[f32; FRAME_SIZE] {
        &self.neural_only
    }

    pub fn before_impact(&self) -> &[f32; FRAME_SIZE] {
        &self.before_impact
    }

    /// Denoise one complete frame. Strength mixes time-aligned dry and wet signals.
    /// Bypass still advances the model, preventing a stale inference state on resume.
    /// Returns the model's voice-activity probability for diagnostics.
    pub fn process_frame(
        &mut self,
        input: &[f32; FRAME_SIZE],
        output: &mut [f32; FRAME_SIZE],
        strength: u8,
        impact_strength: u8,
        bypass: bool,
    ) -> f32 {
        for (pcm, sample) in self.input_pcm.iter_mut().zip(input.iter()) {
            *pcm = if sample.is_finite() {
                sample.clamp(-1.0, 1.0)
            } else {
                0.0
            } * 32768.0;
        }
        let vad = self.model.process_frame(&mut self.wet_pcm, &self.input_pcm);
        if !self.primed {
            output.fill(0.0); // Discard RNNoise's first-frame synthesis artefacts.
            self.neural_only.fill(0.0);
            self.before_impact.fill(0.0);
            self.primed = true;
        } else {
            // At the old 55% setting almost half the unprocessed clap leaked through.
            // Bias the blend toward wet RNNoise, but preserve a true zero/dry bypass.
            let mix = if bypass { 0.0 } else { wet_mix(strength) };
            // Residual gate only reacts to RNNoise's speech estimate, not raw volume.
            // Moderate settings are deliberately forgiving of quiet speech.
            // Preserve quiet speech: the old default residual gate modulated
            // vowels at the 10-ms frame rate. Enable only mild gating above 85%.
            let gate_strength =
                ((f32::from(strength.min(100)) / 100.0 - 0.85) / 0.15).clamp(0.0, 1.0) * 0.25;
            // A stronger setting requires higher speech confidence before opening
            // the residual gate; low-strength settings preserve faint speech.
            let voice_threshold = 0.28 + 0.27 * gate_strength;
            let target = if self.previous_vad >= voice_threshold || mix == 0.0 {
                1.0
            } else {
                1.0 - 0.93 * gate_strength
            };
            let previous_gain = self.vad_gain;
            self.vad_gain +=
                (target - self.vad_gain) * if target > self.vad_gain { 0.50 } else { 0.16 };
            for (index, dst) in output.iter_mut().enumerate() {
                let dry = self.previous_dry[index];
                let wet = (self.wet_pcm[index] / 32768.0).clamp(-1.0, 1.0);
                let neural = (dry * (1.0 - mix) + wet * mix).clamp(-1.0, 1.0);
                self.neural_only[index] = neural;
                // Smooth across all 480 samples, not one discontinuous gain change
                // every 10 ms (which can add a 100-Hz buzz to quiet vowels).
                let fraction = (index + 1) as f32 / FRAME_SIZE as f32;
                let gate = if mix == 0.0 {
                    1.0
                } else {
                    previous_gain + (self.vad_gain - previous_gain) * fraction
                };
                *dst = (neural * gate).clamp(-1.0, 1.0);
            }
            self.before_impact.copy_from_slice(output);
            self.impact.process_frame(
                &self.previous_dry,
                output,
                self.previous_vad,
                impact_strength,
                bypass,
            );
        }
        self.previous_vad = vad;
        for (dst, sample) in self.previous_dry.iter_mut().zip(input.iter()) {
            *dst = if sample.is_finite() {
                sample.clamp(-1.0, 1.0)
            } else {
                0.0
            };
        }
        vad
    }
}

fn wet_mix(strength: u8) -> f32 {
    let x = f32::from(strength.min(100)) / 100.0;
    if x == 0.0 {
        0.0
    } else {
        (x / 0.65).powf(0.82).min(1.0)
    }
}

impl Default for NeuralSuppressor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn balanced_strength_is_mostly_neural_not_half_dry() {
        assert!(wet_mix(55) > 0.85);
        assert_eq!(wet_mix(0), 0.0);
        assert_eq!(wet_mix(100), 1.0);
    }
    #[test]
    fn impact_filter_is_independent_of_neural_strength() {
        let mut denoiser = NeuralSuppressor::new();
        let mut clap = [0.0; FRAME_SIZE];
        let mut seed = 0x9AE3_187Du32;
        for sample in clap.iter_mut().skip(40).take(200) {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            *sample = (((seed >> 16) as i32 - 32768) as f32 / 32768.0) * 0.6;
        }
        let mut processed = [0.0; FRAME_SIZE];
        denoiser.process_frame(&clap, &mut processed, 0, 100, false);
        denoiser.process_frame(&[0.0; FRAME_SIZE], &mut processed, 0, 100, false);
        let unprocessed_energy: f32 = clap.iter().map(|x| x * x).sum();
        let processed_energy: f32 = processed.iter().map(|x| x * x).sum();
        assert!(
            processed_energy < unprocessed_energy * 0.06,
            "clap filter must work even when neural setting is zero"
        );
        assert_eq!(denoiser.impact_events(), 1);
    }
    #[test]
    fn low_energy_click_is_ducked_in_end_to_end_neural_frame() {
        let mut suppressor = NeuralSuppressor::new();
        let mut short_click = [0.0; FRAME_SIZE];
        short_click[27..30].fill(0.11);
        let mut output = [0.0; FRAME_SIZE];
        suppressor.process_frame(&short_click, &mut output, 0, 100, false);
        suppressor.process_frame(&[0.0; FRAME_SIZE], &mut output, 0, 100, false);
        let dry_energy: f32 = short_click.iter().map(|x| x * x).sum();
        let wet_energy: f32 = output.iter().map(|x| x * x).sum();
        assert!(wet_energy < dry_energy * 0.01, "low-energy click leaked");
        assert_eq!(suppressor.impact_events(), 1);
    }
    #[test]
    fn normal_suppression_does_not_apply_a_residual_vad_gate() {
        let mut suppressor = NeuralSuppressor::new();
        let input = [0.02_f32; FRAME_SIZE];
        let mut output = [0.0_f32; FRAME_SIZE];
        for _ in 0..16 {
            suppressor.process_frame(&input, &mut output, 65, 0, false);
            assert_eq!(suppressor.neural_only(), suppressor.before_impact());
            assert_eq!(&output, suppressor.before_impact());
        }
    }
    #[test]
    fn rnnoise_frame_is_ten_milliseconds() {
        assert_eq!(FRAME_SIZE, 480);
        assert_eq!(SAMPLE_RATE, 48_000);
    }
    #[test]
    fn off_and_bypass_are_delayed_dry_and_remain_finite() {
        let mut denoiser = NeuralSuppressor::new();
        let input = std::array::from_fn(|i| (i as f32 / FRAME_SIZE as f32) * 0.2);
        let mut output = [f32::NAN; FRAME_SIZE];
        denoiser.process_frame(&input, &mut output, 0, 70, false);
        assert!(output.iter().all(|sample| *sample == 0.0));
        denoiser.process_frame(&[0.0; FRAME_SIZE], &mut output, 0, 70, false);
        assert_eq!(input, output);
        denoiser.process_frame(&input, &mut output, 100, 70, true);
        assert_eq!(output, [0.0; FRAME_SIZE]);
        denoiser.process_frame(&[0.0; FRAME_SIZE], &mut output, 100, 70, true);
        assert_eq!(input, output);
    }
    #[test]
    fn full_suppression_handles_bad_samples_without_nan() {
        let mut denoiser = NeuralSuppressor::new();
        let mut input = [0.0; FRAME_SIZE];
        input[0] = f32::NAN;
        input[1] = f32::INFINITY;
        input[2] = 12.0;
        let mut output = [0.0; FRAME_SIZE];
        for _ in 0..8 {
            let vad = denoiser.process_frame(&input, &mut output, 100, 70, false);
            assert!(vad.is_finite());
            assert!(output.iter().all(|s| s.is_finite() && s.abs() <= 1.0));
        }
    }
}

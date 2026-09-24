//! Local neural noise suppression, derived from RNNoise (nnnoiseless, BSD-3-Clause).
//! Fixed 480-sample / 48 kHz frames. All model allocation happens before the DSP loop.
//! The dry path is delayed by one frame to line up with RNNoise's algorithmic latency.
use nnnoiseless::DenoiseState;

pub const FRAME_SIZE: usize = DenoiseState::FRAME_SIZE;
pub const SAMPLE_RATE: u32 = 48_000;

pub struct NeuralSuppressor {
    model: Box<DenoiseState<'static>>,
    input_pcm: [f32; FRAME_SIZE],
    wet_pcm: [f32; FRAME_SIZE],
    previous_dry: [f32; FRAME_SIZE],
    primed: bool,
    vad_gain: f32,
}

impl NeuralSuppressor {
    pub fn new() -> Self {
        Self {
            model: DenoiseState::new(),
            input_pcm: [0.0; FRAME_SIZE],
            wet_pcm: [0.0; FRAME_SIZE],
            previous_dry: [0.0; FRAME_SIZE],
            primed: false,
            vad_gain: 1.0,
        }
    }

    /// Denoise one complete frame. Strength mixes time-aligned dry and wet signals.
    /// Bypass still advances the model, preventing a stale inference state on resume.
    /// Returns the model's voice-activity probability for diagnostics.
    pub fn process_frame(
        &mut self,
        input: &[f32; FRAME_SIZE],
        output: &mut [f32; FRAME_SIZE],
        strength: u8,
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
            self.primed = true;
        } else {
            let mix = if bypass {
                0.0
            } else {
                f32::from(strength.min(100)) / 100.0
            };
            // Residual gate only reacts to RNNoise's speech estimate, not raw volume.
            // Moderate settings are deliberately forgiving of quiet speech.
            let gate_strength = ((mix - 0.45) / 0.55).clamp(0.0, 1.0);
            // A stronger setting requires higher speech confidence before opening
            // the residual gate; low-strength settings preserve faint speech.
            let voice_threshold = 0.30 + 0.35 * gate_strength;
            let target = if vad >= voice_threshold || mix == 0.0 {
                1.0
            } else {
                1.0 - 0.93 * gate_strength
            };
            self.vad_gain +=
                (target - self.vad_gain) * if target > self.vad_gain { 0.50 } else { 0.16 };
            for (index, dst) in output.iter_mut().enumerate() {
                let dry = self.previous_dry[index];
                let wet = (self.wet_pcm[index] / 32768.0).clamp(-1.0, 1.0);
                // Bypass / Off is genuinely dry; do not apply a stale VAD gate.
                let gate = if mix == 0.0 { 1.0 } else { self.vad_gain };
                *dst = (dry * (1.0 - mix) + wet * mix)
                    .mul_add(gate, 0.0)
                    .clamp(-1.0, 1.0);
            }
        }
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

impl Default for NeuralSuppressor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        denoiser.process_frame(&input, &mut output, 0, false);
        assert!(output.iter().all(|sample| *sample == 0.0));
        denoiser.process_frame(&[0.0; FRAME_SIZE], &mut output, 0, false);
        assert_eq!(input, output);
        denoiser.process_frame(&input, &mut output, 100, true);
        assert_eq!(output, [0.0; FRAME_SIZE]);
        denoiser.process_frame(&[0.0; FRAME_SIZE], &mut output, 100, true);
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
            let vad = denoiser.process_frame(&input, &mut output, 100, false);
            assert!(vad.is_finite());
            assert!(output.iter().all(|s| s.is_finite() && s.abs() <= 1.0));
        }
    }
}

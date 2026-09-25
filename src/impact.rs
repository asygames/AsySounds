//! Conservative frame-aligned suppression for isolated broadband transients (claps/clicks).
//! Runs on the dedicated DSP worker; no heap allocation, locks or new model calls.
//! Not a replacement for multi-source speech separation during overlapping speech.
use crate::noise::FRAME_SIZE;

pub struct ImpactSuppressor {
    background_rms: f32,
    gain: f32,
    hold: u8,
    speech_hangover: u8,
    events: u64,
}

impl ImpactSuppressor {
    pub fn new() -> Self {
        Self {
            background_rms: 0.008,
            gain: 1.0,
            hold: 0,
            speech_hangover: 0,
            events: 0,
        }
    }

    pub fn events(&self) -> u64 {
        self.events
    }

    /// The input and output must refer to the same 10-ms frame (RNNoise delays one frame).
    pub fn process_frame(
        &mut self,
        input: &[f32; FRAME_SIZE],
        output: &mut [f32; FRAME_SIZE],
        voice_probability: f32,
        strength: u8,
        bypass: bool,
    ) {
        let mut sum = 0.0_f32;
        let mut peak = 0.0_f32;
        let mut movement = 0.0_f32;
        let mut magnitude = 0.0_f32;
        let mut last = 0.0_f32;
        for &sample in input {
            let x = if sample.is_finite() {
                sample.clamp(-1.0, 1.0)
            } else {
                0.0
            };
            sum += x * x;
            peak = peak.max(x.abs());
            magnitude += x.abs();
            movement += (x - last).abs();
            last = x;
        }
        let rms = (sum / FRAME_SIZE as f32).sqrt();
        let roughness = movement / magnitude.max(1e-5);
        let crest = peak / rms.max(1e-5);
        let rise = rms / self.background_rms.max(0.006);
        let speech_like =
            voice_probability.is_finite() && voice_probability > 0.63 && roughness < 0.76;
        if speech_like {
            self.speech_hangover = 18;
        } else {
            self.speech_hangover = self.speech_hangover.saturating_sub(1);
        }

        // Distinguish an abrupt broadband impulse from ordinary sustained vowels.
        // A concurrent voice can be damaged by hard gating, so apply only partial ducking.
        let impact = peak > 0.12
            && rms > 0.025
            && crest > 1.65
            && roughness > 0.78
            && rise > if self.speech_hangover > 0 { 4.5 } else { 2.7 };
        let amount = strength.min(100) as f32 / 100.0;
        if bypass || strength == 0 {
            self.hold = 0;
            self.gain = 1.0;
        } else if impact {
            if self.hold == 0 {
                self.events = self.events.saturating_add(1);
            }
            self.hold = 7; // includes short reflections following the initial clap
            let floor = if self.speech_hangover > 0 {
                0.40
            } else {
                0.045
            };
            self.gain = 1.0 - (1.0 - floor) * amount;
        } else if self.hold > 0 {
            self.hold -= 1;
        } else {
            // Decay in tens of milliseconds to avoid pumping on room reverberation.
            self.gain += (1.0 - self.gain) * 0.32;
        }

        // Update the baseline mostly from non-impulsive frames; avoid teaching a clap
        // to the detector as the new normal. Never let the floor grow without bound.
        if !impact {
            let observed = rms.min(self.background_rms * 1.3 + 0.001);
            self.background_rms =
                (self.background_rms * 0.985 + observed * 0.015).clamp(0.002, 0.12);
        }
        if !bypass && strength != 0 {
            for sample in output.iter_mut() {
                *sample = (*sample * self.gain).clamp(-1.0, 1.0);
            }
        }
    }
}

impl Default for ImpactSuppressor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn clap() -> [f32; FRAME_SIZE] {
        // Deterministic broadband short burst, not a prerecorded user signal.
        let mut data = [0.0; FRAME_SIZE];
        let mut seed = 0xA5C3_37E9_u32;
        for x in data.iter_mut().skip(80).take(170) {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            *x = (((seed >> 16) as i32 - 32768) as f32 / 32768.0) * 0.55;
        }
        data
    }
    #[test]
    fn isolated_clap_is_attenuated_without_mutating_dry_input() {
        let mut filter = ImpactSuppressor::new();
        let input = clap();
        let mut wet = input;
        filter.process_frame(&input, &mut wet, 0.08, 100, false);
        let input_power: f32 = input.iter().map(|x| x * x).sum();
        let wet_power: f32 = wet.iter().map(|x| x * x).sum();
        assert!(
            wet_power < input_power * 0.06,
            "isolated clap should be strongly ducked"
        );
        assert_eq!(filter.events(), 1);
    }
    #[test]
    fn steady_voiced_tone_is_not_treated_as_impact() {
        let mut filter = ImpactSuppressor::new();
        for _ in 0..60 {
            let input = std::array::from_fn(|i| {
                0.13 * (std::f32::consts::TAU * 220.0 * i as f32 / 48_000.0).sin()
            });
            let mut output = input;
            filter.process_frame(&input, &mut output, 0.95, 100, false);
            assert_eq!(input, output);
        }
        assert_eq!(filter.events(), 0);
    }
    #[test]
    fn bypass_and_off_leave_signal_unmodified() {
        let mut filter = ImpactSuppressor::new();
        let signal = clap();
        let mut out = signal;
        filter.process_frame(&signal, &mut out, 0.1, 100, true);
        assert_eq!(signal, out);
        filter.process_frame(&signal, &mut out, 0.1, 0, false);
        assert_eq!(signal, out);
    }
    #[test]
    fn simultaneous_voice_uses_a_less_aggressive_floor() {
        let mut filter = ImpactSuppressor::new();
        let tone = std::array::from_fn(|i| {
            0.10 * (std::f32::consts::TAU * 220.0 * i as f32 / 48_000.0).sin()
        });
        for _ in 0..3 {
            let mut output = tone;
            filter.process_frame(&tone, &mut output, 0.9, 100, false);
        }
        let signal = clap();
        let mut output = signal;
        filter.process_frame(&signal, &mut output, 0.9, 100, false);
        assert!(output.iter().any(|x| x.abs() > 0.04));
    }
}

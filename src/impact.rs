//! Frame-aligned attenuation of short isolated microphone transients.
//! RNNoise outputs the previous 10 ms frame; the detector examines that same
//! delayed dry frame rather than trusting RNNoise's speech estimate for clicks.
//! This is conservative around speech and cannot perfectly separate overlapping sources.
use crate::noise::FRAME_SIZE;

pub struct ImpactSuppressor {
    background_rms: f32,
    gain: f32,
    hold: u8,
    speech_hangover: u8,
    events: u64,
}

impl Default for ImpactSuppressor {
    fn default() -> Self {
        Self::new()
    }
}

impl ImpactSuppressor {
    pub fn new() -> Self {
        Self {
            background_rms: 0.006,
            gain: 1.0,
            hold: 0,
            speech_hangover: 0,
            events: 0,
        }
    }

    pub fn events(&self) -> u64 {
        self.events
    }
    pub fn gain(&self) -> f32 {
        self.gain
    }

    pub fn process_frame(
        &mut self,
        input: &[f32; FRAME_SIZE],
        output: &mut [f32; FRAME_SIZE],
        voice_probability: f32,
        strength: u8,
        bypass: bool,
    ) {
        let mut sum = 0.0;
        let mut peak = 0.0_f32;
        let mut movement = 0.0;
        let mut magnitude = 0.0;
        let mut last = 0.0;
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
        let roughness = movement / magnitude.max(1e-6);
        let crest = peak / rms.max(1e-6);
        let rise = rms / self.background_rms.max(0.0025);

        // A high VAD alone can misclassify a clap as speech. Require relatively
        // periodic spectral behaviour before extending the speech protection.
        let speech_like = voice_probability.is_finite()
            && voice_probability > 0.68
            && roughness < 0.76
            && crest < 3.5;
        if speech_like {
            self.speech_hangover = 16;
        } else {
            self.speech_hangover = self.speech_hangover.saturating_sub(1);
        }

        // The old rms > 0.025 requirement silently missed clicks lasting only a
        // few samples. The impulse branch uses crest and relative peak instead.
        let loud_impulse = (peak > (self.background_rms * 4.5).max(0.032)
            && crest > 4.5
            && roughness > 0.48)
            || (peak > (self.background_rms * 5.0).max(0.070) && crest > 2.5 && roughness > 0.65);
        let wide_impact = peak > (self.background_rms * 4.0).max(0.085)
            && rms > self.background_rms.max(0.010) * 1.45
            && crest > 1.75
            && roughness > 0.76
            && rise > 1.65;
        let impact = loud_impulse || wide_impact;
        let intensity = (f32::from(strength.min(100)) / 100.0).sqrt();

        if bypass || strength == 0 {
            self.hold = 0;
            self.gain = 1.0;
        } else if impact {
            if self.hold == 0 {
                self.events = self.events.saturating_add(1);
            }
            // Suppression around ongoing speech must not aggressively mute syllables.
            let speaking = self.speech_hangover > 0;
            let floor = if speaking { 0.50 } else { 0.018 };
            self.gain = (1.0 - (1.0 - floor) * intensity).min(self.gain);
            self.hold = if speaking { 2 } else { 6 };
        } else if self.hold > 0 {
            self.hold -= 1;
        } else {
            self.gain += (1.0 - self.gain) * 0.25;
        }

        // Do not learn an impulse as the new baseline or inflate it with speech.
        if !impact && self.speech_hangover == 0 {
            let observed = rms.min(self.background_rms * 1.15 + 0.0008);
            self.background_rms =
                (self.background_rms * 0.992 + observed * 0.008).clamp(0.0015, 0.10);
        }

        if !bypass && strength != 0 {
            for x in output {
                *x = (*x * self.gain).clamp(-1.0, 1.0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clap() -> [f32; FRAME_SIZE] {
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
    fn power(signal: &[f32; FRAME_SIZE]) -> f32 {
        signal.iter().map(|x| x * x).sum()
    }
    #[test]
    fn isolated_clap_is_attenuated() {
        let mut filter = ImpactSuppressor::new();
        let original = clap();
        let mut wet = original;
        filter.process_frame(&original, &mut wet, 0.08, 100, false);
        assert!(power(&wet) < power(&original) * 0.01);
        assert_eq!(filter.events(), 1);
    }
    #[test]
    fn short_low_energy_mouse_click_is_not_ignored() {
        let mut filter = ImpactSuppressor::new();
        let mut input = [0.0; FRAME_SIZE];
        input[47..50].fill(0.11); // three samples: 0.27ms; old RMS gate missed this
        let mut processed = input;
        filter.process_frame(&input, &mut processed, 0.07, 100, false);
        assert!(
            power(&processed) < power(&input) * 0.01,
            "short click leaked"
        );
        assert_eq!(filter.events(), 1);
    }
    #[test]
    fn quiet_short_click_is_attenuated_at_default_strength() {
        let mut filter = ImpactSuppressor::new();
        let mut click = [0.0; FRAME_SIZE];
        click[33..36].fill(0.04);
        let mut out = click;
        filter.process_frame(&click, &mut out, 0.1, 85, false);
        assert!(
            power(&out) < power(&click) * 0.02,
            "quiet mouse click leaked"
        );
        assert_eq!(filter.events(), 1);
    }
    #[test]
    fn clap_reverberation_is_attenuated_for_following_frames() {
        let mut filter = ImpactSuppressor::new();
        let original = clap();
        let mut out = original;
        filter.process_frame(&original, &mut out, 0.08, 100, false);
        let tail = [0.01; FRAME_SIZE];
        let mut wet = tail;
        filter.process_frame(&tail, &mut wet, 0.08, 100, false);
        assert!(power(&wet) < power(&tail) * 0.02);
    }
    #[test]
    fn steady_voiced_tone_is_not_ducked() {
        let mut filter = ImpactSuppressor::new();
        for _ in 0..60 {
            let input = std::array::from_fn(|i| {
                0.13 * (std::f32::consts::TAU * 220.0 * i as f32 / 48_000.0).sin()
            });
            let mut out = input;
            filter.process_frame(&input, &mut out, 0.95, 100, false);
            assert_eq!(input, out);
        }
        assert_eq!(filter.events(), 0);
    }
    #[test]
    fn voice_hangover_limits_suppression_on_impacts() {
        let mut filter = ImpactSuppressor::new();
        let tone = std::array::from_fn(|i| {
            0.10 * (std::f32::consts::TAU * 220.0 * i as f32 / 48_000.0).sin()
        });
        for _ in 0..4 {
            let mut out = tone;
            filter.process_frame(&tone, &mut out, 0.9, 100, false);
        }
        let input = clap();
        let mut out = input;
        filter.process_frame(&input, &mut out, 0.9, 100, false);
        assert!(
            power(&out) > power(&input) * 0.24,
            "speech should not be hard gated"
        );
        assert!(
            power(&out) < power(&input) * 0.6,
            "clap should still be ducked"
        );
    }
    #[test]
    fn bypass_and_zero_preserve_audio() {
        let mut filter = ImpactSuppressor::new();
        let input = clap();
        let mut out = input;
        filter.process_frame(&input, &mut out, 0.1, 100, true);
        assert_eq!(input, out);
        filter.process_frame(&input, &mut out, 0.1, 0, false);
        assert_eq!(input, out);
    }
}

//! Transient suppressor for the RNNoise-aligned 10 ms microphone preview.
//! A short click is removed locally; a broadband clap gets a stronger short
//! duck. Speech protection never trusts RNNoise VAD alone (impacts can fool it).
//! This does not perform source separation when noise overlaps speech.
use crate::noise::FRAME_SIZE;

const CLICK_RADIUS: usize = 72; // 1.5 ms at 48 kHz on either side of a click.

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
        let mut peak_at = 0;
        let mut movement = 0.0;
        let mut magnitude = 0.0;
        let mut last = 0.0;
        let mut max_step = 0.0_f32;
        for (i, &sample) in input.iter().enumerate() {
            let x = if sample.is_finite() {
                sample.clamp(-1.0, 1.0)
            } else {
                0.0
            };
            sum += x * x;
            if x.abs() > peak {
                peak = x.abs();
                peak_at = i;
            }
            magnitude += x.abs();
            let step = (x - last).abs();
            movement += step;
            max_step = max_step.max(step);
            last = x;
        }
        let rms = (sum / FRAME_SIZE as f32).sqrt();
        let roughness = movement / magnitude.max(1e-6);
        let crest = peak / rms.max(1e-6);
        let rise = rms / self.background_rms.max(0.0025);
        let speech_like = voice_probability.is_finite()
            && voice_probability > 0.68
            && roughness < 0.76
            && crest < 3.5;
        if speech_like {
            self.speech_hangover = 16;
        } else {
            self.speech_hangover = self.speech_hangover.saturating_sub(1);
        }

        // Count the samples comprising the loud transient, rather than using
        // RMS over 480 samples: an ordinary mouse click may last <1 ms.
        // A click superposed on a vowel often has a low frame crest factor.
        // Isolated steep sample-to-sample steps detect it without muting a word.
        let isolated_steps = input
            .windows(2)
            .filter(|pair| (pair[1] - pair[0]).abs() > max_step * 0.42)
            .count();
        let sharp_click = peak > (self.background_rms * 4.0).max(0.045)
            && max_step > (rms * 1.1).max(0.040)
            && max_step > peak * 0.32
            && crest > 1.8
            && isolated_steps <= 12;
        let active_samples = if peak > 0.0 {
            let threshold = (peak * 0.28).max(self.background_rms * 2.0);
            input.iter().filter(|x| x.abs() > threshold).count()
        } else {
            0
        };
        let relative_peak = peak > (self.background_rms * 4.0).max(0.028);
        let narrow_click = relative_peak
            && crest > 4.0
            && (roughness > 0.36 || max_step > (rms * 4.0).max(0.025))
            && active_samples <= 52;
        let narrow_click = narrow_click || sharp_click;
        let broadband = peak > (self.background_rms * 4.0).max(0.075)
            && rms > self.background_rms.max(0.010) * 1.4
            && roughness > 0.74
            && rise > 1.55
            && crest > 1.6;
        // VAD is often high on a clap. A highly tonal voice with a high VAD
        // needs a stronger threshold than a real broadband transient.
        let impact = if speech_like {
            sharp_click || (narrow_click && crest > 6.0)
        } else {
            narrow_click || broadband
        };
        let intensity = (f32::from(strength.min(100)) / 100.0).sqrt();
        let previous_gain = self.gain;
        if bypass || strength == 0 {
            self.hold = 0;
            self.gain = 1.0;
        } else if impact {
            if self.hold == 0 {
                self.events = self.events.saturating_add(1);
            }
            let speaking = self.speech_hangover > 0;
            if speaking && narrow_click {
                // The click occupies only a few samples. Keeping a 10 ms
                // full-frame 50% duck made the entire syllable noticeably pump.
                // Carve out the attack with a smoothly tapered local window.
                self.hold = 0;
                self.gain = 1.0;
            } else {
                // Wider claps need a short frame duck, including their first
                // reflections. During speech remove more of the impact than
                // the old 0.50 floor, without hard-gating an entire word.
                let floor = if speaking { 0.18 } else { 0.012 };
                self.gain = (1.0 - (1.0 - floor) * intensity).min(self.gain);
                self.hold = if speaking { 2 } else { 7 };
            }
        } else if self.hold > 0 {
            self.hold -= 1;
        } else {
            self.gain += (1.0 - self.gain) * 0.25;
        }

        // Prevent a single impact from raising the adaptive noise baseline.
        if !impact && self.speech_hangover == 0 {
            let observed = rms.min(self.background_rms * 1.15 + 0.0008);
            self.background_rms =
                (self.background_rms * 0.992 + observed * 0.008).clamp(0.0015, 0.10);
        }

        if bypass || strength == 0 {
            return;
        }
        let local_click = impact && narrow_click && self.speech_hangover > 0;
        let fade_samples = if self.gain < previous_gain {
            24
        } else {
            FRAME_SIZE
        };
        for (index, sample) in output.iter_mut().enumerate() {
            // Frame-wise gain steps can create a 100-Hz buzz on voiced audio.
            // Fast attack suppresses the transient; slow release protects syllables.
            let fraction = ((index + 1) as f32 / fade_samples as f32).min(1.0);
            let smooth_gain = previous_gain + (self.gain - previous_gain) * fraction;
            let local = if local_click {
                let distance = index.abs_diff(peak_at);
                if distance < CLICK_RADIUS {
                    let fade = (distance as f32 / CLICK_RADIUS as f32).powi(2);
                    1.0 - 0.985 * intensity * (1.0 - fade)
                } else {
                    1.0
                }
            } else {
                1.0
            };
            *sample = (*sample * local.min(smooth_gain)).clamp(-1.0, 1.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn power(signal: &[f32; FRAME_SIZE]) -> f32 {
        signal.iter().map(|x| x * x).sum()
    }
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
    fn tone() -> [f32; FRAME_SIZE] {
        std::array::from_fn(|i| 0.10 * (std::f32::consts::TAU * 220.0 * i as f32 / 48_000.0).sin())
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
        input[47..50].fill(0.11);
        let mut out = input;
        filter.process_frame(&input, &mut out, 0.07, 100, false);
        assert!(power(&out) < power(&input) * 0.01);
        assert_eq!(filter.events(), 1);
    }
    #[test]
    fn quiet_short_click_is_attenuated_at_default_strength() {
        let mut filter = ImpactSuppressor::new();
        let mut click = [0.0; FRAME_SIZE];
        click[33..36].fill(0.04);
        let mut out = click;
        filter.process_frame(&click, &mut out, 0.1, 85, false);
        assert!(power(&out) < power(&click) * 0.02);
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
            let input = tone();
            let mut out = input;
            filter.process_frame(&input, &mut out, 0.95, 100, false);
            assert_eq!(input, out);
        }
        assert_eq!(filter.events(), 0);
    }
    #[test]
    fn short_click_during_speech_is_carved_out_without_ducking_whole_frame() {
        let mut filter = ImpactSuppressor::new();
        for _ in 0..4 {
            let input = tone();
            let mut out = input;
            filter.process_frame(&input, &mut out, 0.95, 100, false);
        }
        let mut input = tone();
        input[240] += 0.8;
        input[241] -= 0.8;
        let mut out = input;
        filter.process_frame(&input, &mut out, 0.95, 100, false);
        assert_eq!(filter.events(), 1);
        assert!(out[240].abs() < input[240].abs() * 0.06);
        assert!((out[30] - input[30]).abs() < 1e-6);
        assert!((out[460] - input[460]).abs() < 1e-6);
    }
    #[test]
    fn broadband_clap_during_speech_is_more_than_half_attenuated() {
        let mut filter = ImpactSuppressor::new();
        for _ in 0..4 {
            let input = tone();
            let mut out = input;
            filter.process_frame(&input, &mut out, 0.95, 100, false);
        }
        let input = clap();
        let mut out = input;
        filter.process_frame(&input, &mut out, 0.95, 100, false);
        assert!(power(&out) < power(&input) * 0.07);
        assert!(power(&out) > power(&input) * 0.02); // no hard gate over voice
    }
    #[test]
    fn simultaneous_voiced_tone_and_broadband_clap_is_detected() {
        let mut filter = ImpactSuppressor::new();
        for _ in 0..8 {
            let reference = tone();
            let mut out = reference;
            filter.process_frame(&reference, &mut out, 0.96, 100, false);
        }
        let mut mixed = tone();
        for (dst, noise) in mixed.iter_mut().zip(clap()) {
            *dst = (*dst + noise).clamp(-1.0, 1.0);
        }
        let mut out = mixed;
        filter.process_frame(&mixed, &mut out, 0.96, 100, false);
        assert_eq!(
            filter.events(),
            1,
            "clap overlapping speech must be detected"
        );
        assert!(power(&out) < power(&mixed) * 0.15);
    }
    #[test]
    fn modest_mouse_click_over_speech_is_caught_without_muting_word() {
        let mut filter = ImpactSuppressor::new();
        for _ in 0..10 {
            let mut output = tone();
            filter.process_frame(&tone(), &mut output, 0.98, 85, false);
        }
        let mut input = tone();
        input[238] += 0.22;
        input[239] -= 0.18;
        let mut output = input;
        filter.process_frame(&input, &mut output, 0.98, 85, false);
        assert_eq!(filter.events(), 1, "quiet click over voice was missed");
        assert!(output[238].abs() < input[238].abs() * 0.18);
        assert!((output[30] - input[30]).abs() < 1e-6);
        assert!((output[460] - input[460]).abs() < 1e-6);
    }
    #[test]
    fn impact_release_is_smooth_across_audio_frame_boundaries() {
        let mut filter = ImpactSuppressor::new();
        let impact = clap();
        let mut output = impact;
        filter.process_frame(&impact, &mut output, 0.05, 100, false);
        // The gain is held briefly, then recovers. A hard per-frame step
        // would exceed this bound on a constant low-level signal.
        let steady = [0.10_f32; FRAME_SIZE];
        let mut previous_last: Option<f32> = None;
        for _ in 0..20 {
            let mut processed = steady;
            filter.process_frame(&steady, &mut processed, 0.05, 100, false);
            if let Some(last) = previous_last {
                assert!(
                    (processed[0] - last).abs() < 0.003,
                    "gain change introduced a discontinuity between frames"
                );
            }
            previous_last = Some(processed[FRAME_SIZE - 1]);
        }
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

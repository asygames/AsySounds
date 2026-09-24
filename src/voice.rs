//! Bounded, allocation-free mono voice processing for a real-time audio callback.
//! This filter/expander/compressor cannot remove noise that overlaps speech.
use std::f32::consts::PI;

#[derive(Clone, Copy, Debug)]
pub struct VoiceSettings {
    pub high_pass_hz: f32,
    pub gate_threshold_db: f32,
    pub compressor_threshold_db: f32,
    pub compressor_ratio: f32,
    pub makeup_db: f32,
}

impl Default for VoiceSettings {
    fn default() -> Self {
        Self {
            high_pass_hz: 85.0,
            gate_threshold_db: -48.0,
            compressor_threshold_db: -20.0,
            compressor_ratio: 3.0,
            makeup_db: 3.0,
        }
    }
}

impl VoiceSettings {
    fn validated(self, sample_rate: f32) -> Self {
        let d = Self::default();
        let bounded = |value: f32, low: f32, high: f32, fallback: f32| {
            if value.is_finite() {
                value.clamp(low, high)
            } else {
                fallback
            }
        };
        Self {
            high_pass_hz: bounded(
                self.high_pass_hz,
                20.0,
                (sample_rate * 0.4).min(250.0),
                d.high_pass_hz,
            ),
            gate_threshold_db: bounded(self.gate_threshold_db, -80.0, -20.0, d.gate_threshold_db),
            compressor_threshold_db: bounded(
                self.compressor_threshold_db,
                -40.0,
                -6.0,
                d.compressor_threshold_db,
            ),
            compressor_ratio: bounded(self.compressor_ratio, 1.0, 10.0, d.compressor_ratio),
            makeup_db: bounded(self.makeup_db, -12.0, 12.0, d.makeup_db),
        }
    }
}

pub struct VoiceProcessor {
    sample_rate: f32,
    settings: VoiceSettings,
    high_pass_alpha: f32,
    previous_input: f32,
    previous_output: f32,
    envelope: f32,
    gate_gain: f32,
    compressor_gain: f32,
    makeup_gain: f32,
    gate_open: bool,
}

impl VoiceProcessor {
    pub fn new(sample_rate: u32, settings: VoiceSettings) -> Result<Self, &'static str> {
        if !(8_000..=192_000).contains(&sample_rate) {
            return Err("sample rate must be between 8000 and 192000 Hz");
        }
        let mut processor = Self {
            sample_rate: sample_rate as f32,
            settings: VoiceSettings::default(),
            high_pass_alpha: 0.0,
            previous_input: 0.0,
            previous_output: 0.0,
            envelope: 0.0,
            gate_gain: 0.0,
            compressor_gain: 1.0,
            makeup_gain: 1.0,
            gate_open: false,
        };
        processor.set_settings(settings);
        Ok(processor)
    }

    /// Apply control changes between buffers; the sample loop has no allocation or lock.
    pub fn set_settings(&mut self, settings: VoiceSettings) {
        self.settings = settings.validated(self.sample_rate);
        let rc = 1.0 / (2.0 * PI * self.settings.high_pass_hz);
        self.high_pass_alpha = rc / (rc + 1.0 / self.sample_rate);
        self.makeup_gain = 10.0_f32.powf(self.settings.makeup_db / 20.0);
    }

    pub fn reset(&mut self) {
        self.previous_input = 0.0;
        self.previous_output = 0.0;
        self.envelope = 0.0;
        self.gate_gain = 0.0;
        self.compressor_gain = 1.0;
        self.gate_open = false;
    }

    pub fn process_in_place(&mut self, samples: &mut [f32]) {
        let envelope_attack = coefficient(self.sample_rate, 5.0);
        let envelope_release = coefficient(self.sample_rate, 80.0);
        let gate_attack = coefficient(self.sample_rate, 4.0);
        let gate_release = coefficient(self.sample_rate, 120.0);
        let compressor_attack = coefficient(self.sample_rate, 8.0);
        let compressor_release = coefficient(self.sample_rate, 100.0);
        let open_level = db_to_linear(self.settings.gate_threshold_db);
        let close_level = open_level * 0.5;
        let compressor_threshold = db_to_linear(self.settings.compressor_threshold_db);

        for sample in samples {
            let input = if sample.is_finite() {
                sample.clamp(-4.0, 4.0)
            } else {
                0.0
            };
            let filtered =
                self.high_pass_alpha * (self.previous_output + input - self.previous_input);
            self.previous_input = input;
            self.previous_output = filtered;
            let amplitude = filtered.abs();
            let envelope_coeff = if amplitude > self.envelope {
                envelope_attack
            } else {
                envelope_release
            };
            self.envelope += (amplitude - self.envelope) * envelope_coeff;

            if self.gate_open {
                if self.envelope < close_level {
                    self.gate_open = false;
                }
            } else if self.envelope > open_level {
                self.gate_open = true;
            }
            let gate_target = if self.gate_open { 1.0 } else { 0.02 };
            let gate_coeff = if gate_target > self.gate_gain {
                gate_attack
            } else {
                gate_release
            };
            self.gate_gain += (gate_target - self.gate_gain) * gate_coeff;

            let target_gain = if self.envelope > compressor_threshold {
                let level_db = 20.0 * self.envelope.log10();
                db_to_linear(
                    (self.settings.compressor_threshold_db - level_db)
                        * (1.0 - 1.0 / self.settings.compressor_ratio),
                )
            } else {
                1.0
            };
            let compressor_coeff = if target_gain < self.compressor_gain {
                compressor_attack
            } else {
                compressor_release
            };
            self.compressor_gain += (target_gain - self.compressor_gain) * compressor_coeff;
            *sample = (filtered * self.gate_gain * self.compressor_gain * self.makeup_gain)
                .clamp(-1.0, 1.0);
        }
    }
}

fn coefficient(sample_rate: f32, milliseconds: f32) -> f32 {
    1.0 - (-1.0 / (sample_rate * milliseconds * 0.001)).exp()
}
fn db_to_linear(db: f32) -> f32 {
    10.0_f32.powf(db / 20.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_invalid_sample_rates() {
        assert!(VoiceProcessor::new(0, VoiceSettings::default()).is_err());
        assert!(VoiceProcessor::new(48_000, VoiceSettings::default()).is_ok());
    }
    #[test]
    fn removes_dc_and_reduces_idle_noise() {
        let mut processor = VoiceProcessor::new(48_000, VoiceSettings::default()).unwrap();
        let mut samples = vec![0.005; 48_000];
        processor.process_in_place(&mut samples);
        assert!(samples[47_000..].iter().all(|s| s.abs() < 0.001));
    }
    #[test]
    fn passes_voice_and_bounds_bad_input() {
        let mut processor = VoiceProcessor::new(48_000, VoiceSettings::default()).unwrap();
        let mut samples: Vec<f32> = (0..48_000)
            .map(|n| 0.25 * (2.0 * PI * 200.0 * n as f32 / 48_000.0).sin())
            .collect();
        samples[0] = f32::NAN;
        samples[1] = f32::INFINITY;
        processor.process_in_place(&mut samples);
        assert!(samples.iter().all(|s| s.is_finite() && s.abs() <= 1.0));
        assert!(samples[24_000..].iter().any(|s| s.abs() > 0.05));
    }
    #[test]
    fn invalid_controls_do_not_poison_processing() {
        let mut processor = VoiceProcessor::new(
            48_000,
            VoiceSettings {
                high_pass_hz: f32::NAN,
                gate_threshold_db: f32::INFINITY,
                compressor_threshold_db: f32::NEG_INFINITY,
                compressor_ratio: f32::NAN,
                makeup_db: f32::NAN,
            },
        )
        .unwrap();
        let mut samples = [0.25; 256];
        processor.process_in_place(&mut samples);
        assert!(samples.iter().all(|s| s.is_finite()));
    }
}

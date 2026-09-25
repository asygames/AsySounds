//! Gentle optional presence EQ after RNNoise and dynamics. The DSP worker owns state.
use std::f32::consts::PI;

pub struct VoiceClarity {
    lp140: f32,
    lp500: f32,
    lp2200: f32,
    lp4500: f32,
    a140: f32,
    a500: f32,
    a2200: f32,
    a4500: f32,
}

fn pole(hz: f32, sample_rate: f32) -> f32 {
    1.0 - (-2.0 * PI * hz / sample_rate).exp()
}

impl VoiceClarity {
    pub fn new(sample_rate: u32) -> Self {
        let rate = sample_rate.max(8_000) as f32;
        Self {
            lp140: 0.0,
            lp500: 0.0,
            lp2200: 0.0,
            lp4500: 0.0,
            a140: pole(140.0, rate),
            a500: pole(500.0, rate),
            a2200: pole(2200.0, rate),
            a4500: pole(4500.0, rate),
        }
    }

    pub fn reset(&mut self) {
        self.lp140 = 0.0;
        self.lp500 = 0.0;
        self.lp2200 = 0.0;
        self.lp4500 = 0.0;
    }

    /// Moderate 2.2-4.5 kHz presence lift with a smaller 140-500 Hz mud cut.
    /// State advances even when strength is zero for artifact-free adjustment.
    pub fn process_in_place(&mut self, samples: &mut [f32], strength: u8) {
        let intensity = f32::from(strength.min(100)) / 100.0;
        let presence = 0.38 * intensity;
        let mud = 0.25 * intensity;
        let headroom = 1.0 / (1.0 + 0.12 * intensity);
        for sample in samples {
            let input = if sample.is_finite() {
                sample.clamp(-1.0, 1.0)
            } else {
                0.0
            };
            self.lp140 += self.a140 * (input - self.lp140);
            self.lp500 += self.a500 * (input - self.lp500);
            self.lp2200 += self.a2200 * (input - self.lp2200);
            self.lp4500 += self.a4500 * (input - self.lp4500);
            let boost = (self.lp4500 - self.lp2200) * presence;
            let cut = (self.lp500 - self.lp140) * mud;
            *sample = ((input + boost - cut) * headroom).clamp(-1.0, 1.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn measure(hz: f32, clarity: u8) -> f32 {
        let mut filter = VoiceClarity::new(48_000);
        let mut power = 0.0_f32;
        for frame in 0..75 {
            let mut samples: [f32; 480] = std::array::from_fn(|i| {
                let time = (frame * 480 + i) as f32 / 48_000.0;
                0.10 * (2.0 * PI * hz * time).sin()
            });
            filter.process_in_place(&mut samples, clarity);
            if frame > 12 {
                power += samples.iter().map(|x| x * x).sum::<f32>();
            }
        }
        power
    }
    #[test]
    fn presence_is_more_prominent_than_low_mids() {
        let high = measure(3_000.0, 80) / measure(3_000.0, 0);
        let low = measure(320.0, 80) / measure(320.0, 0);
        assert!(
            high > low * 1.07,
            "presence should be emphasized over low mids: {high} vs {low}"
        );
        assert!(high < 1.65, "EQ must remain gentle: {high}");
    }
    #[test]
    fn zero_setting_and_invalid_samples_are_bounded() {
        let mut filter = VoiceClarity::new(48_000);
        let mut samples = [0.1; 480];
        filter.process_in_place(&mut samples, 0);
        assert!(samples.iter().all(|x| (*x - 0.1).abs() < 1e-6));
        let mut invalid = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.4];
        filter.process_in_place(&mut invalid, 100);
        assert!(invalid.iter().all(|x| x.is_finite() && x.abs() <= 1.0));
        filter.reset();
    }
}

//! Two real vocal EQ bands on the dedicated audio worker.
//! The previous first-order LP difference yielded barely audible presence changes.
//! A measured peaking EQ at 3 kHz and low-mid cut at 320 Hz replace that approximation.
use std::f32::consts::PI;

#[derive(Clone, Copy)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}
impl Biquad {
    fn unity() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
            z1: 0.0,
            z2: 0.0,
        }
    }
    /// RBJ audio-EQ peaking band; coefficients are updated between 10-ms frames.
    fn peaking(&mut self, rate: f32, hz: f32, q: f32, db: f32) {
        let amp = 10.0_f32.powf(db / 40.0);
        let omega = 2.0 * PI * hz / rate;
        let alpha = omega.sin() / (2.0 * q);
        let cos = omega.cos();
        let a0 = 1.0 + alpha / amp;
        self.b0 = (1.0 + alpha * amp) / a0;
        self.b1 = (-2.0 * cos) / a0;
        self.b2 = (1.0 - alpha * amp) / a0;
        self.a1 = (-2.0 * cos) / a0;
        self.a2 = (1.0 - alpha / amp) / a0;
    }
    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0.mul_add(x, self.z1);
        self.z1 = self.b1.mul_add(x, self.z2 - self.a1 * y);
        self.z2 = self.b2.mul_add(x, -self.a2 * y);
        y
    }
    fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }
}

pub struct VoiceClarity {
    sample_rate: f32,
    low_mid: Biquad,
    presence: Biquad,
    level: u8,
    headroom: f32,
}
impl VoiceClarity {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate: sample_rate.max(8_000) as f32,
            low_mid: Biquad::unity(),
            presence: Biquad::unity(),
            level: 0,
            headroom: 1.0,
        }
    }
    pub fn reset(&mut self) {
        self.low_mid.reset();
        self.presence.reset();
    }
    /// 0 = unchanged signal. At 100, +6 dB at 3 kHz and -3.5 dB at 320 Hz,
    /// plus 0.9 dB headroom. The spectral contrast is audible at the midpoint.
    pub fn process_in_place(&mut self, samples: &mut [f32], strength: u8) {
        let strength = strength.min(100);
        if strength != self.level {
            self.level = strength;
            let amount = f32::from(strength) / 100.0;
            self.low_mid
                .peaking(self.sample_rate, 320.0, 0.85, -3.5 * amount);
            self.presence
                .peaking(self.sample_rate, 3_000.0, 0.85, 6.0 * amount);
            self.headroom = 10.0_f32.powf(-0.9 * amount / 20.0);
            if strength == 0 {
                self.reset();
            }
        }
        for sample in samples {
            let x = if sample.is_finite() {
                sample.clamp(-1.0, 1.0)
            } else {
                0.0
            };
            if strength == 0 {
                *sample = x;
                continue;
            }
            let eq = self.presence.process(self.low_mid.process(x));
            *sample = (eq * self.headroom).clamp(-1.0, 1.0);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn energy(hz: f32, clarity: u8) -> f32 {
        let mut eq = VoiceClarity::new(48_000);
        let mut power = 0.0;
        for frame in 0..100 {
            let mut samples: [f32; 480] = std::array::from_fn(|i| {
                let t = (frame * 480 + i) as f32 / 48_000.0;
                0.1 * (2.0 * PI * hz * t).sin()
            });
            eq.process_in_place(&mut samples, clarity);
            if frame > 15 {
                power += samples.iter().map(|x| x * x).sum::<f32>();
            }
        }
        power
    }
    #[test]
    fn presence_is_measurable_at_the_default_setting() {
        let high = energy(3_000.0, 55) / energy(3_000.0, 0);
        let low = energy(320.0, 55) / energy(320.0, 0);
        assert!(high > 1.7, "3 kHz presence should be audible: {high}");
        assert!(low < 0.82, "low-mid reduction should be audible: {low}");
    }
    #[test]
    fn response_is_bounded_at_maximum() {
        let high = energy(3_000.0, 100) / energy(3_000.0, 0);
        assert!(
            high > 2.8 && high < 4.5,
            "EQ response unexpectedly weak or excessive: {high}"
        );
    }
    #[test]
    fn zero_and_bad_samples_do_not_corrupt_audio() {
        let mut eq = VoiceClarity::new(48_000);
        let mut input = [0.1; 480];
        eq.process_in_place(&mut input, 0);
        assert!(input.iter().all(|x| (*x - 0.1).abs() < 1e-6));
        let mut bad = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.4];
        eq.process_in_place(&mut bad, 100);
        assert!(bad.iter().all(|x| x.is_finite() && x.abs() <= 1.0));
        eq.process_in_place(&mut bad, 0);
        assert!(bad.iter().all(|x| x.is_finite()));
    }
}

//! Allocation-free linear interpolation for a live mono preview.
//! Quality is intentionally conservative; production routing needs listening tests.

pub struct LinearResampler {
    phase: f64,
    previous: f32,
    next: f32,
    primed: bool,
    nominal_step: f64,
}

impl LinearResampler {
    pub fn new(input_rate: u32, output_rate: u32) -> Result<Self, &'static str> {
        if !(8_000..=192_000).contains(&input_rate) || !(8_000..=192_000).contains(&output_rate) {
            return Err("sample rates must be between 8000 and 192000 Hz");
        }
        Ok(Self {
            phase: 0.0,
            previous: 0.0,
            next: 0.0,
            primed: false,
            nominal_step: input_rate as f64 / output_rate as f64,
        })
    }

    /// Returns one output sample and the count of missing source samples.
    /// `rate_adjustment` is a small clock-drift correction around 1.0.
    #[inline]
    pub fn next(
        &mut self,
        mut source: impl FnMut() -> Option<f32>,
        rate_adjustment: f64,
    ) -> (f32, u32) {
        let mut missing = 0;
        if !self.primed {
            let Some(first) = source() else {
                return (0.0, 1);
            };
            self.previous = first;
            self.next = source().unwrap_or_else(|| {
                missing += 1;
                0.0
            });
            self.primed = true;
        }
        let output = self.previous + (self.next - self.previous) * self.phase as f32;
        let adjustment = if rate_adjustment.is_finite() {
            rate_adjustment.clamp(0.995, 1.005)
        } else {
            1.0
        };
        self.phase += self.nominal_step * adjustment;
        while self.phase >= 1.0 {
            self.phase -= 1.0;
            self.previous = self.next;
            self.next = source().unwrap_or_else(|| {
                missing += 1;
                0.0
            });
        }
        (output, missing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[test]
    fn interpolates_upsampling() {
        let mut resampler = LinearResampler::new(24_000, 48_000).unwrap();
        let mut input = VecDeque::from(vec![0.0, 1.0, 0.0, -1.0, 0.0]);
        let output: Vec<f32> = (0..6)
            .map(|_| resampler.next(|| input.pop_front(), 1.0).0)
            .collect();
        assert_eq!(output, [0.0, 0.5, 1.0, 0.5, 0.0, -0.5]);
    }

    #[test]
    fn downsampling_consumes_source_at_expected_rate() {
        let mut resampler = LinearResampler::new(48_000, 24_000).unwrap();
        let mut input = (0..100).map(|x| x as f32);
        let output: Vec<f32> = (0..4)
            .map(|_| resampler.next(|| input.next(), 1.0).0)
            .collect();
        assert_eq!(output, [0.0, 2.0, 4.0, 6.0]);
    }

    #[test]
    fn missing_data_is_silence_and_recovers() {
        let mut resampler = LinearResampler::new(48_000, 48_000).unwrap();
        assert_eq!(resampler.next(|| None, 1.0), (0.0, 1));
        let mut input = [0.2, 0.4, 0.6, 0.8].into_iter();
        let first = resampler.next(|| input.next(), 1.0);
        let second = resampler.next(|| input.next(), 1.0);
        assert_eq!(first.0, 0.2);
        assert_eq!(second.0, 0.4);
    }

    #[test]
    fn preserves_audible_frequency_across_48k_to_44k() {
        let mut resampler = LinearResampler::new(48_000, 44_100).unwrap();
        let mut input =
            (0..48_100).map(|n| (2.0 * std::f32::consts::PI * 1000.0 * n as f32 / 48_000.0).sin());
        let output: Vec<f32> = (0..44_100)
            .map(|_| resampler.next(|| input.next(), 1.0).0)
            .collect();
        let crossings = output
            .windows(2)
            .filter(|pair| pair[0] <= 0.0 && pair[1] > 0.0)
            .count();
        assert!((999..=1001).contains(&crossings));
        assert!(
            output
                .iter()
                .all(|sample| sample.is_finite() && sample.abs() <= 1.0)
        );
    }

    #[test]
    fn invalid_clock_correction_uses_nominal_rate() {
        let mut resampler = LinearResampler::new(48_000, 24_000).unwrap();
        let mut input = (0..10).map(|x| x as f32);
        assert_eq!(resampler.next(|| input.next(), f64::NAN).0, 0.0);
        assert_eq!(resampler.next(|| input.next(), f64::NAN).0, 2.0);
    }
}

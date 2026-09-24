//! Allocation-free, in-place stereo DSP primitives for the AsySounds audio callback.
//! Control-plane updates should be applied between buffers, not inside the sample loop.
pub mod monitor;
pub mod neural_monitor;
pub mod noise;
pub mod resample;
pub mod voice;
#[derive(Clone, Copy, Debug)]
pub struct Gain {
    current: f32,
    target: f32,
    step: f32,
    remaining: usize,
}
impl Gain {
    pub fn new(linear: f32) -> Self {
        let g = if linear.is_finite() {
            linear.clamp(0.0, 4.0)
        } else {
            1.0
        };
        Self {
            current: g,
            target: g,
            step: 0.0,
            remaining: 0,
        }
    }
    pub fn set_target(&mut self, linear: f32, frames: usize) {
        self.target = if linear.is_finite() {
            linear.clamp(0.0, 4.0)
        } else {
            self.current
        };
        self.remaining = frames;
        self.step = if frames == 0 {
            self.current = self.target;
            0.0
        } else {
            (self.target - self.current) / frames as f32
        };
    }
    #[inline]
    pub fn next_gain(&mut self) -> f32 {
        if self.remaining > 0 {
            self.current += self.step;
            self.remaining -= 1;
            if self.remaining == 0 {
                self.current = self.target;
            }
        }
        self.current
    }
    pub fn value(&self) -> f32 {
        self.current
    }
}
#[derive(Clone, Copy, Debug)]
pub struct StereoStrip {
    pub gain: Gain,
    pub muted: bool,
}
impl StereoStrip {
    pub fn new() -> Self {
        Self {
            gain: Gain::new(1.0),
            muted: false,
        }
    }
    /// Adds interleaved stereo input to output; lengths must match and be even.
    /// Input and output must not alias.
    pub fn mix_into(&mut self, input: &[f32], output: &mut [f32]) {
        assert_eq!(input.len(), output.len());
        assert_eq!(input.len() % 2, 0);
        for frame in 0..input.len() / 2 {
            let g = self.gain.next_gain();
            if !self.muted {
                output[2 * frame] += input[2 * frame] * g;
                output[2 * frame + 1] += input[2 * frame + 1] * g;
            }
        }
    }
}
impl Default for StereoStrip {
    fn default() -> Self {
        Self::new()
    }
}
/// Peak limiter at output; prevents floating-point excursions outside [-1,1].
pub fn hard_limit(buffer: &mut [f32]) {
    for s in buffer {
        *s = if s.is_finite() {
            s.clamp(-1.0, 1.0)
        } else {
            0.0
        };
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stereo_mix() {
        let mut strip = StereoStrip::new();
        let mut out = [0.0; 4];
        strip.mix_into(&[0.25, -0.25, 0.5, -0.5], &mut out);
        assert_eq!(out, [0.25, -0.25, 0.5, -0.5]);
    }
    #[test]
    fn smooth_gain() {
        let mut g = Gain::new(0.0);
        g.set_target(1.0, 4);
        assert!((0..4).map(|_| g.next_gain()).last().unwrap() == 1.0);
        assert_eq!(g.value(), 1.0);
    }
    #[test]
    fn mute_advances_ramp() {
        let mut s = StereoStrip::new();
        s.muted = true;
        s.gain.set_target(0.0, 2);
        let mut out = [0.0; 4];
        s.mix_into(&[1.0; 4], &mut out);
        assert_eq!(out, [0.0; 4]);
        assert_eq!(s.gain.value(), 0.0);
    }
    #[test]
    fn limit_nonfinite() {
        let mut b = [2.0, -3.0, f32::NAN, f32::INFINITY];
        hard_limit(&mut b);
        assert_eq!(b, [1.0, -1.0, 0.0, 0.0]);
    }
    #[test]
    fn gain_rejects_nonfinite_controls() {
        let mut gain = Gain::new(f32::NAN);
        assert_eq!(gain.value(), 1.0);
        gain.set_target(f32::INFINITY, 10);
        for _ in 0..10 {
            assert!(gain.next_gain().is_finite());
        }
        assert_eq!(gain.value(), 1.0);
    }
}

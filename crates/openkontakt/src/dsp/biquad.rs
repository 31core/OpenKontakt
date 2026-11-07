//! Stereo-capable direct-form-1 biquad filter.
//!
//! The public constructors use RBJ cookbook coefficients. A running filter can be retuned with
//! [`Biquad::set_cutoff`] and [`Biquad::set_resonance`]: both only recompute five coefficients and
//! preserve the per-channel delay state, so they are safe to call from the audio thread and do
//! not allocate.
//!
//! The direct-form-1 difference equation used by [`Biquad::process`] is:
//!
//! ```text
//! y[n] = b0*x[n] + b1*x[n-1] + b2*x[n-2] - a1*y[n-1] - a2*y[n-2]
//! ```
//!
//! with `a0` normalised to 1.
#![allow(dead_code)]

/// Maximum number of independent channels handled by one [`Biquad`].
pub const MAX_CHANNELS: usize = 2;

const MIN_CUTOFF_HZ: f32 = 20.0;
const NYQUIST_RATIO: f32 = 0.45;
const MIN_Q: f32 = 0.5;
const MAX_Q: f32 = 20.0;
const DEFAULT_SAMPLE_RATE: f32 = 48_000.0;

/// The response shape produced by a [`Biquad`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BiquadKind {
    /// Second-order resonant low-pass.
    LowPass,
    /// Second-order resonant high-pass.
    HighPass,
    /// Constant-peak-gain band-pass.
    BandPass,
    /// Peaking EQ with a gain in decibels.
    Peak,
}

#[derive(Clone, Copy, Debug, Default)]
struct ChannelState {
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

/// A direct-form-1 biquad with independent state for up to [`MAX_CHANNELS`] channels.
///
/// Coefficients are shared across channels; only the delay state is per channel. The filter is
/// real-time safe: [`process`](Biquad::process) performs no allocation and no locking.
#[derive(Clone, Debug)]
pub struct Biquad {
    kind: BiquadKind,
    sample_rate: f32,
    cutoff_hz: f32,
    resonance: f32,
    gain_db: f32,
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    state: [ChannelState; MAX_CHANNELS],
}

impl Biquad {
    /// Creates a resonant low-pass filter.
    ///
    /// `resonance` is clamped to `0.0..=1.0` and mapped exponentially to a Q between 0.5 and 20.
    /// `cutoff_hz` is clamped to `[20 Hz, 0.45 * sample_rate]`.
    pub fn low_pass(sample_rate: f32, cutoff_hz: f32, resonance: f32) -> Self {
        Self::new(BiquadKind::LowPass, sample_rate, cutoff_hz, resonance, 0.0)
    }

    /// Creates a resonant high-pass filter.
    ///
    /// The clamping and resonance mapping are identical to [`Biquad::low_pass`].
    pub fn high_pass(sample_rate: f32, cutoff_hz: f32, resonance: f32) -> Self {
        Self::new(BiquadKind::HighPass, sample_rate, cutoff_hz, resonance, 0.0)
    }

    /// Creates a constant-peak-gain band-pass filter.
    pub fn band_pass(sample_rate: f32, cutoff_hz: f32, resonance: f32) -> Self {
        Self::new(BiquadKind::BandPass, sample_rate, cutoff_hz, resonance, 0.0)
    }

    /// Creates a peaking EQ with `gain_db` decibels of boost or cut at `cutoff_hz`.
    pub fn peak(sample_rate: f32, cutoff_hz: f32, resonance: f32, gain_db: f32) -> Self {
        Self::new(BiquadKind::Peak, sample_rate, cutoff_hz, resonance, gain_db)
    }

    fn new(
        kind: BiquadKind,
        sample_rate: f32,
        cutoff_hz: f32,
        resonance: f32,
        gain_db: f32,
    ) -> Self {
        let mut filter = Self {
            kind,
            sample_rate,
            cutoff_hz,
            resonance,
            gain_db,
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
            state: [ChannelState::default(); MAX_CHANNELS],
        };
        filter.update_coefficients();
        filter
    }

    /// Retunes the cutoff frequency in Hz without resetting the filter state.
    ///
    /// The value is clamped to `[20 Hz, 0.45 * sample_rate]`.
    pub fn set_cutoff(&mut self, cutoff_hz: f32) {
        self.cutoff_hz = cutoff_hz;
        self.update_coefficients();
    }

    /// Changes the resonance and recomputes coefficients without resetting the filter state.
    ///
    /// `resonance` is clamped to `0.0..=1.0`; 0 maps to Q = 0.5 and 1 maps to Q = 20.
    pub fn set_resonance(&mut self, resonance: f32) {
        self.resonance = resonance;
        self.update_coefficients();
    }

    /// Changes the peak gain in decibels (only meaningful for [`BiquadKind::Peak`]).
    pub fn set_gain_db(&mut self, gain_db: f32) {
        self.gain_db = gain_db;
        self.update_coefficients();
    }

    /// Processes one sample on `channel`.
    ///
    /// Channels at or above [`MAX_CHANNELS`] are passed through unchanged rather than panicking.
    #[inline]
    pub fn process(&mut self, channel: usize, x: f32) -> f32 {
        if channel >= MAX_CHANNELS {
            return x;
        }
        let state = &mut self.state[channel];
        let y = self.b0 * x + self.b1 * state.x1 + self.b2 * state.x2
            - self.a1 * state.y1
            - self.a2 * state.y2;
        state.x2 = state.x1;
        state.x1 = x;
        state.y2 = state.y1;
        state.y1 = y;
        y
    }

    /// Clears the delay state of every channel, leaving the coefficients untouched.
    pub fn reset(&mut self) {
        self.state = [ChannelState::default(); MAX_CHANNELS];
    }

    fn update_coefficients(&mut self) {
        if !self.sample_rate.is_finite() || self.sample_rate <= 0.0 {
            self.sample_rate = DEFAULT_SAMPLE_RATE;
        }
        let max_cutoff = (self.sample_rate * NYQUIST_RATIO).max(MIN_CUTOFF_HZ);
        if !self.cutoff_hz.is_finite() {
            self.cutoff_hz = MIN_CUTOFF_HZ;
        }
        self.cutoff_hz = self.cutoff_hz.clamp(MIN_CUTOFF_HZ, max_cutoff);
        if !self.resonance.is_finite() {
            self.resonance = 0.0;
        }
        self.resonance = self.resonance.clamp(0.0, 1.0);
        if !self.gain_db.is_finite() {
            self.gain_db = 0.0;
        }

        let sample_rate = self.sample_rate as f64;
        let cutoff = self.cutoff_hz as f64;
        let q = MIN_Q as f64 * ((MAX_Q / MIN_Q) as f64).powf(self.resonance as f64);
        let w0 = 2.0 * std::f64::consts::PI * cutoff / sample_rate;
        let cos_w0 = w0.cos();
        let sin_w0 = w0.sin();
        let alpha = sin_w0 / (2.0 * q);

        let (b0, b1, b2, a0, a1, a2) = match self.kind {
            BiquadKind::LowPass => {
                let one_minus_cos = 1.0 - cos_w0;
                (
                    one_minus_cos * 0.5,
                    one_minus_cos,
                    one_minus_cos * 0.5,
                    1.0 + alpha,
                    -2.0 * cos_w0,
                    1.0 - alpha,
                )
            }
            BiquadKind::HighPass => {
                let one_plus_cos = 1.0 + cos_w0;
                (
                    one_plus_cos * 0.5,
                    -one_plus_cos,
                    one_plus_cos * 0.5,
                    1.0 + alpha,
                    -2.0 * cos_w0,
                    1.0 - alpha,
                )
            }
            BiquadKind::BandPass => (alpha, 0.0, -alpha, 1.0 + alpha, -2.0 * cos_w0, 1.0 - alpha),
            BiquadKind::Peak => {
                let a = 10.0f64.powf(self.gain_db as f64 / 40.0);
                (
                    1.0 + alpha * a,
                    -2.0 * cos_w0,
                    1.0 - alpha * a,
                    1.0 + alpha / a,
                    -2.0 * cos_w0,
                    1.0 - alpha / a,
                )
            }
        };

        self.b0 = (b0 / a0) as f32;
        self.b1 = (b1 / a0) as f32;
        self.b2 = (b2 / a0) as f32;
        self.a1 = (a1 / a0) as f32;
        self.a2 = (a2 / a0) as f32;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feeds a sine through the filter and returns the settled gain in decibels.
    fn sine_gain_db(filter: &mut Biquad, frequency_hz: f32, sample_rate: f32) -> f32 {
        let total_samples = 48_000usize;
        let settle_samples = 12_000usize;
        let mut input_energy = 0.0f64;
        let mut output_energy = 0.0f64;
        let mut counted = 0usize;

        for index in 0..total_samples {
            let x = (std::f32::consts::TAU * frequency_hz * index as f32 / sample_rate).sin();
            let y = filter.process(0, x);
            if index >= settle_samples {
                input_energy += (x as f64) * (x as f64);
                output_energy += (y as f64) * (y as f64);
                counted += 1;
            }
        }

        let input_rms = (input_energy / counted as f64).sqrt();
        let output_rms = (output_energy / counted as f64).sqrt();
        (20.0 * (output_rms / input_rms).log10()) as f32
    }

    #[test]
    fn low_pass_dc_converges_to_unity() {
        let mut filter = Biquad::low_pass(48_000.0, 1_000.0, 0.0);
        let mut y = 0.0;
        for _ in 0..48_000 {
            y = filter.process(0, 1.0);
        }
        assert!(
            (y - 1.0).abs() < 1e-3,
            "low-pass DC should settle to 1.0, got {y}"
        );
    }

    #[test]
    fn low_pass_attenuates_1khz_with_200hz_cutoff() {
        let mut filter = Biquad::low_pass(48_000.0, 200.0, 0.0);
        let gain_db = sine_gain_db(&mut filter, 1_000.0, 48_000.0);
        assert!(
            gain_db < -20.0,
            "1 kHz should be attenuated by more than 20 dB, got {gain_db:.2} dB"
        );
    }

    #[test]
    fn high_pass_attenuates_50hz_more_than_5khz() {
        let mut low = Biquad::high_pass(48_000.0, 200.0, 0.0);
        let low_gain_db = sine_gain_db(&mut low, 50.0, 48_000.0);
        let mut high = Biquad::high_pass(48_000.0, 200.0, 0.0);
        let high_gain_db = sine_gain_db(&mut high, 5_000.0, 48_000.0);
        assert!(
            low_gain_db + 20.0 < high_gain_db,
            "50 Hz ({low_gain_db:.2} dB) should be at least 20 dB below 5 kHz ({high_gain_db:.2} dB)"
        );
    }

    #[test]
    fn channels_have_independent_state_and_reset_clears_them() {
        let mut filter = Biquad::low_pass(48_000.0, 1_000.0, 0.0);
        let left = filter.process(0, 1.0);
        let right = filter.process(1, 1.0);
        assert!((left - right).abs() < 1e-7);

        filter.reset();
        let left_after_reset = filter.process(0, 1.0);
        assert!((left_after_reset - left).abs() < 1e-7);

        // Out-of-range channels are passed through rather than panicking.
        assert_eq!(filter.process(2, 0.25), 0.25);
    }

    #[test]
    fn retuning_changes_the_response_without_resetting_state() {
        let mut filter = Biquad::low_pass(48_000.0, 1_000.0, 0.0);
        let before = sine_gain_db(&mut filter, 5_000.0, 48_000.0);

        filter.set_cutoff(10_000.0);
        filter.set_resonance(1.0);
        let after = sine_gain_db(&mut filter, 5_000.0, 48_000.0);

        assert!(
            after > before + 10.0,
            "moving the cutoff above 5 kHz should raise its level: {before:.2} dB -> {after:.2} dB"
        );
    }
}

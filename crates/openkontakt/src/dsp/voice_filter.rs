//! The per-voice low-pass a script drives with a MIDI controller.
//!
//! A group's `filterCutoff` has an external modulator whose source is a MIDI controller — on the
//! library this was measured from, controller 76, the one its "FILTER LFO" panel sweeps. Nothing
//! in the file says what a controller value means in hertz, so the curve here is fitted to the
//! original player's output:
//!
//! * one take with the LFO running (the panel's stored table is a V from 127 down to 4 and back),
//! * the same take with it off,
//! * the ratio of their spectra, fitted with a second-order low-pass at each moment.
//!
//! The ratio is a 12 dB/octave slope, i.e. one second-order stage, and its cutoff follows
//! `log2(fc) = 7.73 + 0.0405 * cc`. A fourth-order fit is 2 dB worse everywhere and needs a cutoff
//! two to twelve times higher to explain the same slope.
//!
//! The absolute numbers are provisional. The take they are fitted from is nearly all release tail
//! where the filter moves (its level there is −60…−75 dBFS), so the shape is better supported than
//! the endpoints: a sustained-note reference would move the constants, and probably the intercept
//! by an octave or so. Another library or another controller may sit on a different segment of the
//! same curve.

/// `log2` of the cutoff at controller value 0.
const CUTOFF_LOG2_AT_ZERO: f32 = 7.73;
/// Octaves the cutoff rises per controller unit.
const OCTAVES_PER_UNIT: f32 = 0.0405;

pub const MIN_CUTOFF_HZ: f32 = 20.0;
pub const MAX_CUTOFF_HZ: f32 = 20_000.0;

/// Cutoff in hertz for a controller value 0…127.
pub fn cutoff_hz(controller: i64) -> f32 {
    let controller = controller.clamp(0, 127) as f32;
    let log2 = CUTOFF_LOG2_AT_ZERO + OCTAVES_PER_UNIT * controller;
    2f32.powf(log2).clamp(MIN_CUTOFF_HZ, MAX_CUTOFF_HZ)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three points the fit was checked against: the bottom of the panel's stored table, the
    /// middle, and the top.
    #[test]
    fn the_fitted_curve_matches_the_measured_cutoffs() {
        assert!((cutoff_hz(4) - 240.0).abs() < 40.0, "{}", cutoff_hz(4));
        assert!((cutoff_hz(64) - 1280.0).abs() < 130.0, "{}", cutoff_hz(64));
        assert!(
            (cutoff_hz(127) - 7500.0).abs() < 800.0,
            "{}",
            cutoff_hz(127)
        );
    }

    #[test]
    fn out_of_range_controllers_are_clamped() {
        assert_eq!(cutoff_hz(-10), cutoff_hz(0));
        assert_eq!(cutoff_hz(200), cutoff_hz(127));
    }
}

use nice_plug::prelude::*;
use std::sync::Arc;
use std::sync::Mutex;

#[derive(Params)]
pub struct OpenKontaktParams {
    /// Instrument level, edited in decibels and shown in the instrument bar.
    #[id = "volume"]
    pub volume: FloatParam,

    /// Master output level (the Global Vol knob), in decibels.
    #[id = "master_volume"]
    pub master_volume: FloatParam,

    /// Master pan, -1 (left) to +1 (right).
    #[id = "pan"]
    pub pan: FloatParam,

    /// Global concert pitch, stored as a cent offset from A440 (so 440 Hz sits at the knob's
    /// centre). The instrument's own Program tune ratio is applied on top.
    #[id = "tune"]
    pub tune: FloatParam,

    /// Last loaded instrument, stored in host sessions.
    #[persist = "instrument_path"]
    pub instrument_path: Mutex<String>,

    /// Cached display name, so a restored session can show the instrument before it is reloaded.
    #[persist = "instrument_name"]
    pub instrument_name: Mutex<String>,

    /// Panel control values, as `<instrument path>|<slot>:<name>=<value>;…`.
    ///
    /// Kontakt keeps the state of a library's own panel with the session, so a knob a user moved
    /// stays where they left it after a reload. Without this, every reload re-applies the values
    /// stored inside the `.nki` and the user's change silently disappears.
    #[persist = "panel_values"]
    pub panel_values: Mutex<String>,
}

impl Default for OpenKontaktParams {
    fn default() -> Self {
        Self {
            volume: FloatParam::new(
                "Volume",
                0.0,
                FloatRange::Linear {
                    min: -100.0,
                    max: 12.0,
                },
            )
            .with_smoother(SmoothingStyle::Linear(20.0))
            .with_step_size(0.1)
            .with_value_to_string(Arc::new(|value: f32| {
                if value <= -99.5 {
                    String::from("-inf")
                } else {
                    format!("{value:.1}")
                }
            }))
            .with_unit(" dB"),
            master_volume: FloatParam::new(
                "Global Vol",
                0.0,
                FloatRange::Linear {
                    min: -36.0,
                    max: 36.0,
                },
            )
            .with_smoother(SmoothingStyle::Linear(20.0))
            .with_step_size(0.1)
            .with_value_to_string(Arc::new(|value: f32| format!("{value:.1}")))
            .with_unit(" dB"),
            pan: FloatParam::new(
                "Pan",
                0.0,
                FloatRange::Linear {
                    min: -1.0,
                    max: 1.0,
                },
            )
            .with_smoother(SmoothingStyle::Linear(20.0))
            .with_step_size(0.01)
            .with_value_to_string(Arc::new(|value: f32| {
                let percent = (value.clamp(-1.0, 1.0) * 100.0).round() as i32;
                if percent == 0 {
                    String::from("Center")
                } else if percent < 0 {
                    format!("{}L", -percent)
                } else {
                    format!("{percent}R")
                }
            })),
            tune: FloatParam::new(
                "Global Tune",
                0.0,
                FloatRange::Linear {
                    min: -200.0,
                    max: 200.0,
                },
            )
            .with_smoother(SmoothingStyle::Linear(20.0))
            // Hertz, like the original: the knob is calibrated as a reference pitch (A4), so it
            // reads 440.00 Hz at no transposition.
            .with_value_to_string(Arc::new(|cents: f32| {
                format!("{:.2}", 440.0 * 2f32.powf(cents / 1200.0))
            }))
            .with_unit(" Hz"),
            instrument_path: Mutex::new(String::new()),
            instrument_name: Mutex::new(String::new()),
            panel_values: Mutex::new(String::new()),
        }
    }
}

impl OpenKontaktParams {
    pub fn path(&self) -> String {
        self.instrument_path.lock().unwrap().clone()
    }

    pub fn set_instrument(&self, path: &str, name: &str) {
        *self.instrument_path.lock().unwrap() = path.to_string();
        *self.instrument_name.lock().unwrap() = name.to_string();
    }

    pub fn name(&self) -> String {
        self.instrument_name.lock().unwrap().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tune_displays_the_reference_pitch_in_hertz() {
        let params = OpenKontaktParams::default();
        // 0 cents is A440; the knob spans a whole tone either way, like the original's.
        assert_eq!(
            params.tune.normalized_value_to_string(0.0, true),
            "392.00 Hz"
        );
        assert_eq!(
            params.tune.normalized_value_to_string(0.5, true),
            "440.00 Hz"
        );
        assert_eq!(
            params.tune.normalized_value_to_string(1.0, true),
            "493.88 Hz"
        );
    }

    #[test]
    fn pan_displays_left_center_right() {
        let params = OpenKontaktParams::default();
        assert_eq!(params.pan.normalized_value_to_string(0.0, false), "100L");
        assert_eq!(params.pan.normalized_value_to_string(0.5, false), "Center");
        assert_eq!(params.pan.normalized_value_to_string(1.0, false), "100R");
        assert_eq!(params.pan.normalized_value_to_string(0.25, false), "50L");
        assert_eq!(params.pan.normalized_value_to_string(0.75, false), "50R");
    }

    #[test]
    fn master_volume_spans_minus_thirty_six_to_thirty_six_db() {
        let params = OpenKontaktParams::default();
        assert_eq!(
            params.master_volume.normalized_value_to_string(0.0, true),
            "-36.0 dB"
        );
        assert_eq!(
            params.master_volume.normalized_value_to_string(0.5, true),
            "0.0 dB"
        );
        assert_eq!(
            params.master_volume.normalized_value_to_string(1.0, true),
            "36.0 dB"
        );
    }

    #[test]
    fn volume_displays_minus_infinity_to_twelve_db() {
        let params = OpenKontaktParams::default();
        assert_eq!(
            params.volume.normalized_value_to_string(0.0, true),
            "-inf dB"
        );
        assert_eq!(
            params.volume.normalized_value_to_string(1.0, true),
            "12.0 dB"
        );
    }
}

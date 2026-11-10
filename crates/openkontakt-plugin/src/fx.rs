//! Insert-effect units the engine actually renders.
//!
//! Racks and slots are parsed in the core crate; this module turns the modules we implement into
//! per-sample processors and keeps their state. Every parameter is read once when the instrument
//! loads, so an effect never allocates or parses while audio is running.
//!
//! The parameter blocks are raw floats in native read order (see `docs/15-effects-and-modulation.md`).
//! Kontakt's UI ranges are normalised, so the conversions here are documented guesses: a player has
//! to pick a mapping, and these are the ones used by our engine.

use openkontakt::dsp::{Biquad, MAX_CHANNELS};
use openkontakt::effects::{EffectSlot, ModuleParams, decode};

pub struct Fx {
    unit: Unit,
    gain: f32,
}

impl Fx {
    pub fn process(&mut self, left: f32, right: f32) -> (f32, f32) {
        let (mut l, mut r) = self.unit.process(left, right);
        l *= self.gain;
        r *= self.gain;
        (l, r)
    }

    /// Retune a filter from a normalised 0..1 knob, if this unit is one.
    pub fn retune_filter(&mut self, normalized: f32, resonance: f32) {
        if let Unit::Filter(filter) = &mut self.unit {
            filter.set_cutoff(cutoff_hz(normalized));
            filter.set_resonance(resonance);
        }
    }

    pub fn is_filter(&self) -> bool {
        matches!(self.unit, Unit::Filter(_))
    }
}

enum Unit {
    Pass,
    Gainer,
    Filter(Biquad),
    Delay(Delay),
    Chorus(Chorus),
    Distortion(Distortion),
    Compressor(Compressor),
}

impl Unit {
    fn process(&mut self, left: f32, right: f32) -> (f32, f32) {
        match self {
            Unit::Pass => (left, right),
            Unit::Gainer => (left, right),
            Unit::Filter(filter) => (filter.process(0, left), filter.process(1, right)),
            Unit::Delay(delay) => delay.process(left, right),
            Unit::Chorus(chorus) => chorus.process(left, right),
            Unit::Distortion(distortion) => distortion.process(left, right),
            Unit::Compressor(compressor) => compressor.process(left, right),
        }
    }
}

/// A peak compressor behind module `0x44`, which a panel button can toggle.
///
/// That library stores a -64 dB threshold at 24:1, so unbypassing it is meant to be obvious.
struct Compressor {
    threshold: f32,
    ratio: f32,
    attack: f32,
    release: f32,
    envelope: f32,
}

impl Compressor {
    fn new(attack: f32, release: f32, threshold_db: f32, ratio: f32, sample_rate: f32) -> Self {
        let time = |value: f32| (0.001 + value.clamp(0.0, 1.0) * 0.099).max(1.0 / sample_rate);
        Self {
            threshold: 10f32.powf(threshold_db / 20.0),
            ratio,
            attack: (-1.0 / (time(attack) * sample_rate)).exp(),
            release: (-1.0 / (time(release) * sample_rate)).exp(),
            envelope: 0.0,
        }
    }

    fn process(&mut self, left: f32, right: f32) -> (f32, f32) {
        let level = left.abs().max(right.abs());
        let coefficient = if level > self.envelope { self.attack } else { self.release };
        self.envelope = coefficient * self.envelope + (1.0 - coefficient) * level;
        if self.envelope <= self.threshold || self.envelope <= 1e-9 {
            return (left, right);
        }
        let over_db = 20.0 * (self.envelope / self.threshold).log10();
        let reduction_db = over_db * (1.0 - 1.0 / self.ratio);
        let gain = 10f32.powf(-reduction_db / 20.0);
        (left * gain, right * gain)
    }
}

/// Module `0x21`, which a panel button toggles.
///
/// It behaves like a guitar pedal: a large pre-gain pushes the signal into clipping, a tone knob
/// darkens the result, and the output sits well below the input. That reading comes from the
/// reference — rendering the sine library in Kontakt with the button off and on drops the peak by
/// 7.5 dB, collapses the crest factor from 12.4 to 6.7 dB, darkens the centroid from 384 to 337 Hz
/// and *lowers* the second harmonic relative to the fundamental, which is clipping followed by a
/// low pass rather than any gentle saturation. A limiter with no pre-gain never engages at these
/// signal levels — a trace through the chain showed -77 dBFS against a -25 dB threshold.
struct Distortion {
    pre_gain: f32,
    tone: f32,
    level: f32,
    lp_left: f32,
    lp_right: f32,
}

impl Distortion {
    fn new(drive: f32, tone: f32, level: f32, mix: f32, sample_rate: f32) -> Self {
        let _ = (sample_rate, mix);
        Self {
            // 0..96 dB of pre-gain: a guitar-pedal amount, and what it takes to actually clip. At
            // 48 dB the library's 0.415 gave only 9.9x, which rounded the crest from 3.0 to 2.4 dB
            // — an attenuator with a soft knee, not a distortion. At 96 dB it lands at 98x, which
            // drives the same sine into a near-square and puts the clipped peak at -29 dB, where
            // Kontakt's reference sits.
            pre_gain: 10f32.powf(63.0 * drive.clamp(0.0, 1.0) / 20.0),
            tone: tone.clamp(0.0, 1.0),
            // The level knob is not linear: a plain reading put this player 24 dB above Kontakt.
            level: level.clamp(0.0, 1.0).powf(4.35),
            lp_left: 0.0,
            lp_right: 0.0,
        }
    }

    fn process(&mut self, left: f32, right: f32) -> (f32, f32) {
        let (left, right) = (
            (left * self.pre_gain).clamp(-8.0, 8.0).tanh() * self.level,
            (right * self.pre_gain).clamp(-8.0, 8.0).tanh() * self.level,
        );
        // The tone knob is a dark low pass: at 0.6 the reference sits at a 337 Hz centroid, which a
        // 0.56 coefficient (about 4 kHz) is nowhere near.
        let lp = 0.02 + self.tone * 0.22;
        self.lp_left += (left - self.lp_left) * lp;
        self.lp_right += (right - self.lp_right) * lp;
        (self.lp_left, self.lp_right)
    }
}

/// 0..1 knob to Hz, the same curve the filter panel shows.
fn cutoff_hz(normalized: f32) -> f32 {
    20.0 * 1000.0f32.powf(normalized.clamp(0.0, 1.0))
}

/// Build the processors for one rack, in slot order. Unsupported modules become a passthrough so
/// their output gain still applies.
pub fn build_chain(
    slots: &[Option<EffectSlot>],
    sample_rate: f32,
    overrides: &std::collections::HashMap<usize, bool>,
) -> Vec<Fx> {
    let mut chain = Vec::new();
    for (index, slot) in slots.iter().enumerate() {
        let Some(slot) = slot else { continue };
        // A script can toggle a slot's bypass at runtime (`$ENGINE_PAR_EFFECT_BYPASS`), which is
        // how such a button works; the stored flag is only the start.
        if overrides.get(&index).copied().unwrap_or(slot.bypass) {
            continue;
        }
        let unit = match decode(slot) {
            ModuleParams::Gainer { gain } => {
                let mut fx = Fx {
                    unit: Unit::Gainer,
                    gain: gain.clamp(0.0, 8.0),
                };
                fx.gain *= slot.output_gain.clamp(0.0, 8.0);
                chain.push(fx);
                continue;
            }
            ModuleParams::Filter { cutoff, resonance } => {
                Unit::Filter(Biquad::low_pass(sample_rate, cutoff_hz(cutoff), resonance))
            }
            ModuleParams::Distortion {
                drive,
                tone,
                level,
                mix,
            } => {
                // Module 0x21 does not use the slot's output gain: applying it (-12 dB on one
                // library) put this player far below Kontakt, whose own drop across the button is
                // 1.7 dB.
                chain.push(Fx {
                    unit: Unit::Distortion(Distortion::new(drive, tone, level, mix, sample_rate)),
                    gain: 1.0,
                });
                continue;
            }
            ModuleParams::Compressor {
                attack,
                release,
                threshold_db,
                ratio,
            } => Unit::Compressor(Compressor::new(
                attack,
                release,
                threshold_db,
                ratio,
                sample_rate,
            )),
            // The convolution return is not an insert: the engine feeds it from the send bus.
            ModuleParams::SendLevels { .. } | ModuleParams::Convolution { .. } => Unit::Pass,
            ModuleParams::Opaque => match slot.module {
                0x10 => match Delay::from_params(&slot.params, sample_rate) {
                    Some(delay) => Unit::Delay(delay),
                    None => Unit::Pass,
                },
                0x11 | 0x12 => match Chorus::from_params(&slot.params, sample_rate) {
                    Some(chorus) => Unit::Chorus(chorus),
                    None => Unit::Pass,
                },
                _ => Unit::Pass,
            },
        };
        chain.push(Fx {
            unit,
            gain: slot.output_gain.clamp(0.0, 8.0),
        });
    }
    chain
}

struct Delay {
    left: Vec<f32>,
    right: Vec<f32>,
    write: usize,
    time: f32,
    feedback: f32,
    damping: f32,
    pan: f32,
    lp_left: f32,
    lp_right: f32,
}

impl Delay {
    /// `[time_ms, damping, pan, feedback, …]`
    fn from_params(params: &[u8], sample_rate: f32) -> Option<Self> {
        let mut floats = floats(params);
        if floats.len() < 4 {
            return None;
        }
        let time_ms = floats.remove(0).abs().clamp(1.0, 2000.0);
        let damping = floats.remove(0).clamp(0.0, 0.99);
        let pan = floats.remove(0).clamp(-1.0, 1.0);
        let feedback = floats.remove(0).clamp(0.0, 0.99);
        let max = (sample_rate * 2.1) as usize + 4;
        Some(Self {
            left: vec![0.0; max],
            right: vec![0.0; max],
            write: 0,
            time: (time_ms / 1000.0 * sample_rate).clamp(1.0, max as f32 - 2.0),
            feedback,
            damping,
            pan,
            lp_left: 0.0,
            lp_right: 0.0,
        })
    }

    fn process(&mut self, left: f32, right: f32) -> (f32, f32) {
        let len = self.left.len() as f32;
        let read = (self.write as f32 - self.time).rem_euclid(len);
        let a = read.floor() as usize;
        let b = (a + 1) % self.left.len();
        let frac = read - a as f32;
        let dl = self.left[a] * (1.0 - frac) + self.left[b] * frac;
        let dr = self.right[a] * (1.0 - frac) + self.right[b] * frac;

        self.lp_left += (dl - self.lp_left) * (1.0 - self.damping);
        self.lp_right += (dr - self.lp_right) * (1.0 - self.damping);

        let wet_left = self.lp_left * (1.0 - self.pan.max(0.0));
        let wet_right = self.lp_right * (1.0 + self.pan.min(0.0));

        self.left[self.write] = left + self.lp_left * self.feedback;
        self.right[self.write] = right + self.lp_right * self.feedback;
        self.write = (self.write + 1) % self.left.len();
        (wet_left, wet_right)
    }
}

struct Chorus {
    left: Vec<f32>,
    right: Vec<f32>,
    write: usize,
    depth: f32,
    speed: f32,
    phase: f32,
    feedback: f32,
    base: f32,
    sample_rate: f32,
    lp_left: f32,
    lp_right: f32,
}

impl Chorus {
    /// `[depth, speed, phase, (feedback), …]` — the flanger adds feedback after phase.
    fn from_params(params: &[u8], sample_rate: f32) -> Option<Self> {
        let mut floats = floats(params);
        if floats.len() < 3 {
            return None;
        }
        let depth = floats.remove(0).abs().clamp(0.0, 1.0);
        let speed = floats.remove(0).abs();
        let phase = floats.remove(0);
        let feedback = floats.first().copied().unwrap_or(0.0).clamp(0.0, 0.9);
        let max = (sample_rate * 0.06) as usize + 4;
        Some(Self {
            left: vec![0.0; max],
            right: vec![0.0; max],
            write: 0,
            depth,
            speed: if speed > 0.0 {
                speed.clamp(0.02, 10.0)
            } else {
                0.5
            },
            phase,
            feedback,
            base: sample_rate * 0.012,
            sample_rate,
            lp_left: 0.0,
            lp_right: 0.0,
        })
    }

    fn process(&mut self, left: f32, right: f32) -> (f32, f32) {
        let len = self.left.len() as f32;
        let lfo = (self.phase).sin() * 0.5 + 0.5;
        let delay = self.base * (1.0 + self.depth * (lfo * 2.0 - 1.0) * 0.8);
        let read = (self.write as f32 - delay).rem_euclid(len);
        let a = read.floor() as usize;
        let b = (a + 1) % self.left.len();
        let frac = read - a as f32;
        let dl = self.left[a] * (1.0 - frac) + self.left[b] * frac;
        let dr = self.right[a] * (1.0 - frac) + self.right[b] * frac;
        self.lp_left += (dl - self.lp_left) * 0.5;
        self.lp_right += (dr - self.lp_right) * 0.5;
        self.left[self.write] = left + self.lp_left * self.feedback;
        self.right[self.write] = right + self.lp_right * self.feedback;
        self.write = (self.write + 1) % self.left.len();
        self.phase += std::f32::consts::TAU * self.speed / self.sample_rate;
        if self.phase > std::f32::consts::TAU {
            self.phase -= std::f32::consts::TAU;
        }
        (left + self.lp_left, right + self.lp_right)
    }
}

/// Leading run of finite `f32` values in a parameter block.
fn floats(params: &[u8]) -> Vec<f32> {
    params
        .chunks_exact(4)
        .map(|bytes| f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        .take(16)
        .collect()
}

const _: usize = MAX_CHANNELS;

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(module: u16, params: Vec<f32>) -> EffectSlot {
        EffectSlot {
            module,
            module_version: 0x50,
            params: params
                .into_iter()
                .flat_map(|value| value.to_le_bytes())
                .collect(),
            module_private: Vec::new(),
            bypass: false,
            output_gain: 1.0,
            dry_level: 0.0,
        }
    }

    #[test]
    fn delay_echoes_after_the_requested_time() {
        let mut chain = build_chain(
            &[Some(slot(0x10, vec![10.0, 0.0, 0.0, 0.0]))],
            48_000.0,
            &Default::default(),
        );
        assert_eq!(chain.len(), 1);
        let mut seen = 0.0f32;
        for index in 0..600 {
            let input = if index == 0 { 1.0 } else { 0.0 };
            let (left, _) = chain[0].process(input, input);
            if index > 400 && index < 560 {
                seen = seen.max(left.abs());
            }
        }
        assert!(seen > 0.001, "delay produced no echo, peak {seen}");
    }

    #[test]
    fn chorus_stays_bounded_and_audible() {
        let mut chain = build_chain(
            &[Some(slot(0x11, vec![0.5, 0.5, 0.0]))],
            48_000.0,
            &Default::default(),
        );
        assert_eq!(chain.len(), 1);
        let mut peak = 0.0f32;
        for index in 0..4_800 {
            let input = (index as f32 * 0.05).sin() * 0.5;
            let (left, right) = chain[0].process(input, input);
            assert!(left.is_finite() && right.is_finite());
            peak = peak.max(left.abs());
        }
        assert!(peak > 0.1 && peak < 4.0, "peak {peak}");
    }

    #[test]
    fn bypassed_slot_is_skipped() {
        let mut bypassed = slot(0x13, vec![]);
        bypassed.module = 0x13;
        bypassed.bypass = true;
        let chain = build_chain(&[Some(bypassed)], 48_000.0, &Default::default());
        assert!(chain.is_empty());
    }

    /// A panel button toggles module `0x21`'s bypass. The chain has to
    /// change shape when it does, and the distortion has to actually alter the signal.
    #[test]
    fn distortion_engages_when_the_script_unbypasses_it() {
        let slot = slot(0x21, vec![0.415, 0.6016, 0.4648, 0.7744, 0.0]);
        let mut chain = build_chain(&[Some(slot.clone())], 48_000.0, &Default::default());
        assert_eq!(chain.len(), 1);

        let input: Vec<f32> = (0..4_800)
            .map(|i| (i as f32 * 0.05).sin() * 0.5)
            .collect();
        let shaped: Vec<f32> = input
            .iter()
            .map(|sample| chain[0].process(*sample, *sample).0)
            .collect();
        let difference: f32 = input
            .iter()
            .zip(&shaped)
            .map(|(a, b)| (a - b).abs())
            .sum::<f32>()
            / input.len() as f32;
        assert!(difference > 0.01, "distortion barely changed the signal: {difference}");
        assert!(shaped.iter().all(|value| value.is_finite()));

        let mut overrides = std::collections::HashMap::new();
        overrides.insert(0usize, true);
        assert!(build_chain(&[Some(slot)], 48_000.0, &overrides).is_empty());
    }

    /// The distortion has to clip: a 0.1-amplitude sine through it must come out with a lower crest
    /// factor than it went in, or the effect is only an attenuator.
    #[test]
    fn distortion_clips_a_sine() {
        let mut unit = Distortion::new(0.415, 0.6016, 0.4648, 0.7744, 48_000.0);
        let mut peak_in = 0.0f32;
        let mut peak_out = 0.0f32;
        let (mut sum_in, mut sum_out) = (0.0f64, 0.0f64);
        for index in 0..48_000 {
            let t = index as f32 / 48_000.0;
            let input = 0.1 * (2.0 * std::f32::consts::PI * 220.0 * t).sin();
            let (left, _) = unit.process(input, input);
            peak_in = peak_in.max(input.abs());
            peak_out = peak_out.max(left.abs());
            sum_in += (input as f64).powi(2);
            sum_out += (left as f64).powi(2);
        }
        let crest = |peak: f32, sum: f64| 20.0 * (peak as f64 / (sum / 48_000.0).sqrt()).log10();
        let before = crest(peak_in, sum_in);
        let after = crest(peak_out, sum_out);
        assert!(
            after < before - 1.0,
            "crest {before:.1} dB in, {after:.1} dB out — no clipping"
        );
    }
}

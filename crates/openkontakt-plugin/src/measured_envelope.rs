//! Narrow compatibility response measured with phase/frequency-tagged sine samples.
//!
//! This is NOT a decoded Kontakt time/modulation law. The November 2026 Main Lite capture
//! establishes five CC84/CC72 responses for the sustained amplifier at CC70=CC71=127.
//! Match the actual envelope and routes, not the filename, and decline changed configurations.
//! Between captured CC values we interpolate effective durations. Other instruments and the
//! release-sample envelopes continue through the ordinary renderer.

use openkontakt::Ahdsr;
use openkontakt::ksp::EngineState;
use openkontakt::modulation::{Source, Target};

pub const ATTACK_SHAPE: f32 = 6.676_439;
pub const DECAY_SHAPE: f32 = 2.657_190;
pub const RELEASE_SHAPE: f32 = 2.653_306;

fn near(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.000_01
}

fn curve_matches(target: &Target, points: &[(f32, f32, f32)]) -> bool {
    !target.inverted
        && target.curve.as_ref().is_some_and(|curve| {
            curve.bipolar
                && curve.points.len() == points.len()
                && curve
                    .points
                    .iter()
                    .zip(points)
                    .all(|(p, &(x, y, shape))| near(p.x, x) && near(p.y, y) && near(p.shape, shape))
        })
}

fn interpolate(cc: f32, times: [f32; 5]) -> f32 {
    let knots = [0.0, 32.0, 64.0, 96.0, 127.0];
    let cc = cc.clamp(0.0, 127.0);
    for i in 0..4 {
        if cc <= knots[i + 1] {
            return times[i]
                + (times[i + 1] - times[i]) * (cc - knots[i]) / (knots[i + 1] - knots[i]);
        }
    }
    times[4]
}

pub fn response(state: &EngineState, group: usize, mut env: Ahdsr) -> Option<Ahdsr> {
    if !near(env.attack_curve, 0.83)
        || !near(env.attack_ms, 400.0)
        || !near(env.release_ms, 436.7)
        || !near(env.hold_ms, 0.0)
        || !near(env.sustain, 0.0)
        || !((env.decay_ms - 12000.0).abs() < 0.1 || (env.decay_ms - 25000.043).abs() < 0.1)
    {
        return None;
    }
    // A script's direct envelope edit takes precedence over the captured preset response.
    for name in [
        "ENGINE_PAR_ATTACK",
        "ENGINE_PAR_DECAY",
        "ENGINE_PAR_SUSTAIN",
        "ENGINE_PAR_RELEASE",
    ] {
        if state
            .parameter(name, group as i64, 0, -1)
            .or_else(|| state.parameter(name, -1, 0, -1))
            .is_some()
        {
            return None;
        }
    }
    let routes = state.modulation.get(group)?;
    let amp = routes.amp_slot()?;
    let mut inputs = [None; 4];
    for source in &routes.modulators {
        for (index, target) in source.targets.iter().enumerate() {
            if target.destination != Some(amp) {
                continue;
            }
            let field = match target.parameter.as_str() {
                "ahdsr_attack" => 0,
                "ahdsr_decay" => 1,
                "ahdsr_sustain" => 2,
                "ahdsr_release" => 3,
                _ => continue,
            };
            // Additional/disabled/unrecognised routes invalidate this measured configuration.
            if inputs[field].is_some() {
                return None;
            }
            let Source::Controller { number, .. } = source.source else {
                return None;
            };
            let expected_cc = [84, 70, 71, 72][field];
            if number != expected_cc {
                return None;
            }
            let amount = state
                .get_cached(
                    "ENGINE_PAR_MOD_TARGET_INTENSITY",
                    group as i64,
                    source.handle,
                    index as i64,
                )
                .or_else(|| {
                    state.get_cached(
                        "ENGINE_PAR_MOD_TARGET_INTENSITY",
                        group as i64,
                        source.handle,
                        -1,
                    )
                })
                .map(|value| value as f32 / 1_000_000.0)
                .unwrap_or(target.amount);
            if !near(amount, [1.0, 1.0, 1.0, 0.806998][field]) {
                return None;
            }
            let points: &[(f32, f32, f32)] = match field {
                0 => &[(0.0, 0.3108108, -0.15202703), (1.0, 1.0, 0.0)],
                1 => &[(0.0, 0.5, 0.050675675), (1.0, 1.0, 0.0)],
                2 => &[
                    (0.0, 0.0, 0.32094598),
                    (0.97244096, 1.0, 0.0),
                    (1.0, 1.0, 0.0),
                ],
                _ => &[(0.0, 0.4054054, 0.101351336), (1.0, 0.8513514, 0.0)],
            };
            if !curve_matches(target, points) {
                return None;
            }
            inputs[field] = state.controller_source_value(&source.source);
        }
    }
    let [Some(attack), Some(decay), Some(sustain), Some(release)] = inputs else {
        return None;
    };
    if !near(decay, 1.0) || !near(sustain, 1.0) {
        return None;
    }
    // Effective fitted times, not the patch's physical parameter values. Attack/decay fit
    // uses both independently tagged mics; neither microphone's gain is adjusted here.
    env.attack_ms = interpolate(
        attack * 127.0,
        [8.918015, 56.283157, 151.86566, 265.7688, 358.21918],
    );
    env.release_ms = interpolate(release * 127.0, [31.613, 40.186, 58.010, 102.226, 226.384]);
    Some(env)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolated_durations_are_continuous_and_monotonic() {
        let times = [8.918015, 56.283157, 151.86566, 265.7688, 358.21918];
        for (cc, expected) in [0.0, 32.0, 64.0, 96.0, 127.0].into_iter().zip(times) {
            assert!((interpolate(cc, times) - expected).abs() < 0.001);
        }
        for cc in 0..127 {
            assert!(interpolate(cc as f32, times) < interpolate(cc as f32 + 1.0, times));
        }
    }
}

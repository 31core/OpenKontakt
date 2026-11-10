//! Group amplitude envelopes (`BParEnv_AHDSR`, chunk `0x3F`).
//!
//! A group's amplitude envelope lives inside its internal-modulator array (`0x3B`): one slot's
//! modulator (`0x0D`) targets `volume` and contains an AHDSR object. The envelope is not part of the
//! zone data, so a player that ignores it can still make sound — just without the instrument's
//! attack, decay, sustain or release.
//!
//! The layout is a flat record (no structured sections), documented in
//! `docs/15-effects-and-modulation.md` §15.5.

use crate::group::Group;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ahdsr {
    /// −1…1; 0 is linear.
    pub attack_curve: f32,
    pub attack_ms: f32,
    pub decay_ms: f32,
    pub hold_ms: f32,
    pub release_ms: f32,
    /// Linear gain, 0…1.
    pub sustain: f32,
}

impl Default for Ahdsr {
    /// What a player falls back to when a group has no envelope: a short de-click attack and
    /// a quick release, i.e. the behaviour of a plain sampler.
    fn default() -> Self {
        Self {
            attack_curve: 0.0,
            attack_ms: 4.0,
            decay_ms: 0.0,
            hold_ms: 0.0,
            release_ms: 120.0,
            sustain: 1.0,
        }
    }
}

/// Envelope time mapping used both by the engine and by `get_engine_par_disp`: 0 ms at 0,
/// five seconds at 1, cubic in between. Kontakt's own law is not in the file, so a player picks one;
/// this one makes one library's stored 500 ms land near the middle of the knob, which is where the
/// original panel draws it.
/// The law is anchored on the one mapping the corpus pins down: one library stores a 500 ms decay
/// and the panel persists a controller value of 586 069 for that same envelope, so `0.586` reads as
/// 500 ms. Below that point the curve is quadratic, above it exponential up to 20 s at 1.0 — the
/// same shape Kontakt's envelope knobs have (fine control at the short end).
const ANCHOR_V: f32 = 0.586;
const ANCHOR_MS: f32 = 500.0;
const MAX_MS: f32 = 20_000.0;

pub fn normalized_to_ms(normalized: f32) -> f32 {
    let v = normalized.clamp(0.0, 1.0);
    if v <= ANCHOR_V {
        ANCHOR_MS * (v / ANCHOR_V).powi(2)
    } else {
        ANCHOR_MS * (MAX_MS / ANCHOR_MS).powf((v - ANCHOR_V) / (1.0 - ANCHOR_V))
    }
}

pub fn ms_to_normalized(ms: f32) -> f32 {
    let ms = ms.max(0.0);
    if ms <= ANCHOR_MS {
        ANCHOR_V * (ms / ANCHOR_MS).sqrt()
    } else {
        ANCHOR_V + (1.0 - ANCHOR_V) * (ms / ANCHOR_MS).ln() / (MAX_MS / ANCHOR_MS).ln()
    }
    .clamp(0.0, 1.0)
}

const CHUNK_AHDSR: u16 = 0x3F;
const CHUNK_INTERNAL_MODS: u16 = 0x3B;

/// Parse a `0x3F` body. The object is flat: `u8 flag`, `u16 version`, then six floats.
pub fn parse_ahdsr(body: &[u8]) -> Option<Ahdsr> {
    if body.len() < 3 + 6 * 4 {
        return None;
    }
    let flag = body[0];
    let version = u16::from_le_bytes([body[1], body[2]]);
    if flag != 0 || !(0x10..=0x11).contains(&version) {
        return None;
    }
    let mut at = 3;
    let mut next = || {
        let value = f32::from_le_bytes(body[at..at + 4].try_into().ok()?);
        at += 4;
        Some(value)
    };
    let envelope = Ahdsr {
        attack_curve: next()?,
        attack_ms: next()?,
        decay_ms: next()?,
        hold_ms: next()?,
        release_ms: next()?,
        sustain: next()?,
    };
    if !envelope.attack_ms.is_finite()
        || !envelope.decay_ms.is_finite()
        || !envelope.hold_ms.is_finite()
        || !envelope.release_ms.is_finite()
        || !envelope.sustain.is_finite()
    {
        return None;
    }
    Some(envelope)
}

/// The group's amplitude envelope: the first AHDSR anywhere inside its `0x3B` modulator array.
///
/// The modulator's target list decides which envelope addresses `volume`; decoding that two-pass
/// target/shaper grammar is not needed for the common case, because a group has exactly one
/// amplitude envelope and it is the first AHDSR in the array.
pub fn group_amp_envelope(group: &Group) -> Option<Ahdsr> {
    let array = group
        .children
        .iter()
        .find(|child| child.id == CHUNK_INTERNAL_MODS)?;
    find_ahdsr(&array.body, 0)
}

/// Depth-first search for a `0x3F` chunk inside nested structured objects.
///
/// A group's modulator array (`0x3B`) is a presence-prefixed chunk list whose prefix length varies
/// with the array version, and the envelope is wrapped twice (`0x0D` → `0x07` → `0x3F`). Rather
/// than hard-coding each level, the search accepts a small window of leading bytes and only
/// descends into ids that can lead to a modulator.
fn find_ahdsr(body: &[u8], depth: usize) -> Option<Ahdsr> {
    if depth > 8 || body.len() < 3 {
        return None;
    }
    if let Some(envelope) = parse_ahdsr(body) {
        return Some(envelope);
    }
    let payloads: Vec<Vec<u8>> = if body[0] == 1 {
        structured_children_section(body)
            .map(|children| vec![children])
            .unwrap_or_default()
    } else {
        vec![body[3..].to_vec()]
    };
    for payload in payloads {
        for start in 0..=8usize {
            if start >= payload.len() {
                break;
            }
            for (id, child) in walk_chunks(&payload[start..]) {
                if id == CHUNK_AHDSR {
                    if let Some(envelope) = parse_ahdsr(&child) {
                        return Some(envelope);
                    }
                }
                if matches!(id, 0x07 | 0x0D | 0x0C | 0x3B | 0x3C | 0x08 | 0x0A | 0x0B) {
                    if let Some(envelope) = find_ahdsr(&child, depth + 1) {
                        return Some(envelope);
                    }
                }
            }
        }
    }
    None
}

/// Chunks of a structured object's children section.
fn structured_children_section(body: &[u8]) -> Option<Vec<u8>> {
    let mut at = 3usize;
    let read_u32 = |at: &mut usize| -> Option<u32> {
        let value = u32::from_le_bytes(body.get(*at..*at + 4)?.try_into().ok()?);
        *at += 4;
        Some(value)
    };
    let private = read_u32(&mut at)? as usize;
    at += private;
    let public = read_u32(&mut at)? as usize;
    at += public;
    let children = read_u32(&mut at)? as usize;
    body.get(at..at + children).map(|slice| slice.to_vec())
}

fn walk_chunks(mut data: &[u8]) -> Vec<(u16, Vec<u8>)> {
    let mut out = Vec::new();
    while data.len() >= 6 {
        let id = u16::from_le_bytes([data[0], data[1]]);
        let len = u32::from_le_bytes([data[2], data[3], data[4], data[5]]) as usize;
        if data.len() < 6 + len {
            break;
        }
        out.push((id, data[6..6 + len].to_vec()));
        data = &data[6 + len..];
    }
    out
}

/// One envelope per group index, for the engine.
pub fn instrument_envelopes(groups: &[Group]) -> Vec<Ahdsr> {
    groups
        .iter()
        .map(|group| group_amp_envelope(group).unwrap_or_default())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ahdsr_body(attack: f32, decay: f32, hold: f32, release: f32, sustain: f32) -> Vec<u8> {
        let mut body = vec![0u8, 0x11, 0x00];
        for value in [0.0f32, attack, decay, hold, release, sustain] {
            body.extend_from_slice(&value.to_le_bytes());
        }
        body
    }

    #[test]
    fn parses_the_flat_record() {
        let body = ahdsr_body(250.0, 500.0, 10.0, 900.0, 0.5);
        let envelope = parse_ahdsr(&body).unwrap();
        assert_eq!(envelope.attack_ms, 250.0);
        assert_eq!(envelope.decay_ms, 500.0);
        assert_eq!(envelope.hold_ms, 10.0);
        assert_eq!(envelope.release_ms, 900.0);
        assert_eq!(envelope.sustain, 0.5);
    }

    #[test]
    fn rejects_structured_or_truncated_records() {
        let mut body = ahdsr_body(1.0, 1.0, 0.0, 1.0, 1.0);
        body[0] = 1;
        assert!(parse_ahdsr(&body).is_none());
        assert!(parse_ahdsr(&[0, 0x11, 0]).is_none());
    }

    #[test]
    fn finds_an_ahdsr_nested_in_a_chunk_list() {
        let envelope = ahdsr_body(100.0, 200.0, 0.0, 300.0, 0.4);
        let mut child = Vec::new();
        child.extend_from_slice(&CHUNK_AHDSR.to_le_bytes());
        child.extend_from_slice(&(envelope.len() as u32).to_le_bytes());
        child.extend_from_slice(&envelope);
        let mut outer = Vec::new();
        outer.extend_from_slice(&0x0Du16.to_le_bytes());
        outer.extend_from_slice(&(child.len() as u32).to_le_bytes());
        outer.extend_from_slice(&child);
        let found = find_ahdsr(&outer, 0).unwrap();
        assert_eq!(found.attack_ms, 100.0);
    }
}

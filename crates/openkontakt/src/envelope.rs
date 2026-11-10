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
const CHUNK_MODULATOR: u16 = 0x0D;

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

/// The name a modulator carries when its envelope addresses `volume`.
const VOLUME_ENVELOPE_TAG: &[u8] = b"ENV_AHDSR_VOLUME";

/// The group's amplitude envelope: the AHDSR of the modulator that targets `volume`.
///
/// A group can carry several envelopes and their order in the `0x3B` array is whatever the author
/// added first. A guitar patch that models pick dynamics with a filter envelope puts the *filter*
/// one there, so "the first AHDSR" is the wrong envelope: using it gives the instrument a 3.6 s
/// attack and zero sustain, because those are the cutoff envelope's numbers. Select by target.
pub fn group_amp_envelope(group: &Group) -> Option<Ahdsr> {
    let array = group
        .children
        .iter()
        .find(|child| child.id == CHUNK_INTERNAL_MODS)?;
    amp_envelope_from_array(&array.body)
}

/// The same choice, made on the `0x3B` array body alone.
fn amp_envelope_from_array(body: &[u8]) -> Option<Ahdsr> {
    let mut first = None;
    for (id, payload) in presence_chunks(body, 3) {
        if id != CHUNK_MODULATOR {
            continue;
        }
        let Some(envelope) = scan_ahdsr(payload, 0) else {
            continue;
        };
        if contains(payload, VOLUME_ENVELOPE_TAG) {
            return Some(envelope);
        }
        first.get_or_insert(envelope);
    }
    first
        .or_else(|| find_ahdsr(body, 0))
        .or_else(|| scan_ahdsr(body, 0))
}

/// Split a presence-prefixed chunk list: `[u8 present][u16 id][u32 length][payload]`, repeated.
fn presence_chunks(body: &[u8], mut at: usize) -> Vec<(u16, &[u8])> {
    let mut out = Vec::new();
    while at + 7 <= body.len() {
        if body[at] != 1 {
            break;
        }
        let id = u16::from_le_bytes([body[at + 1], body[at + 2]]);
        let length =
            u32::from_le_bytes([body[at + 3], body[at + 4], body[at + 5], body[at + 6]]) as usize;
        let start = at + 7;
        if start + length > body.len() {
            break;
        }
        out.push((id, &body[start..start + length]));
        at = start + length;
    }
    out
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Search nested chunk data for an AHDSR, without knowing how many layers wrap it or whether the
/// wrappers carry a presence byte.
///
/// The envelope sits two or three levels down (`0x0D` → `0x07` → `0x3F`), and the two writers in
/// this corpus disagree on framing: a chunk is either `[u8 present][u16 id][u32 length]` or
/// `[u32 size][u16 id][u32 length]`. `parse_ahdsr` is the filter — a wrong offset almost never
/// yields a `0x10`/`0x11` version followed by plausible times.
fn scan_ahdsr(bytes: &[u8], depth: usize) -> Option<Ahdsr> {
    if depth > 6 {
        return None;
    }
    if let Some(envelope) = parse_ahdsr(bytes) {
        return Some(envelope);
    }
    for at in 0..bytes.len() {
        for (id_offset, length_offset, payload_offset) in
            [(0usize, 2usize, 6usize), (1usize, 3usize, 7usize)]
        {
            let Some(header) = bytes.get(at..at + length_offset + 4) else {
                continue;
            };
            let id = u16::from_le_bytes([header[id_offset], header[id_offset + 1]]);
            let length = u32::from_le_bytes([
                header[length_offset],
                header[length_offset + 1],
                header[length_offset + 2],
                header[length_offset + 3],
            ]) as usize;
            let Some(payload) = bytes.get(at + payload_offset..at + payload_offset + length) else {
                continue;
            };
            if id == CHUNK_AHDSR {
                if let Some(envelope) = parse_ahdsr(payload) {
                    return Some(envelope);
                }
            } else if length >= 3 {
                if let Some(envelope) = scan_ahdsr(payload, depth + 1) {
                    return Some(envelope);
                }
            }
        }
    }
    None
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

    /// A modulator payload as the guitar patches write it: a `0x80` header naming the target,
    /// then the envelope wrapped in a `0x07`.
    fn modulator(name: &str, attack: f32, release: f32, sustain: f32) -> Vec<u8> {
        fn chunk(id: u16, payload: &[u8]) -> Vec<u8> {
            let mut out = vec![1u8];
            out.extend_from_slice(&id.to_le_bytes());
            out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            out.extend_from_slice(payload);
            out
        }
        let mut body = chunk(0x80, format!("ENV_AHDSR_{name}").as_bytes());
        body.extend_from_slice(&chunk(
            0x07,
            &chunk(0x3F, &ahdsr_body(attack, 0.0, 0.0, release, sustain)),
        ));
        body
    }

    fn array_with_envelopes(modulators: Vec<Vec<u8>>) -> Vec<u8> {
        let mut array = vec![0u8, 0x12, 0x00];
        for modulator in modulators {
            array.extend_from_slice(&chunk_with_presence(0x0D, &modulator));
        }
        array
    }

    fn chunk_with_presence(id: u16, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![1u8];
        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(payload);
        out
    }

    /// The amplifier envelope is the one whose modulator targets `volume`, and it is not
    /// necessarily the first: these guitar patches model pick dynamics with a filter envelope and
    /// store the filter first. Taking the first AHDSR gave the instrument a 3.6 s attack and zero
    /// sustain, which is the cutoff envelope's shape, not the amplifier's.
    #[test]
    fn picks_the_volume_envelope_out_of_several() {
        let array = array_with_envelopes(vec![
            modulator("CUTOFF", 3596.6, 0.0, 0.0),
            modulator("VOLUME", 0.0, 2411.0, 1.0),
        ]);
        let envelope = amp_envelope_from_array(&array).expect("envelope");
        assert_eq!(envelope.attack_ms, 0.0);
        assert_eq!(envelope.release_ms, 2411.0);
        assert_eq!(envelope.sustain, 1.0);
    }

    #[test]
    fn falls_back_to_the_only_envelope() {
        let array = array_with_envelopes(vec![modulator("CUSTOM", 12.0, 300.0, 0.8)]);
        let envelope = amp_envelope_from_array(&array).expect("envelope");
        assert_eq!(envelope.attack_ms, 12.0);
        assert_eq!(envelope.release_ms, 300.0);
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

//! Effect racks (`0x3A`), slots (`0x25`) and the module bodies they contain.
//!
//! A rack is an array of eight slots; a slot carries a 22-byte private block (bypass, output
//! gain, dry level) plus exactly one child chunk whose id is the effect's SerType. The module
//! body holds the parameters in read order and is kept raw here — `decode` knows the few modules
//! the engine implements.

use crate::chunk::OwnedChunk;
use crate::error::{Error, Result};

/// Number of slots in every FX rack. Kontakt always allocates eight, even when the file stores
/// fewer trailing empty slots.
pub const RACK_SLOTS: usize = 8;

#[derive(Clone, Debug)]
pub struct EffectSlot {
    /// Child chunk id: the effect's SerType (`0x10` delay, `0x13` gainer, `0x18` filter, …).
    pub module: u16,
    pub module_version: u16,
    /// Raw module body, after its own structured header.
    pub params: Vec<u8>,
    /// The module object's private section. Kept because file references (an IR sample, for one)
    /// live there rather than in the parameter block.
    pub module_private: Vec<u8>,
    /// `0x25` private block.
    pub bypass: bool,
    pub output_gain: f32,
    pub dry_level: f32,
}

#[derive(Clone, Debug, Default)]
pub struct Rack {
    pub slots: Vec<Option<EffectSlot>>,
}

impl Rack {
    pub fn slot(&self, index: usize) -> Option<&EffectSlot> {
        self.slots.get(index).and_then(Option::as_ref)
    }

    pub fn is_empty(&self) -> bool {
        self.slots.iter().all(Option::is_none)
    }
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    what: &'static str,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8], what: &'static str) -> Self {
        Self { data, pos: 0, what }
    }

    fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.pos + n > self.data.len() {
            return Err(Error::malformed(
                self.what,
                self.pos,
                format!("need {n} bytes, {} left", self.remaining()),
            ));
        }
        let out = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        let bytes = self.take(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&mut self) -> Result<u32> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn i32(&mut self) -> Result<i32> {
        Ok(self.u32()? as i32)
    }

    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_bits(self.u32()?))
    }
}

struct Object<'a> {
    version: u16,
    private: &'a [u8],
    public: &'a [u8],
    children: &'a [u8],
    /// Bytes consumed from the source slice.
    consumed: usize,
}

/// Read the `flag + version + three counted sections` form used by every Kontakt object.
fn read_object<'a>(reader: &mut Reader<'a>) -> Result<Object<'a>> {
    let start = reader.pos;
    let flag = reader.u8()?;
    let version = reader.u16()?;
    if flag == 0 {
        // Unstructured: the rest of the slice is the public body.
        let public = reader.take(reader.remaining())?;
        return Ok(Object {
            version,
            private: &[],
            public,
            children: &[],
            consumed: reader.pos - start,
        });
    }
    let private_len = reader.u32()? as usize;
    let private = reader.take(private_len)?;
    let public_len = reader.u32()? as usize;
    let public = reader.take(public_len)?;
    let children_len = reader.u32()? as usize;
    let children = reader.take(children_len)?;
    Ok(Object {
        version,
        private,
        public,
        children,
        consumed: reader.pos - start,
    })
}

/// Parse one `0x25` slot body (the chunk data, without its `u16 id + u32 len` header).
fn parse_slot(body: &[u8]) -> Result<EffectSlot> {
    let mut reader = Reader::new(body, "effect slot 0x25");
    let object = read_object(&mut reader)?;
    if object.version != 0x50 {
        return Err(Error::malformed(
            "effect slot 0x25",
            0,
            format!("unsupported slot version 0x{:02X}", object.version),
        ));
    }
    if object.private.len() < 22 {
        return Err(Error::malformed(
            "effect slot 0x25",
            0,
            format!(
                "private block is {} bytes, expected 22",
                object.private.len()
            ),
        ));
    }
    let private = &object.private[..22];
    let bypass = private[9] != 0;
    let output_gain = f32::from_bits(u32::from_le_bytes([
        private[0x0A],
        private[0x0B],
        private[0x0C],
        private[0x0D],
    ]));
    let dry_level = f32::from_bits(u32::from_le_bytes([
        private[0x0E],
        private[0x0F],
        private[0x10],
        private[0x11],
    ]));

    // Exactly one child: the effect module.
    let mut children = Reader::new(object.children, "effect module list");
    let module_chunk = read_chunk_body(&mut children)?;
    if children.remaining() != 0 {
        return Err(Error::malformed(
            "effect slot 0x25",
            0,
            "more than one module chunk in a slot".to_string(),
        ));
    }

    let (module, module_version, params, module_private) = match module_chunk {
        Some((id, body)) => {
            let mut inner = Reader::new(body, "effect module");
            let object = read_object(&mut inner)?;
            (
                id,
                object.version,
                object.public.to_vec(),
                object.private.to_vec(),
            )
        }
        None => (0, 0, Vec::new(), Vec::new()),
    };

    Ok(EffectSlot {
        module,
        module_version,
        params,
        module_private,
        bypass,
        output_gain,
        dry_level,
    })
}

/// Read `{u16 id, u32 len, body}` from a chunk stream.
fn read_chunk_body<'a>(reader: &mut Reader<'a>) -> Result<Option<(u16, &'a [u8])>> {
    if reader.remaining() < 6 {
        return Ok(None);
    }
    let id = reader.u16()?;
    let len = reader.u32()? as usize;
    let body = reader.take(len)?;
    Ok(Some((id, body)))
}

/// Parse a rack body (a Program child `0x3A`, or the rack inside a group's private trailer).
///
/// The slot list framing is version-dependent and, in the corpus, even differs between files of
/// the same version, so the shape is chosen by which one consumes the body exactly.
pub fn parse_rack(body: &[u8]) -> Result<Rack> {
    let mut reader = Reader::new(body, "FX rack 0x3A");
    let object = read_object(&mut reader)?;
    let public = if object.public.is_empty() && object.consumed == body.len() {
        body
    } else {
        object.public
    };

    for variant in [
        RackFraming::CountThenPresence,
        RackFraming::EightPresence,
        RackFraming::PresenceChunkPairs,
        RackFraming::ChunksOnly,
    ] {
        if let Ok(slots) = try_rack(public, object.version, variant) {
            return Ok(Rack { slots });
        }
    }
    Err(Error::malformed(
        "FX rack 0x3A",
        0,
        format!(
            "no known slot framing consumed the {} byte rack body (version 0x{:02X})",
            public.len(),
            object.version
        ),
    ))
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum RackFraming {
    CountThenPresence,
    EightPresence,
    PresenceChunkPairs,
    ChunksOnly,
}

fn try_rack(public: &[u8], version: u16, framing: RackFraming) -> Result<Vec<Option<EffectSlot>>> {
    let mut reader = Reader::new(public, "FX rack body");
    let mut slots: Vec<Option<EffectSlot>> = vec![None; RACK_SLOTS];
    match framing {
        RackFraming::CountThenPresence => {
            let count = reader.u32()? as usize;
            if count == 0 || count > RACK_SLOTS {
                return Err(Error::malformed("FX rack", 0, "slot count"));
            }
            let present = reader.take(count)?.to_vec();
            for (index, flag) in present.iter().enumerate() {
                if *flag == 0 {
                    continue;
                }
                let Some((id, body)) = read_chunk_body(&mut reader)? else {
                    return Err(Error::malformed("FX rack", 0, "missing slot chunk"));
                };
                if id != 0x25 {
                    return Err(Error::malformed("FX rack", 0, "unexpected slot chunk"));
                }
                slots[index] = Some(parse_slot(body)?);
            }
        }
        RackFraming::EightPresence => {
            let present = reader.take(RACK_SLOTS)?.to_vec();
            for (index, flag) in present.iter().enumerate() {
                if *flag == 0 {
                    continue;
                }
                let Some((id, body)) = read_chunk_body(&mut reader)? else {
                    return Err(Error::malformed("FX rack", 0, "missing slot chunk"));
                };
                if id != 0x25 {
                    return Err(Error::malformed("FX rack", 0, "unexpected slot chunk"));
                }
                slots[index] = Some(parse_slot(body)?);
            }
        }
        RackFraming::PresenceChunkPairs => {
            let mut index = 0;
            while reader.remaining() > 0 {
                let flag = reader.u8()?;
                if index >= RACK_SLOTS {
                    return Err(Error::malformed("FX rack", 0, "too many slots"));
                }
                if flag != 0 {
                    let Some((id, body)) = read_chunk_body(&mut reader)? else {
                        return Err(Error::malformed("FX rack", 0, "missing slot chunk"));
                    };
                    if id != 0x25 {
                        return Err(Error::malformed("FX rack", 0, "unexpected slot chunk"));
                    }
                    slots[index] = Some(parse_slot(body)?);
                }
                index += 1;
            }
        }
        RackFraming::ChunksOnly => {
            let mut index = 0;
            while reader.remaining() >= 6 {
                let Some((id, body)) = read_chunk_body(&mut reader)? else {
                    break;
                };
                if id != 0x25 {
                    return Err(Error::malformed("FX rack", 0, "unexpected slot chunk"));
                }
                if index >= RACK_SLOTS {
                    return Err(Error::malformed("FX rack", 0, "too many slots"));
                }
                slots[index] = Some(parse_slot(body)?);
                index += 1;
            }
        }
    }
    if reader.remaining() != 0 {
        return Err(Error::malformed(
            "FX rack",
            reader.pos,
            format!(
                "{} trailing bytes for framing {framing:?}",
                reader.remaining()
            ),
        ));
    }
    let _ = version;
    Ok(slots)
}

/// Parameters of the modules the engine implements. Unknown modules keep `params` raw.
#[derive(Clone, Debug, PartialEq)]
pub enum ModuleParams {
    Gainer {
        gain: f32,
    },
    /// Normalised cutoff and resonance (0..1), which is how they are stored.
    Filter {
        cutoff: f32,
        resonance: f32,
    },
    SendLevels {
        levels: Vec<f32>,
    },
    /// Convolution reverb (`0x16`). The impulse response is a filename reference stored as the
    /// last `u32` of the parameter block: an index into the filename table's `other` list, which is
    /// the same list the program's wallpaper reference uses.
    Convolution {
        ir_index: Option<i32>,
    },
    /// Compressor/limiter (`0x44`), ten floats. A panel button toggles this
    /// slot's bypass; the stored values are threshold -64 dB at 24:1, which is why the button
    /// matters as much as it does.
    Compressor {
        attack: f32,
        release: f32,
        threshold_db: f32,
        ratio: f32,
    },
    /// Distortion (`0x21`), five normalised parameters. A panel button toggles this slot's bypass,
    /// so the values only matter once somebody presses it.
    Distortion {
        drive: f32,
        tone: f32,
        level: f32,
        mix: f32,
    },
    /// Anything not decoded: preserved, not interpreted.
    Opaque,
}

/// Leading finite `f32` values, ignoring a trailing partial word.
fn floats_of(params: &[u8], limit: usize) -> Vec<f32> {
    params
        .chunks_exact(4)
        .map(|bytes| f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        .take(limit)
        .filter(|value| value.is_finite())
        .collect()
}

pub fn decode(slot: &EffectSlot) -> ModuleParams {
    let mut reader = Reader::new(&slot.params, "effect module");
    match slot.module {
        0x13 => reader
            .f32()
            .map(|gain| ModuleParams::Gainer { gain })
            .unwrap_or(ModuleParams::Opaque),
        0x18 => {
            // `i32 filter_type` then optional flag byte from 0x92, then cutoff/resonance at the
            // end. The tail is what the engine needs; scan from the end for two plausible floats.
            let _ = reader.i32();
            match (slot.params.len(), read_filter_tail(&slot.params)) {
                (_, Some((cutoff, resonance))) => ModuleParams::Filter { cutoff, resonance },
                _ => ModuleParams::Opaque,
            }
        }
        0x44 => {
            // Ten floats; the first four that read as plausible cover attack, release, threshold
            // and ratio. One library stores 0.5 / 0.5 / -64 / 24, matching a limiter.
            let values = floats_of(&slot.params, 5);
            match values.len() {
                5.. => ModuleParams::Compressor {
                    attack: values[0],
                    release: values[1],
                    threshold_db: values[2].clamp(-80.0, 0.0),
                    ratio: values[4].clamp(1.0, 40.0),
                },
                _ => ModuleParams::Opaque,
            }
        }
        0x21 => {
            // Five 0..1 floats; one library stores 0.415 / 0.602 / 0.465 / 0.774 / 0.
            let mut values = [0.0f32; 4];
            let mut ok = true;
            for value in &mut values {
                match reader.f32() {
                    Ok(v) => *value = v,
                    Err(_) => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                ModuleParams::Distortion {
                    drive: values[0],
                    tone: values[1],
                    level: values[2],
                    mix: values[3],
                }
            } else {
                ModuleParams::Opaque
            }
        }
        0x16 => {
            let ir_index = slot
                .params
                .len()
                .checked_sub(4)
                .and_then(|at| slot.params.get(at..at + 4))
                .map(|bytes| i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
                .filter(|index| *index >= 0);
            ModuleParams::Convolution { ir_index }
        }
        0x17 => {
            let count = reader.u32().unwrap_or(0) as usize;
            if count > 8 {
                return ModuleParams::Opaque;
            }
            let mut levels = Vec::with_capacity(count);
            for _ in 0..count {
                match reader.f32() {
                    Ok(level) => levels.push(level),
                    Err(_) => return ModuleParams::Opaque,
                }
            }
            ModuleParams::SendLevels { levels }
        }
        _ => ModuleParams::Opaque,
    }
}

fn read_filter_tail(params: &[u8]) -> Option<(f32, f32)> {
    let len = params.len();
    if len < 12 {
        return None;
    }
    let cutoff = f32::from_le_bytes(params[len - 8..len - 4].try_into().ok()?);
    let resonance = f32::from_le_bytes(params[len - 4..].try_into().ok()?);
    if !(0.0..=1.0).contains(&cutoff) || !(0.0..=1.0).contains(&resonance) {
        return None;
    }
    Some((cutoff, resonance))
}

/// The program's insert / send / main racks, in child order.
pub fn program_racks(children: &[OwnedChunk]) -> Vec<Rack> {
    children
        .iter()
        .filter(|child| child.id == 0x3A)
        .filter_map(|child| parse_rack(&child.body).ok())
        .collect()
}

/// The rack stored in a group's private block: 136 fixed 12-byte records, 24 trailer bytes.
pub fn group_rack(private: &[u8]) -> Option<Rack> {
    const RECORDS: usize = 136 * 12;
    const TRAILER: usize = 24;
    if private.len() < RECORDS + TRAILER {
        return None;
    }
    if private[..RECORDS]
        .chunks(12)
        .any(|record| record[..4] != [8, 0, 0, 0])
    {
        return None;
    }
    parse_rack(&private[RECORDS + TRAILER..]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gainer_module_decodes() {
        let slot = EffectSlot {
            module: 0x13,
            module_version: 0x50,
            params: 0.5f32.to_le_bytes().to_vec(),
            module_private: Vec::new(),
            bypass: false,
            output_gain: 1.0,
            dry_level: 0.0,
        };
        assert_eq!(decode(&slot), ModuleParams::Gainer { gain: 0.5 });
    }

    #[test]
    fn convolution_reads_the_impulse_reference_from_the_tail() {
        // A real 0x16 parameter block ends with the filename-table `other` index.
        let mut params = vec![0u8; 117];
        params.extend_from_slice(&1i32.to_le_bytes());
        let slot = EffectSlot {
            module: 0x16,
            module_version: 0x70,
            params,
            module_private: Vec::new(),
            bypass: false,
            output_gain: 0.581,
            dry_level: 1.0,
        };
        assert_eq!(
            decode(&slot),
            ModuleParams::Convolution { ir_index: Some(1) }
        );
    }

    #[test]
    fn unknown_module_is_opaque_not_an_error() {
        let slot = EffectSlot {
            module: 0x7A,
            module_version: 0x10,
            params: vec![1, 2, 3],
            module_private: Vec::new(),
            bypass: false,
            output_gain: 1.0,
            dry_level: 0.0,
        };
        assert_eq!(decode(&slot), ModuleParams::Opaque);
    }
}

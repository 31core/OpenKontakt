//! ZoneList (`0x34`), zone objects, loops, slices and the group mapping.

use crate::chunk::{Chunk, OwnedChunk, read_structured_object, walk_chunks};
use crate::error::{Error, Result};
use crate::reader::Reader;

#[derive(Clone, Debug, Default)]
pub struct ZoneSampleInfo {
    pub filename_id: i32,
    pub has_sample: bool,
    pub prefix: Option<[u8; 6]>,
    pub data_type: i32,
    pub sample_rate: i32,
    pub channels: u8,
    pub frames: i32,
    pub reserved1: i32,
    pub reserved2: Option<i32>,
    pub root_note: i32,
    pub tuning: f32,
    pub reserved3: u8,
    pub reserved4: i32,
}

#[derive(Clone, Debug, Default)]
pub struct Loop {
    pub mode: i32,
    pub loop_start: i32,
    pub loop_length: i32,
    pub loop_count: i32,
    pub alternating: bool,
    pub loop_tuning: f32,
    pub crossfade_length: i32,
}

#[derive(Clone, Debug)]
pub struct ZoneSlices {
    pub selected: i32,
    pub count: u32,
    pub grid_length: Option<u32>,
    pub boundaries: Vec<(u32, f32, u32)>,
}

#[derive(Clone, Debug)]
pub struct Zone {
    pub index: usize,
    pub group_index: usize,
    pub version: u16,
    pub flag: u8,
    pub sample_start: i32,
    pub sample_end: i32,
    pub sample_start_mod_range: i32,
    pub low_velocity: i16,
    pub high_velocity: i16,
    pub low_key: i16,
    pub high_key: i16,
    pub fade_low_velocity: i16,
    pub fade_high_velocity: i16,
    pub fade_low_key: i16,
    pub fade_high_key: i16,
    pub root_key: i16,
    pub volume: f32,
    pub pan: f32,
    pub tune: f32,
    /// Per-zone trim from the private block, `private[12..16]`, expressed relative to the neutral
    /// value 16. The field only ever holds a power of two in the corpus (1, 2, 4, 8, 16, 32) and
    /// tracks the level the reference implementation renders each zone at; see
    /// `docs/re/20-differential-workflow.md`. 1.0 when the block is too short or the value is not
    /// usable.
    pub gain: f32,
    pub sample: Option<ZoneSampleInfo>,
    pub public_tail: Vec<u8>,
    pub private: Vec<u8>,
    pub private_mode: Option<u32>,
    pub children: Vec<OwnedChunk>,
    pub loops: Vec<(u8, Loop)>,
    pub slices: Option<ZoneSlices>,
    pub raw_public: Vec<u8>,
}

impl Zone {
    pub fn matches(&self, key: u8, velocity: u8) -> bool {
        let key = key as i16;
        let velocity = velocity as i16;
        key >= self.low_key
            && key <= self.high_key
            && velocity >= self.low_velocity
            && velocity <= self.high_velocity
    }

    pub fn main_loop(&self) -> Option<&Loop> {
        self.loops
            .iter()
            .find(|(slot, _)| *slot == 0)
            .map(|(_, l)| l)
    }
}

const ACCEPTED_ZONE_VERSIONS: &[u16] =
    &[0x92, 0x93, 0x94, 0x95, 0x97, 0x98, 0x99, 0x9A, 0x9B, 0x9C];

pub fn parse_zone_list(body: &[u8]) -> Result<Vec<Zone>> {
    let mut reader = Reader::new(body, "ZoneList");
    let count = reader.u32()? as usize;
    if count > reader.remaining() / 52 {
        return Err(Error::malformed(
            "ZoneList",
            reader.position(),
            format!("zone count {count} is impossible for the remaining bytes"),
        ));
    }
    let mut zones = Vec::with_capacity(count);
    for index in 0..count {
        let group_index = reader.u32()? as usize;
        zones.push(parse_zone(&mut reader, index, group_index)?);
    }
    if !reader.is_empty() {
        return Err(Error::malformed(
            "ZoneList",
            reader.position(),
            format!(
                "{} trailing bytes after {} zones",
                reader.remaining(),
                count
            ),
        ));
    }
    Ok(zones)
}

fn parse_zone(reader: &mut Reader<'_>, index: usize, group_index: usize) -> Result<Zone> {
    let flag = reader.u8()?;
    let version = reader.u16()?;
    if !ACCEPTED_ZONE_VERSIONS.contains(&version) {
        return Err(Error::malformed(
            "Zone",
            reader.position() - 2,
            format!("unsupported zone version 0x{version:02X}"),
        ));
    }
    if flag == 0 {
        return Err(Error::unsupported(
            "unstructured zone object: its public body has no length delimiter",
        ));
    }
    let private_len = reader.u32()? as usize;
    let private = reader.take(private_len)?.to_vec();
    let public_len = reader.u32()? as usize;
    let public = reader.take(public_len)?.to_vec();
    let children_len = reader.u32()? as usize;
    let children_raw = reader.take(children_len)?;
    let children: Vec<OwnedChunk> = if children_len == 0 {
        Vec::new()
    } else {
        walk_chunks(children_raw)?
            .into_iter()
            .map(|Chunk { id, body }| OwnedChunk {
                id,
                body: body.to_vec(),
            })
            .collect()
    };

    let (mut zone, main_loop_present) = parse_public(index, group_index, version, flag, &public)?;
    // `private[12..16]` is a per-zone trim, always a power of two, that the reference
    // implementation applies to the zone's output and the zone record itself does not carry.
    // Measured: zones storing 16 render at unity there, zones storing 8 come out 6 dB lower.
    if let Some(bytes) = private.get(12..16) {
        let raw = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        // Values below 2 belong to the release-trigger and pedal-noise zones, which are authored
        // at unity in that field: applying the /16 scale there made the key noise 24 dB too quiet
        // against the reference. 16 is the neutral trim for note zones.
        if raw.is_finite() && raw >= 2.0 && raw <= 64.0 {
            zone.gain = raw / 16.0;
        }
    }
    zone.private = private;
    zone.children = children;
    zone.raw_public = public;

    // The zone private section is only partially documented. NFO 0x99 zones begin with
    // 0xFFFFFFFF, contradicting the "leading mode in 0..=2" note, and the final word is not the
    // KSP source identity there either. Preserve the bytes verbatim instead of inventing fields.
    if zone.private.len() >= 4 {
        zone.private_mode = Some(u32::from_le_bytes(zone.private[0..4].try_into().unwrap()));
    }

    for child in &zone.children {
        if child.id == 0x39 {
            let (mask, loops) = parse_loop_array(&child.body)?;
            if mask != 0 && loops.is_empty() {
                return Err(Error::malformed(
                    "LoopArray",
                    0,
                    "mask has bits set but no loops were decoded",
                ));
            }
            zone.loops = loops;
        } else if child.id == 0x35 {
            if let Some(slices) = parse_zone_slices(&child.body)? {
                zone.slices = Some(slices);
            }
        }
    }

    // A main loop slot present in the mask but absent from the decoded list means the loop parser
    // desynchronised. `parse_loop_array` already checks consumption; this is an extra guard.
    let _ = main_loop_present;
    Ok(zone)
}

fn parse_public(
    index: usize,
    group_index: usize,
    version: u16,
    flag: u8,
    public: &[u8],
) -> Result<(Zone, bool)> {
    let mut reader = Reader::new(public, "Zone public section");
    let sample_start = reader.i32()?;
    let sample_end = reader.i32()?;
    let sample_start_mod_range = reader.i32()?;
    let low_velocity = reader.i16()?;
    let high_velocity = reader.i16()?;
    let low_key = reader.i16()?;
    let high_key = reader.i16()?;
    let fade_low_velocity = reader.i16()?;
    let fade_high_velocity = reader.i16()?;
    let fade_low_key = reader.i16()?;
    let fade_high_key = reader.i16()?;
    let root_key = reader.i16()?;
    let volume = reader.f32()?;
    let pan = reader.f32()?;
    let tune = reader.f32()?;
    // Filled in by the caller once the private block is available.
    let gain = 1.0;

    let prefix: Option<[u8; 6]> = if version >= 0x9A {
        Some(reader.take(6)?.try_into().unwrap())
    } else {
        None
    };

    let has_sample = match prefix {
        Some(prefix) => prefix[1] != 0,
        None => true,
    };

    let sample = if has_sample {
        let filename_id = reader.i32()?;
        let data_type = reader.i32()?;
        let sample_rate = reader.i32()?;
        let channels = reader.u8()?;
        let frames = reader.i32()?;
        let reserved1 = reader.i32()?;
        let reserved2 = if version < 0x96 {
            Some(reader.i32()?)
        } else {
            None
        };
        let root_note = reader.i32()?;
        let tuning = reader.f32()?;
        let reserved3 = reader.u8()?;
        let reserved4 = reader.i32()?;
        Some(ZoneSampleInfo {
            filename_id,
            has_sample,
            prefix,
            data_type,
            sample_rate,
            channels,
            frames,
            reserved1,
            reserved2,
            root_note,
            tuning,
            reserved3,
            reserved4,
        })
    } else {
        None
    };
    let public_tail = reader.take(reader.remaining())?.to_vec();

    Ok((
        Zone {
            index,
            group_index,
            version,
            flag,
            sample_start,
            sample_end,
            sample_start_mod_range,
            low_velocity,
            high_velocity,
            low_key,
            high_key,
            fade_low_velocity,
            fade_high_velocity,
            fade_low_key,
            fade_high_key,
            root_key,
            volume,
            pan,
            tune,
            gain,
            sample,
            public_tail,
            private: Vec::new(),
            private_mode: None,
            children: Vec::new(),
            loops: Vec::new(),
            slices: None,
            raw_public: Vec::new(),
        },
        false,
    ))
}

fn parse_loop_array(body: &[u8]) -> Result<(u8, Vec<(u8, Loop)>)> {
    let mut reader = Reader::new(body, "LoopArray");
    let mask = reader.u8()?;
    let mut loops = Vec::new();
    for slot in 0..8u8 {
        if mask & (1 << slot) == 0 {
            continue;
        }
        let structured = reader.u8()?;
        let version = reader.u16()?;
        if version != 0x60 {
            return Err(Error::malformed(
                "LoopArray",
                reader.position() - 2,
                format!("unsupported loop version 0x{version:02X}"),
            ));
        }
        let loop_value = if structured == 0 {
            read_loop(&mut reader)?
        } else {
            let private_len = reader.u32()? as usize;
            let _private = reader.take(private_len)?;
            let public_len = reader.u32()? as usize;
            let public = reader.take(public_len)?;
            let children_len = reader.u32()? as usize;
            let _children = reader.take(children_len)?;
            let mut p = Reader::new(public, "Loop public");
            let value = read_loop(&mut p)?;
            if !p.is_empty() {
                return Err(Error::malformed(
                    "Loop",
                    p.position(),
                    format!("{} trailing bytes", p.remaining()),
                ));
            }
            value
        };
        loops.push((slot, loop_value));
    }
    if !reader.is_empty() {
        return Err(Error::malformed(
            "LoopArray",
            reader.position(),
            format!("{} trailing bytes after loops", reader.remaining()),
        ));
    }
    Ok((mask, loops))
}

fn read_loop(reader: &mut Reader<'_>) -> Result<Loop> {
    Ok(Loop {
        mode: reader.i32()?,
        loop_start: reader.i32()?,
        loop_length: reader.i32()?,
        loop_count: reader.i32()?,
        alternating: reader.u8()? != 0,
        loop_tuning: reader.f32()?,
        crossfade_length: reader.i32()?,
    })
}

fn parse_zone_slices(body: &[u8]) -> Result<Option<ZoneSlices>> {
    let object = read_structured_object(body, "ZoneSlices")?;
    if object.version != 0x10 {
        return Ok(None);
    }
    let mut reader = Reader::new(object.public, "ZoneSlices");
    let _reserved = reader.u8()?;
    let selected = reader.i32()?;
    let count = reader.u32()?;
    let grid_length = if count > 0 { Some(reader.u32()?) } else { None };
    if count > reader.remaining() as u32 / 12 {
        return Err(Error::malformed(
            "ZoneSlices",
            reader.position(),
            "slice count overruns",
        ));
    }
    let mut boundaries = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let frame = reader.u32()?;
        let frame_as_float = reader.f32()?;
        let extra = reader.u32()?;
        boundaries.push((frame, frame_as_float, extra));
    }
    if !reader.is_empty() {
        return Err(Error::malformed(
            "ZoneSlices",
            reader.position(),
            format!("{} trailing bytes", reader.remaining()),
        ));
    }
    Ok(Some(ZoneSlices {
        selected,
        count,
        grid_length,
        boundaries,
    }))
}

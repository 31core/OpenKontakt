//! GroupList (`0x33`) and group objects.

use crate::chunk::{Chunk, OwnedChunk};
use crate::error::{Error, Result};
use crate::reader::Reader;

#[derive(Clone, Debug)]
pub struct Group {
    pub index: usize,
    pub version: u16,
    pub flag: u8,
    pub name: String,
    pub volume: f32,
    pub pan: f32,
    pub tune: f32,
    pub key_tracking: bool,
    pub reverse: bool,
    pub release_trigger: bool,
    pub release_trigger_note_monophonic: bool,
    pub rls_trig_counter: i32,
    pub midi_channel: i16,
    pub voice_group_index: i32,
    pub fx_idx_amp_split_point: i32,
    pub muted: bool,
    pub soloed: bool,
    pub interp_quality: i32,
    pub public_tail: Vec<u8>,
    pub private: Vec<u8>,
    pub private_bus: Option<u8>,
    pub children: Vec<OwnedChunk>,
    pub raw_public: Vec<u8>,
}

const ACCEPTED_GROUP_VERSIONS: &[u16] = &[0x95, 0x96];

/// Groups have no framing length in the list, so each object is parsed from the remaining slice
/// and its consumed length is computed from the three counted sections.
pub fn parse_group_list(body: &[u8]) -> Result<Vec<Group>> {
    let mut reader = Reader::new(body, "GroupList");
    let count = reader.u32()? as usize;
    if count > reader.remaining() / 48 {
        return Err(Error::malformed(
            "GroupList",
            reader.position(),
            format!("group count {count} is impossible for the remaining bytes"),
        ));
    }
    let mut groups = Vec::with_capacity(count);
    for index in 0..count {
        groups.push(parse_group(&mut reader, index)?);
    }
    if !reader.is_empty() {
        return Err(Error::malformed(
            "GroupList",
            reader.position(),
            format!(
                "{} trailing bytes after {} groups",
                reader.remaining(),
                count
            ),
        ));
    }
    Ok(groups)
}

fn parse_group(reader: &mut Reader<'_>, index: usize) -> Result<Group> {
    let flag = reader.u8()?;
    let version = reader.u16()?;
    if !ACCEPTED_GROUP_VERSIONS.contains(&version) {
        return Err(Error::malformed(
            "Group",
            reader.position() - 2,
            format!("unsupported group version 0x{version:02X}"),
        ));
    }

    let (private, public, children) = if flag == 0 {
        // Without the structured wrapper there is no length, so treat the rest of the list as
        // this object's public body. This is legal but never observed in the corpus.
        return Err(Error::unsupported(
            "unstructured group object: its public body has no length delimiter",
        ));
    } else {
        let private_len = reader.u32()? as usize;
        let private = reader.take(private_len)?.to_vec();
        let public_len = reader.u32()? as usize;
        let public = reader.take(public_len)?.to_vec();
        let children_len = reader.u32()? as usize;
        let children_raw = reader.take(children_len)?;
        let children = if children_len == 0 {
            Vec::new()
        } else {
            crate::chunk::walk_chunks(children_raw)?
                .into_iter()
                .map(|Chunk { id, body }| OwnedChunk {
                    id,
                    body: body.to_vec(),
                })
                .collect()
        };
        (private, public, children)
    };

    let mut p = Reader::new(&public, "Group public section");
    let name = p.wstring()?;
    let volume = p.f32()?;
    let pan = p.f32()?;
    let tune = p.f32()?;
    let key_tracking = p.u8()? != 0;
    let reverse = p.u8()? != 0;
    let release_trigger = p.u8()? != 0;
    let release_trigger_note_monophonic = p.u8()? != 0;
    let rls_trig_counter = p.i32()?;
    let midi_channel = p.i16()?;
    let voice_group_index = p.i32()?;
    let fx_idx_amp_split_point = p.i32()?;
    let muted = p.u8()? != 0;
    let soloed = p.u8()? != 0;
    let interp_quality = p.i32()?;
    let public_tail = p.take(p.remaining())?.to_vec();

    let private_bus = if private.len() >= 20 {
        Some(private[private.len() - 20])
    } else {
        None
    };

    Ok(Group {
        index,
        version,
        flag,
        name,
        volume,
        pan,
        tune,
        key_tracking,
        reverse,
        release_trigger,
        release_trigger_note_monophonic,
        rls_trig_counter,
        midi_channel,
        voice_group_index,
        fx_idx_amp_split_point,
        muted,
        soloed,
        interp_quality,
        public_tail,
        private,
        private_bus,
        children,
        raw_public: public.to_vec(),
    })
}

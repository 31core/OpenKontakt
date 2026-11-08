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

impl Group {
    /// Round-robin position, when the `0x38` start-criteria record carries one.
    ///
    /// **This interpretation is inferred, not verified.** It comes from a single library
    /// (Pianobook's Soft Steinway), whose two groups are byte-identical in `0x38` except for one
    /// `u32` that reads 0 and 1 — matching the two recordings, and matching what a cycle needs. The field
    /// could be something else that merely correlates with it (a solo flag, a voice slot), so an
    /// instrument that sounds wrong after this change is more likely to be a wrong guess than a
    /// wrong engine. Cross-check with `ok-inspect`, which prints the cycles it believes in:
    ///
    /// ```text
    /// round robin (inferred): 0:RR1_tube@0  1:RR2_tube@1
    /// ```
    ///
    /// A record of one byte (`00`) means "no criteria" and never cycles, which is the common case.
    ///
    /// Library: <https://www.pianobook.co.uk/packs/soft-steinway/>.
    pub fn round_robin(&self) -> Option<(u32, Vec<u8>)> {
        let body = self
            .children
            .iter()
            .find(|child| child.id == 0x38)
            .map(|child| child.body.as_slice())?;
        if body.len() < 26 || body[0] != 1 {
            return None;
        }
        let position = u32::from_le_bytes(body.get(22..26)?.try_into().ok()?);
        // The position is what differs between the alternatives, so the cycle key is the record
        // with that word zeroed.
        let mut key = body.to_vec();
        key.get_mut(22..26)?.fill(0);
        Some((position, key))
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn group_with(record: Vec<u8>) -> Group {
        Group {
            index: 2,
            version: 0x95,
            flag: 0,
            name: "RR4".into(),
            volume: 1.0,
            pan: 0.0,
            tune: 1.0,
            key_tracking: true,
            reverse: false,
            release_trigger: false,
            release_trigger_note_monophonic: false,
            rls_trig_counter: 0,
            midi_channel: 0,
            voice_group_index: -1,
            fx_idx_amp_split_point: -1,
            muted: false,
            soloed: false,
            interp_quality: 0,
            public_tail: Vec::new(),
            private: Vec::new(),
            private_bus: None,
            children: vec![OwnedChunk {
                id: 0x38,
                body: record,
            }],
            raw_public: Vec::new(),
        }
    }

    #[test]
    fn round_robin_reads_the_position_word() {
        let mut record = vec![0u8; 26];
        record[0] = 1;
        record[22..26].copy_from_slice(&3u32.to_le_bytes());
        let (position, key) = group_with(record).round_robin().expect("a position word");
        assert_eq!(position, 3);
        assert_eq!(
            &key[22..26],
            &[0, 0, 0, 0],
            "the cycle key ignores the position"
        );

        assert!(
            group_with(vec![0]).round_robin().is_none(),
            "a one-byte record carries no criteria"
        );
    }
}

//! Program chunk (`0x28`) — instrument parameters and the root of the object tree.

use crate::chunk::{Chunk, OwnedChunk, walk_chunks};
use crate::error::{Error, Result};
use crate::reader::Reader;

#[derive(Clone, Debug, Default)]
pub struct ProgramParams {
    pub name: String,
    pub sample_bytes_total: f64,
    pub transpose: i8,
    pub volume: f32,
    pub pan: f32,
    pub tune: f32,
    pub low_velocity: u8,
    pub high_velocity: u8,
    pub low_key: u8,
    pub high_key: u8,
    pub default_keyswitch: i16,
    /// Preload buffer size in **bytes**, per sample — Kontakt's "DFD preload buffer size".
    ///
    /// Every observed library stores 61440 (60 KiB) except one piano library, which stores 245760
    /// (240 KiB). Reading this as a frame count over-allocates by the frame size (6× for 24-bit
    /// stereo).
    pub dfd_preload: i32,
    pub library_id: i32,
    pub fingerprint: u32,
    pub loading_flags: u32,
    pub group_solo: u8,
    pub category_icon: i32,
    pub credits: String,
    pub author: String,
    pub url: String,
    pub category_1: i16,
    pub category_2: i16,
    pub category_3: i16,
    pub suffix_bytes: usize,
    /// The complete version-specific suffix, preserved verbatim. Only the tokens needed for a
    /// particular version are interpreted; any undocumented remainder stays here.
    pub suffix: Vec<u8>,
    /// Terminal filename reference (`F2`, present from schema 0xA2). In every retail library
    /// checked it indexes the filename table's `other` list (v2) or flat list (v3) and points at
    /// the instrument wallpaper. Negative sentinels mean no wallpaper.
    pub wallpaper_filename: Option<i32>,
}

#[derive(Clone, Debug)]
pub struct Program {
    pub version: u16,
    pub flag: u8,
    pub private: Vec<u8>,
    pub public: Vec<u8>,
    pub params: ProgramParams,
    pub children: Vec<OwnedChunk>,
}

impl Program {
    pub fn children_of(&self, id: u16) -> impl Iterator<Item = &OwnedChunk> {
        self.children.iter().filter(move |chunk| chunk.id == id)
    }
}

const VERSIONS: &[u16] = &[
    0x80, 0x82, 0x90, 0x91, 0x92, 0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7, 0xA8, 0xA9, 0xAA,
    0xAB, 0xAC, 0xAD, 0xAE, 0xAF, 0xB0, 0xB1, 0xB2, 0xB3, 0xB4, 0xB5,
];

pub fn parse_program(body: &[u8]) -> Result<Program> {
    let mut reader = Reader::new(body, "Program");
    let flag = reader.u8()?;
    let version = reader.u16()?;
    if !VERSIONS.contains(&version) {
        return Err(Error::malformed(
            "Program",
            1,
            format!("schema version 0x{version:02X} is not in the accepted set"),
        ));
    }

    if flag == 0 {
        // `structured == 0` means public data only. Everything after the 3-byte header is public.
        let public = reader.take(reader.remaining())?.to_vec();
        let params = parse_public(&public, version)?;
        return Ok(Program {
            version,
            flag,
            private: Vec::new(),
            public,
            params,
            children: Vec::new(),
        });
    }

    let private_len = reader.u32()? as usize;
    let private = reader.take(private_len)?.to_vec();
    let public_len = reader.u32()? as usize;
    let public = reader.take(public_len)?.to_vec();
    let children_len = reader.u32()? as usize;
    let children_raw = reader.take(children_len)?;
    if !reader.is_empty() {
        return Err(Error::malformed(
            "Program",
            reader.position(),
            format!(
                "{} trailing bytes after Program sections",
                reader.remaining()
            ),
        ));
    }
    let params = parse_public(&public, version)?;
    let children = walk_chunks(children_raw)?
        .into_iter()
        .map(|Chunk { id, body }| OwnedChunk {
            id,
            body: body.to_vec(),
        })
        .collect();

    Ok(Program {
        version,
        flag,
        private,
        public,
        params,
        children,
    })
}

fn parse_public(public: &[u8], version: u16) -> Result<ProgramParams> {
    let mut reader = Reader::new(public, "Program public section");
    let mut params = ProgramParams {
        name: reader.wstring()?,
        sample_bytes_total: reader.f64()?,
        transpose: reader.i8()?,
        volume: reader.f32()?,
        pan: reader.f32()?,
        tune: reader.f32()?,
        low_velocity: reader.u8()?,
        high_velocity: reader.u8()?,
        low_key: reader.u8()?,
        high_key: reader.u8()?,
        default_keyswitch: reader.i16()?,
        dfd_preload: reader.i32()?,
        library_id: reader.i32()?,
        fingerprint: reader.u32()?,
        loading_flags: reader.u32()?,
        group_solo: reader.u8()?,
        category_icon: reader.i32()?,
        credits: reader.wstring()?,
        author: reader.wstring()?,
        url: reader.wstring()?,
        category_1: reader.i16()?,
        category_2: reader.i16()?,
        category_3: reader.i16()?,
        suffix_bytes: 0,
        suffix: Vec::new(),
        wallpaper_filename: None,
    };

    let suffix_start = reader.offset_in_slice();
    // The suffix table is known for the verified versions. For newer generations the stored
    // bytes outrun the table in `docs/03`; those bytes are preserved instead of guessed at.
    let _ = parse_suffix(&mut reader, version);
    // Keep the complete suffix. For B3+ the observed bytes outrun the published token table, so
    // the unconsumed remainder must not be dropped.
    params.suffix_bytes = public.len() - suffix_start;
    params.suffix = public[suffix_start..].to_vec();
    // F2 is the terminal signed filename reference for every schema that has it (>= 0xA2).
    // The token table in docs/03 places it last; this remains true for the newer 0xB3-0xB5
    // generations whose intermediate suffix bytes outrun that table.
    if version >= 0xA2 && params.suffix.len() >= 4 {
        let tail = &params.suffix[params.suffix.len() - 4..];
        let reference = i32::from_le_bytes(tail.try_into().unwrap());
        if reference >= 0 {
            params.wallpaper_filename = Some(reference);
        }
    }
    Ok(params)
}

/// Consume the version-specific suffix. Tokens are exactly the ones tabulated in
/// `docs/03-nki-instrument.md` §03.4. `S` is only accepted when absent; a present BNISoundData
/// body is not decoded in the docs yet.
fn parse_suffix(reader: &mut Reader<'_>, version: u16) -> Result<()> {
    let s0 = matches!(version, 0xB0..=0xB5);
    let s1 = matches!(version, 0xB1..=0xB5);
    let u0 = matches!(version, 0xAF..=0xB5);
    let u1 = matches!(version, 0xB2..=0xB5);
    let b0 = version >= 0xB3;
    let u2 = version >= 0xB4;
    let f0 = matches!(version, 0x91 | 0x92 | 0xA0..=0xB5);
    let f2 = matches!(version, 0xA2..=0xB5);
    let d0 = version == 0xA6;
    let d1 = version == 0xA6;
    let w0 = matches!(version, 0xA6..=0xB5);
    let w1 = matches!(version, 0xA8..=0xB5);
    let f1 = matches!(version, 0xA6..=0xB5);

    if f0 {
        let _ = reader.i32()?;
    }
    if d0 {
        let _ = reader.wstring()?;
    }
    if d1 {
        let _ = reader.wstring()?;
    }
    if w0 {
        let _ = reader.wstring()?;
    }
    if w1 {
        let _ = reader.wstring()?;
    }
    if u0 {
        let _ = reader.u32()?;
    }
    if s0 {
        let present = reader.u8()?;
        if present != 0 {
            return Err(Error::unsupported(
                "Program suffix contains a BNISoundData (S) record; its body is still undocumented",
            ));
        }
    }
    if version >= 0xB1 {
        let _ = reader.u8()?;
    }
    if s1 {
        let present = reader.u8()?;
        if present != 0 {
            return Err(Error::unsupported(
                "Program suffix contains a second BNISoundData (S1) record; its body is still undocumented",
            ));
        }
    }
    if u1 {
        let _ = reader.u32()?;
    }
    if b0 {
        let len = reader.u32()? as usize;
        let _ = reader.take(len)?;
    }
    if u2 {
        let _ = reader.u32()?;
    }
    if f1 {
        let _ = reader.i32()?;
    }
    if f2 {
        let _ = reader.i32()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepted_version_set_has_holes() {
        assert!(VERSIONS.contains(&0xAB));
        assert!(!VERSIONS.contains(&0x93));
        assert!(!VERSIONS.contains(&0xB6));
    }
}

//! The plain Kontakt chunk stream: `{ u16 id, u32 length, body }` repeated to EOF.

use crate::error::{Error, Result};
use crate::reader::Reader;

#[derive(Clone, Debug)]
pub struct OwnedChunk {
    pub id: u16,
    pub body: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct Chunk<'a> {
    pub id: u16,
    pub body: &'a [u8],
}

/// Walk a chunk stream and assert that it consumes `bytes` exactly.
pub fn walk_chunks(bytes: &[u8]) -> Result<Vec<Chunk<'_>>> {
    let mut reader = Reader::new(bytes, "chunk stream");
    let mut chunks = Vec::new();
    while !reader.is_empty() {
        if reader.remaining() < 6 {
            return Err(Error::malformed(
                "chunk stream",
                reader.position(),
                format!(
                    "{} trailing bytes cannot form a chunk header",
                    reader.remaining()
                ),
            ));
        }
        let id = reader.u16()?;
        let length = reader.u32()? as usize;
        if length > reader.remaining() {
            return Err(Error::malformed(
                "chunk stream",
                reader.position() - 4,
                format!(
                    "chunk 0x{id:04X} declares {length} bytes but only {} remain",
                    reader.remaining()
                ),
            ));
        }
        chunks.push(Chunk {
            id,
            body: reader.take(length)?,
        });
    }
    Ok(chunks)
}

pub fn chunk_name(id: u16) -> &'static str {
    match id {
        0x06 => "BParScript",
        0x28 => "Program",
        0x32 => "VoiceGroups",
        0x33 => "GroupList",
        0x34 => "ZoneList",
        0x35 => "ZoneSlices/PrivateRawObject",
        0x38 => "StartCriteria",
        0x39 => "LoopArray",
        0x3A => "BParameterArraySer<BParFX,8>",
        0x3B => "BParameterArraySer<BParInternalMod,16>",
        0x3C => "BParameterArraySer<BParExternalMod,32>",
        0x3D => "FileNameListPreK51",
        0x45 => "InsertBus",
        0x47 => "SaveSettings",
        0x4B => "FileNameTable",
        0x4E => "QuickBrowseData",
        _ => "Unknown",
    }
}

/// A structured object as used by many chunk bodies: `u8 flag`, `u16 version`, then either a
/// public-only body or the three counted sections.
#[derive(Clone, Debug)]
pub struct StructuredObject<'a> {
    pub flag: u8,
    pub version: u16,
    pub private: &'a [u8],
    pub public: &'a [u8],
    pub children: Vec<Chunk<'a>>,
}

pub fn read_structured_object<'a>(
    body: &'a [u8],
    layer: &'static str,
) -> Result<StructuredObject<'a>> {
    let mut reader = Reader::new(body, layer);
    let flag = reader.u8()?;
    let version = reader.u16()?;
    if flag == 0 {
        let public = reader.take(reader.remaining())?;
        return Ok(StructuredObject {
            flag,
            version,
            private: &[],
            public,
            children: Vec::new(),
        });
    }
    let private_len = reader.u32()? as usize;
    let private = reader.take(private_len)?;
    let public_len = reader.u32()? as usize;
    let public = reader.take(public_len)?;
    let children_len = reader.u32()? as usize;
    let children_raw = reader.take(children_len)?;
    if !reader.is_empty() {
        return Err(Error::malformed(
            layer,
            reader.position(),
            format!(
                "{} trailing bytes after structured object",
                reader.remaining()
            ),
        ));
    }
    let children = if children_len == 0 {
        Vec::new()
    } else {
        walk_chunks(children_raw)?
    };
    Ok(StructuredObject {
        flag,
        version,
        private,
        public,
        children,
    })
}

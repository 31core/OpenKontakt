//! `FileNameTable` (`0x4B`) versions 2 and 3, plus legacy `0x3D`.
//!
//! Paths are stored as sequences of typed segments. This module preserves every byte of the
//! registry (timestamps, auxiliary words, v3 envelopes, v2 trailing bytes) and only joins the
//! segments for display; normalisation happens in the resolver.

use crate::error::{Error, Result};
use crate::reader::Reader;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Segment {
    pub kind: u8,
    pub text: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct FileName {
    pub segments: Vec<Segment>,
    /// The exact bytes of this record, for future round-trip writing.
    pub raw: Vec<u8>,
}

impl FileName {
    /// Concatenate the segments with `/` following §08.3. Anchors are represented as empty
    /// components, so kind 11 produces a leading slash and kind 6 produces an empty component.
    pub fn display_path(&self) -> String {
        let mut parts = Vec::with_capacity(self.segments.len());
        for segment in &self.segments {
            match segment.kind {
                1 => parts.push(format!("{}:", segment.text.as_deref().unwrap_or(""))),
                2 | 4 | 5 | 8 | 9 => parts.push(segment.text.clone().unwrap_or_default()),
                3 => parts.push("..".to_string()),
                6 | 11 => parts.push(String::new()),
                _ => {}
            }
        }
        parts.join("/")
    }
}

#[derive(Clone, Debug, Default)]
pub struct FileNameTable {
    pub version: u16,
    pub special: Vec<FileName>,
    pub samples: Vec<FileName>,
    pub sample_timestamps: Vec<u64>,
    pub sample_aux: Vec<u32>,
    pub other: Vec<FileName>,
    pub flat: Option<Vec<FileName>>,
    pub v3_envelopes: Vec<(Vec<u8>, Vec<u8>)>,
    pub trailing: Vec<u8>,
    pub raw: Vec<u8>,
    pub legacy: bool,
}

impl FileNameTable {
    pub fn sample_count(&self) -> usize {
        self.flat
            .as_ref()
            .map(Vec::len)
            .unwrap_or(self.samples.len())
    }

    /// `filename_id` is an absolute index in v3 and an index into the sample list in v2.
    pub fn sample(&self, filename_id: i32) -> Option<&FileName> {
        if filename_id < 0 {
            return None;
        }
        let id = filename_id as usize;
        match &self.flat {
            Some(flat) => flat.get(id),
            None => self.samples.get(id),
        }
    }

    pub fn sample_path(&self, filename_id: i32) -> Option<String> {
        self.sample(filename_id).map(FileName::display_path)
    }

    /// The terminal Program filename reference addresses the v2 `other` list or the v3 flat
    /// namespace. It is used for the instrument wallpaper, not for zone samples.
    pub fn wallpaper(&self, reference: i32) -> Option<&FileName> {
        if reference < 0 {
            return None;
        }
        let index = reference as usize;
        match &self.flat {
            Some(flat) => flat.get(index),
            None => self.other.get(index),
        }
    }
}

pub fn parse_file_name_table(body: &[u8]) -> Result<FileNameTable> {
    if body.len() < 2 {
        return Err(Error::malformed(
            "FileNameTable",
            0,
            "body shorter than version",
        ));
    }
    let version = u16::from_le_bytes([body[0], body[1]]);
    match version {
        2 => parse_v2(body, false),
        3 => parse_v3(body),
        _ => Err(Error::malformed(
            "FileNameTable",
            0,
            format!("unsupported version {version} (only 2 and 3 are documented)"),
        )),
    }
}

pub fn parse_legacy_file_name_list(body: &[u8]) -> Result<FileNameTable> {
    parse_v2(body, true)
}

fn parse_v2(body: &[u8], legacy: bool) -> Result<FileNameTable> {
    let mut reader = Reader::new(body, "FileNameTable");
    let version = if legacy { 0 } else { reader.u16()? };
    let special = read_list(&mut reader, "FileNameTable special")?;
    let samples = read_list(&mut reader, "FileNameTable samples")?;
    let sample_timestamps = read_u64_array(&mut reader, samples.len(), "FileNameTable timestamps")?;
    let sample_aux = if legacy {
        Vec::new()
    } else {
        read_u32_array(&mut reader, samples.len(), "FileNameTable aux")?
    };
    let other = read_list(&mut reader, "FileNameTable other")?;
    let trailing = reader.take(reader.remaining())?.to_vec();
    Ok(FileNameTable {
        version,
        special,
        samples,
        sample_timestamps,
        sample_aux,
        other,
        flat: None,
        v3_envelopes: Vec::new(),
        trailing,
        raw: body.to_vec(),
        legacy,
    })
}

fn parse_v3(body: &[u8]) -> Result<FileNameTable> {
    let mut reader = Reader::new(body, "FileNameTable v3");
    let version = reader.u16()?;
    let count = reader.u32()? as usize;
    if count > reader.remaining() / 32 {
        return Err(Error::malformed(
            "FileNameTable v3",
            reader.position(),
            format!("record count {count} is impossible for the remaining bytes"),
        ));
    }
    let mut flat = Vec::with_capacity(count);
    let mut envelopes = Vec::with_capacity(count);
    for _ in 0..count {
        let header = reader.take(8)?.to_vec();
        let name = read_file_name(&mut reader, "FileNameTable v3")?;
        let metadata = reader.take(20)?.to_vec();
        flat.push(name);
        envelopes.push((header, metadata));
    }
    let trailing = reader.take(reader.remaining())?.to_vec();
    Ok(FileNameTable {
        version,
        special: Vec::new(),
        samples: Vec::new(),
        sample_timestamps: Vec::new(),
        sample_aux: Vec::new(),
        other: Vec::new(),
        flat: Some(flat),
        v3_envelopes: envelopes,
        trailing,
        raw: body.to_vec(),
        legacy: false,
    })
}

fn read_list(reader: &mut Reader<'_>, layer: &'static str) -> Result<Vec<FileName>> {
    let count = reader.u32()? as usize;
    if count > reader.remaining() / 4 {
        return Err(Error::malformed(
            layer,
            reader.position(),
            format!("count {count} is impossible for the remaining bytes"),
        ));
    }
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        out.push(read_file_name(reader, layer)?);
    }
    Ok(out)
}

fn read_file_name(reader: &mut Reader<'_>, layer: &'static str) -> Result<FileName> {
    let start = reader.offset_in_slice();
    let count = reader.i32()?;
    if count < 0 {
        return Err(Error::malformed(
            layer,
            reader.position() - 4,
            format!("negative segment count {count}"),
        ));
    }
    let count = count as usize;
    if count > reader.remaining() {
        return Err(Error::malformed(
            layer,
            reader.position(),
            format!("segment count {count} exceeds remaining bytes"),
        ));
    }
    let mut segments = Vec::with_capacity(count);
    for _ in 0..count {
        let kind = reader.u8()?;
        let text = match kind {
            1 | 2 | 4 | 5 | 8 | 9 => Some(reader.wstring()?),
            3 | 6 | 11 => None,
            other => {
                return Err(Error::malformed(
                    layer,
                    reader.position() - 1,
                    format!("unknown filename segment kind {other}"),
                ));
            }
        };
        segments.push(Segment { kind, text });
    }
    let raw = reader.slice_from(start)?.to_vec();
    Ok(FileName { segments, raw })
}

fn read_u64_array(reader: &mut Reader<'_>, count: usize, layer: &'static str) -> Result<Vec<u64>> {
    if count > reader.remaining() / 8 {
        return Err(Error::malformed(layer, reader.position(), "array overruns"));
    }
    (0..count).map(|_| reader.u64()).collect()
}

fn read_u32_array(reader: &mut Reader<'_>, count: usize, layer: &'static str) -> Result<Vec<u32>> {
    if count > reader.remaining() / 4 {
        return Err(Error::malformed(layer, reader.position(), "array overruns"));
    }
    (0..count).map(|_| reader.u32()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(kind: u8, text: Option<&str>) -> Segment {
        Segment {
            kind,
            text: text.map(str::to_string),
        }
    }

    #[test]
    fn joins_anchors_and_components() {
        let path = FileName {
            segments: vec![name(2, Some("Samples")), name(4, Some("x.wav"))],
            raw: Vec::new(),
        };
        assert_eq!(path.display_path(), "Samples/x.wav");

        let root = FileName {
            segments: vec![
                name(11, None),
                name(2, Some("Samples")),
                name(2, Some("x.wav")),
            ],
            raw: Vec::new(),
        };
        assert_eq!(root.display_path(), "/Samples/x.wav");

        let drive = FileName {
            segments: vec![name(1, Some("C")), name(2, Some("Samples"))],
            raw: Vec::new(),
        };
        assert_eq!(drive.display_path(), "C:/Samples");
    }
}

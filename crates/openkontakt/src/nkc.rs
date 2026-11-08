//! `.nkc` — the catalogue for a packed library.
//!
//! A packed library keeps its samples inside `.nkx`/`.nkr` containers instead of loose `.wav`
//! files. The `.nkc` is the index that says which name lives where: a root node followed by a flat
//! pre-order list of entries, each carrying a UTF-16 name and the absolute offset of the matching
//! record in the payload container.
//!
//! Layout (all little-endian, entries packed and unaligned):
//!
//! ```text
//! header   0x30 bytes   magic 0x3FE1107A, version 0x110
//! root     kind u16, sub u16, version u32, childCount u32, nameLen u32, name, tail
//! entries  nameLen u32, name, tail        (repeated to EOF)
//! tail     offset u32, size u32, kind u16, flags u16, 0 u32, 0 u32   (20 bytes)
//! ```
//!
//! `size` is the payload length for samples and zero for folders and pictures. The root's
//! `childCount` counts immediate children only — the flat list that follows runs to EOF.

use crate::error::{Error, Result};

/// `0x3FE1107A` in every observed file.
const NKC_MAGIC: u32 = 0x3FE1_107A;
const HEADER_LEN: usize = 0x30;
/// Every node and entry ends with this many bytes.
const TAIL_LEN: usize = 20;

/// One indexed name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NkcEntry {
    pub name: String,
    /// Absolute byte offset of the record inside the payload container.
    pub offset: u64,
    /// Payload length, when the file records one (samples only).
    pub size: Option<u64>,
    /// Entry kind: `4` for a data leaf in every observed file. Not relied on.
    pub kind: u32,
}

impl NkcEntry {
    /// True for a sample payload as opposed to artwork or a folder.
    pub fn is_sample(&self) -> bool {
        self.name.to_ascii_lowercase().ends_with(".ncw")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NkcIndex {
    pub version: u32,
    pub root_name: String,
    /// The root's `sub` word: `0x4E69`/`0x4E6A` in older files, `0x0014` in newer ones.
    pub sub: u16,
    pub entries: Vec<NkcEntry>,
}

impl NkcIndex {
    /// Find an entry by name, case-insensitively.
    pub fn get(&self, name: &str) -> Option<&NkcEntry> {
        self.entries
            .iter()
            .find(|entry| entry.name.eq_ignore_ascii_case(name))
    }

    /// Every sample entry, in file order.
    pub fn samples(&self) -> impl Iterator<Item = &NkcEntry> {
        self.entries.iter().filter(|entry| entry.is_sample())
    }
}

/// Parse a `.nkc` catalogue.
pub fn parse_nkc(bytes: &[u8]) -> Result<NkcIndex> {
    if bytes.len() < HEADER_LEN {
        return Err(Error::malformed(
            "NKC header",
            0,
            "file shorter than 48 bytes",
        ));
    }
    let magic = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    if magic != NKC_MAGIC {
        return Err(Error::malformed(
            "NKC header",
            0,
            format!("bad magic 0x{magic:08X}"),
        ));
    }
    let version = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    // `+0x28` reads 0x32 in every observed file, which is not where the root node starts (the
    // root sits at 0x30 and reads cleanly there), so the field is only sanity-checked.
    let body_offset =
        u32::from_le_bytes([bytes[0x28], bytes[0x29], bytes[0x2A], bytes[0x2B]]) as usize;
    if body_offset > bytes.len() {
        return Err(Error::malformed(
            "NKC header",
            0x28,
            format!("body offset {body_offset} past the end of the file"),
        ));
    }

    let sub = u16::from_le_bytes([bytes[0x32], bytes[0x33]]);
    // The root's name length sits after kind, sub, version and childCount.
    let root_name = read_name(bytes, HEADER_LEN + 12)?;
    let after_root = HEADER_LEN + 16 + root_name.len() * 2 + TAIL_LEN;
    let entries = read_entries(bytes, after_root)?;

    Ok(NkcIndex {
        version,
        root_name,
        sub,
        entries,
    })
}

/// Read the `u32` length-prefixed UTF-16 name of a node or entry at `at`.
fn read_name(bytes: &[u8], at: usize) -> Result<String> {
    if at + 4 > bytes.len() {
        return Err(Error::malformed("NKC name", at, "truncated name length"));
    }
    let name_len =
        u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]) as usize;
    if name_len == 0 || name_len > 4096 {
        return Err(Error::malformed(
            "NKC name",
            at,
            format!("implausible name length {name_len}"),
        ));
    }
    let start = at + 4;
    let end = start + name_len * 2;
    if end > bytes.len() {
        return Err(Error::malformed("NKC name", at, "name runs past the file"));
    }
    let units: Vec<u16> = bytes[start..end]
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    Ok(String::from_utf16_lossy(&units))
}

/// Read one 20-byte tail: `offset, size, kind u16, flags u16, 0, 0`.
fn read_tail(bytes: &[u8], at: usize) -> (u64, Option<u64>, u32) {
    let word = |index: usize| -> u32 {
        u32::from_le_bytes([
            bytes[at + index],
            bytes[at + index + 1],
            bytes[at + index + 2],
            bytes[at + index + 3],
        ])
    };
    let size = word(4);
    let kind = u16::from_le_bytes([bytes[at + 8], bytes[at + 9]]) as u32;
    (word(0) as u64, (size > 0).then_some(size as u64), kind)
}

/// Walk the flat entry list; it runs to the end of the file.
fn read_entries(bytes: &[u8], mut at: usize) -> Result<Vec<NkcEntry>> {
    let mut entries = Vec::new();
    while at < bytes.len() {
        let name = read_name(bytes, at)?;
        let tail_at = at + 4 + name.len() * 2;
        if tail_at + TAIL_LEN > bytes.len() {
            // 8Dio's catalogue ends with a name and no tail: 20 bytes are missing from the file,
            // but the name list is intact. Keep the entry so lookups by name still work; it just
            // has no offset.
            if tail_at == bytes.len() {
                entries.push(NkcEntry {
                    name,
                    offset: 0,
                    size: None,
                    kind: 0,
                });
                break;
            }
            return Err(Error::malformed(
                "NKC entry",
                at,
                "entry runs past the file",
            ));
        }
        let (offset, size, kind) = read_tail(bytes, tail_at);
        entries.push(NkcEntry {
            name,
            offset,
            size,
            kind,
        });
        at = tail_at + TAIL_LEN;
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_name(out: &mut Vec<u8>, name: &str) {
        let units: Vec<u16> = name.encode_utf16().collect();
        out.extend_from_slice(&(units.len() as u32).to_le_bytes());
        for unit in &units {
            out.extend_from_slice(&unit.to_le_bytes());
        }
    }

    fn push_tail(out: &mut Vec<u8>, offset: u32, size: u32, kind: u16) {
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&0xFC83u16.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
    }

    /// A catalogue shaped like a retail one, so the parser can be exercised without the real file.
    fn build(entries: &[(&str, u32, u32)]) -> Vec<u8> {
        let mut out = vec![0u8; HEADER_LEN];
        out[0..4].copy_from_slice(&NKC_MAGIC.to_le_bytes());
        out[4..8].copy_from_slice(&0x110u32.to_le_bytes());
        out[0x28..0x2C].copy_from_slice(&(HEADER_LEN as u32 + 2).to_le_bytes());
        // Root node.
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&0x0014u16.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        push_name(&mut out, "Resources");
        push_tail(&mut out, 0, 0, 1);
        for (name, offset, size) in entries {
            push_name(&mut out, name);
            push_tail(&mut out, *offset, *size, 4);
        }
        out
    }

    #[test]
    fn reads_a_catalogue() {
        let bytes = build(&[("a.ncw", 0x1234, 0x400), ("b.PNG", 0x5678, 0)]);
        let index = parse_nkc(&bytes).expect("parses");
        assert_eq!(index.root_name, "Resources");
        assert_eq!(index.version, 0x110);
        assert_eq!(index.entries.len(), 2);
        assert_eq!(index.entries[0].name, "a.ncw");
        assert_eq!(index.entries[0].offset, 0x1234);
        assert_eq!(index.entries[0].size, Some(0x400));
        assert!(index.entries[0].is_sample());
        assert_eq!(index.entries[1].size, None, "no size for artwork");
        assert_eq!(index.samples().count(), 1);
    }

    #[test]
    fn rejects_a_foreign_file() {
        assert!(parse_nkc(&vec![0u8; 0x40]).is_err());
    }
}

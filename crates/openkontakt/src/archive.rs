//! Older libraries keep their UI artwork inside a resource archive (`.nkr`) instead of loose
//! files under `Resources/pictures/`. This module reads that archive's *file table*, which is
//! enough to get the picture sprite sheets and their `.txt` sidecars back.
//!
//! Two independent structures have to be read:
//!
//! * the **directory**, a flat prefix of UTF-16LE names in file order, and
//! * the **payload blocks**, each introduced by a 22-byte header and stored back to back in the
//!   same order as the directory.
//!
//! The directory also carries a `u32` next to every name. It is *not* a payload offset — it does
//! not match the block positions in a retail archive — so the pairing below never uses it and
//! relies on the two lists sharing an order, which they do.

/// One file recovered from a resource archive.
#[derive(Clone, Debug)]
pub struct ArchiveEntry {
    pub name: String,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, Default)]
pub struct ResourceArchive {
    pub entries: Vec<ArchiveEntry>,
}

/// Header that introduces every stored payload: magic, a version word, a flags word, then a
/// 16-bit gap and the payload length.
const BLOCK_MAGIC: [u8; 4] = [0xfa, 0x05, 0xe9, 0x2a];
const BLOCK_VERSION: [u8; 4] = [0x10, 0x01, 0x00, 0x00];
const BLOCK_FLAGS: [u8; 4] = [0x00, 0x00, 0xff, 0x00];
const BLOCK_HEADER: usize = 22;
/// Payload length lives at this unaligned offset inside the block header.
const BLOCK_LENGTH_AT: usize = 14;

impl ResourceArchive {
    /// Find a file by name, case-insensitively.
    pub fn get(&self, name: &str) -> Option<&ArchiveEntry> {
        self.entries
            .iter()
            .find(|entry| entry.name.eq_ignore_ascii_case(name))
    }

    /// Every picture sprite sheet plus the `.txt` sidecar stored next to it, in archive order.
    pub fn pictures(&self) -> Vec<PictureAsset> {
        let mut assets = Vec::new();
        for entry in &self.entries {
            let Some((stem, extension)) = split_extension(&entry.name) else {
                continue;
            };
            if !is_picture_extension(extension) {
                continue;
            }
            // An archive that obfuscates its payloads must not hand ciphertext back as an image.
            if !looks_like_image(&entry.data) {
                continue;
            }
            let sidecar = format!("{stem}.txt");
            let text = self
                .get(&sidecar)
                .and_then(|entry| String::from_utf8(entry.data.clone()).ok());
            assets.push(PictureAsset {
                stem: stem.to_string(),
                image: entry.data.clone(),
                text,
            });
        }
        assets
    }
}

/// A picture resource: the sprite sheet bytes plus the optional `.txt` layout sidecar.
#[derive(Clone, Debug)]
pub struct PictureAsset {
    pub stem: String,
    pub image: Vec<u8>,
    pub text: Option<String>,
}

fn split_extension(name: &str) -> Option<(&str, &str)> {
    let dot = name.rfind('.')?;
    if dot == 0 {
        return None;
    }
    Some((&name[..dot], &name[dot + 1..]))
}

/// True when the bytes start with an image signature a decoder can actually open.
fn looks_like_image(data: &[u8]) -> bool {
    data.starts_with(b"\x89PNG\r\n\x1a\n")
        || data.starts_with(b"\xff\xd8\xff")
        || data.starts_with(b"BM")
}

fn is_picture_extension(extension: &str) -> bool {
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "png" | "jpg" | "jpeg" | "bmp" | "tga"
    )
}

/// Parse a resource archive image.
pub fn parse_resource_archive(bytes: &[u8]) -> ResourceArchive {
    let names = scan_names(bytes);
    let blocks = scan_blocks(bytes);
    let mut entries = Vec::with_capacity(names.len().min(blocks.len()));
    for (name, (offset, length)) in names.into_iter().zip(blocks) {
        let end = offset + length;
        if end > bytes.len() {
            continue;
        }
        entries.push(ArchiveEntry {
            name,
            data: bytes[offset..end].to_vec(),
        });
    }
    ResourceArchive { entries }
}

/// Collect the file names at the head of the archive. They are the only UTF-16LE runs in the
/// directory, which ends where the first payload block begins, so the scan is bounded there to
/// keep compressed payload bytes from being mistaken for names.
fn scan_names(bytes: &[u8]) -> Vec<String> {
    let end = find_block(bytes, 0).unwrap_or(bytes.len()).min(bytes.len());
    let mut names = Vec::new();
    let mut cursor = 0usize;
    while cursor + 4 <= end {
        let mut offset = cursor;
        let mut text = String::new();
        loop {
            if offset + 2 > end {
                break;
            }
            let low = bytes[offset];
            let high = bytes[offset + 1];
            if high != 0 || !(0x20..0x7f).contains(&low) {
                break;
            }
            text.push(low as char);
            offset += 2;
        }
        // A real name is terminated by a UTF-16 NUL and has an extension.
        let terminated = offset + 2 <= end && bytes[offset] == 0 && bytes[offset + 1] == 0;
        if text.len() >= 5 && terminated && text.contains('.') && !text.starts_with('.') {
            names.push(text);
            cursor = offset + 2;
        } else {
            cursor += 1;
        }
    }
    names
}

/// Walk the payload blocks: each one is `BLOCK_HEADER` bytes followed by its payload.
fn scan_blocks(bytes: &[u8]) -> Vec<(usize, usize)> {
    let mut blocks = Vec::new();
    let mut cursor = 0usize;
    while let Some(offset) = find_block(bytes, cursor) {
        let length_at = offset + BLOCK_LENGTH_AT;
        let Some(length_bytes) = bytes.get(length_at..length_at + 4) else {
            break;
        };
        let length = u32::from_le_bytes([
            length_bytes[0],
            length_bytes[1],
            length_bytes[2],
            length_bytes[3],
        ]) as usize;
        let payload = offset + BLOCK_HEADER;
        if length == 0 || payload + length > bytes.len() {
            break;
        }
        blocks.push((payload, length));
        cursor = payload + length;
    }
    blocks
}

fn find_block(bytes: &[u8], from: usize) -> Option<usize> {
    let mut offset = from;
    while offset + BLOCK_HEADER <= bytes.len() {
        let header = &bytes[offset..offset + BLOCK_HEADER];
        if header[0..4] == BLOCK_MAGIC
            && header[4..8] == BLOCK_VERSION
            && header[8..12] == BLOCK_FLAGS
        {
            return Some(offset);
        }
        offset += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build one payload block so the walker can be exercised without a retail archive.
    fn block(payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&BLOCK_MAGIC);
        out.extend_from_slice(&BLOCK_VERSION);
        out.extend_from_slice(&BLOCK_FLAGS);
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&[0, 0, 0, 0]);
        debug_assert_eq!(out.len(), BLOCK_HEADER);
        out.extend_from_slice(payload);
        out
    }

    fn name_bytes(name: &str) -> Vec<u8> {
        let mut out = Vec::new();
        for ch in name.chars() {
            out.push(ch as u8);
            out.push(0);
        }
        out.extend_from_slice(&[0, 0]);
        out
    }

    #[test]
    fn reads_names_and_blocks_in_order() {
        let mut archive = Vec::new();
        archive.extend_from_slice(&name_bytes("pictures"));
        archive.extend_from_slice(&name_bytes("Knob.txt"));
        archive.extend_from_slice(&name_bytes("Knob.PNG"));
        archive.extend_from_slice(&block(
            b"Number of Animations: 4\nHorizontal Animation: no\n",
        ));
        archive.extend_from_slice(&block(b"\x89PNG\r\n\x1a\nnot really"));
        let parsed = parse_resource_archive(&archive);
        assert_eq!(parsed.entries.len(), 2);
        assert_eq!(parsed.entries[0].name, "Knob.txt");
        assert_eq!(parsed.entries[1].name, "Knob.PNG");
        assert_eq!(parsed.entries[1].data.len(), 18);
        let pictures = parsed.pictures();
        assert_eq!(pictures.len(), 1);
        assert_eq!(pictures[0].stem, "Knob");
        assert!(
            pictures[0]
                .text
                .as_deref()
                .unwrap()
                .contains("Animations: 4")
        );
    }

    #[test]
    fn a_truncated_block_is_dropped_instead_of_panicking() {
        let mut archive = name_bytes("A.PNG");
        archive.extend_from_slice(&block(b"0123456789"));
        archive.truncate(archive.len() - 4);
        let parsed = parse_resource_archive(&archive);
        assert!(parsed.entries.is_empty());
    }

    #[test]
    fn a_random_buffer_yields_nothing() {
        let noise: Vec<u8> = (0..4096u32)
            .map(|value| (value.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        assert!(parse_resource_archive(&noise).entries.is_empty());
    }
}

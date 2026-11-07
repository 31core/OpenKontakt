//! The NISound item tree container.
//!
//! Implements `docs/01-container-layers.md` and `docs/13-item-framing.md`. The chunk stream is
//! located through `BNISoundPreset -> EncryptionItem -> SubtreeItem -> PresetChunkItem`, with the
//! subtree payload expanded using FastLZ.

use crate::error::{Error, Result};
use crate::fastlz;
use crate::reader::Reader;

pub const ITEM_HEADER_SIZE: u64 = 40;
pub const ITEM_DATA_HEADER_SIZE: u64 = 20;

#[derive(Clone, Debug)]
pub struct ItemData {
    pub offset: usize,
    pub length: u64,
    pub domain: [u8; 4],
    pub domain_name: String,
    pub item_id: u32,
    pub version: u32,
    /// The property bytes owned by this layer, after any wrapped inner layer.
    pub props: Vec<u8>,
    pub inner: Option<Box<ItemData>>,
}

impl ItemData {
    pub fn is_nisd(&self) -> bool {
        self.domain_name == "NISD"
    }

    pub fn is_leaf_item(&self) -> bool {
        self.is_nisd() && self.item_id == 0x01
    }

    pub fn kind_name(&self) -> String {
        match (self.domain_name.as_str(), self.item_id) {
            ("NISD", 0x01) => "Item".into(),
            ("NISD", 0x65) => "Preset".into(),
            ("NISD", 0x6A) => "Authorization".into(),
            ("NISD", 0x6C) => "SoundInfoItem".into(),
            ("NISD", 0x6D) => "PresetChunkItem".into(),
            ("NISD", 0x6E) => "ExternalFileReference".into(),
            ("NISD", 0x72) => "PictureItem".into(),
            ("NISD", 0x73) => "SubtreeItem".into(),
            ("NISD", 0x74) => "EncryptionItem".into(),
            ("NISD", 0x75) => "AppSpecific".into(),
            ("NISD", 0x76) => "RepositoryRoot".into(),
            ("NISD", 0x78) => "AutomationParameters".into(),
            ("NISD", 0x79) => "ControllerAssignments".into(),
            ("NIK4", 0x03) => "BNISoundPreset".into(),
            ("NIK4", 0x04) => "BNISoundHeader".into(),
            (domain, id) => format!("{domain}/0x{id:04X}"),
        }
    }

    pub fn chain(&self) -> Vec<&ItemData> {
        let mut out = Vec::new();
        let mut current = Some(self);
        while let Some(layer) = current {
            out.push(layer);
            current = layer.inner.as_deref();
        }
        out
    }

    pub fn find_layer(&self, name: &str) -> Option<&ItemData> {
        self.chain()
            .into_iter()
            .find(|layer| layer.kind_name() == name)
    }
}

#[derive(Clone, Debug)]
pub struct NisItem {
    pub offset: usize,
    pub length: u64,
    pub version: u32,
    pub flags: u32,
    pub reserved: u32,
    pub uuid: [u8; 16],
    pub data: ItemData,
    pub children: Vec<NisItem>,
    pub trailing: Vec<u8>,
}

impl NisItem {
    pub fn walk<'a>(&'a self, out: &mut Vec<&'a NisItem>) {
        out.push(self);
        for child in &self.children {
            child.walk(out);
        }
    }

    pub fn all_items(&self) -> Vec<&NisItem> {
        let mut out = Vec::new();
        self.walk(&mut out);
        out
    }

    pub fn find_by_name(&self, name: &str) -> Option<&NisItem> {
        self.all_items()
            .into_iter()
            .find(|item| item.data.kind_name() == name)
    }

    pub fn children_by_name<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a NisItem> {
        self.children
            .iter()
            .filter(move |item| item.data.kind_name() == name)
    }
}

#[derive(Clone, Debug, Default)]
pub struct ContainerInfo {
    pub file_bytes: usize,
    pub root_item_length: u64,
    pub container: &'static str,
    pub encrypted: bool,
    pub subtree_compressed: bool,
    pub subtree_expanded: Option<u32>,
    pub subtree_compressed_size: Option<u32>,
    pub preset_children: Vec<String>,
    pub preset_data_layers: Vec<String>,
    /// Bytes after the declared chunk stream inside PresetChunkItem. Observed as 8 bytes in
    /// retail files; their meaning is undocumented and they are preserved verbatim.
    pub preset_chunk_trailing: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct Unwrapped {
    pub chunk_stream: Vec<u8>,
    pub info: ContainerInfo,
}

/// Unwrap a `.nki` file to its plain Kontakt chunk stream.
pub fn unwrap_nki(bytes: &[u8]) -> Result<Unwrapped> {
    if bytes.len() >= 16 && &bytes[12..16] == b"hsin" {
        return unwrap_nis(bytes);
    }

    // The legacy NKS container starts with BPatchHeaderV42. Its 3.7-era layout is not specified
    // precisely enough in the docs, so refuse rather than guessing at the decompressed length.
    if bytes.len() >= 4 && u32::from_le_bytes(bytes[0..4].try_into().unwrap()) == 0x7FA8_9012 {
        unimplemented!(
            "legacy NKS (.nki Kontakt 1-4) containers: the BPatchHeaderV42 decompressed-length \
             offset is not documented yet; only NISound .nki + loose WAV is supported"
        );
    }

    Err(Error::NotAnInstrument(
        "no \"hsin\" item header at offset 12 and no BPatchHeaderV42 signature".into(),
    ))
}

fn unwrap_nis(bytes: &[u8]) -> Result<Unwrapped> {
    let mut reader = Reader::new(bytes, "NIS document");
    let root = parse_item(&mut reader)?;
    if reader.remaining() != 0 {
        return Err(Error::malformed(
            "NIS document",
            reader.position(),
            format!("{} trailing bytes after root item", reader.remaining()),
        ));
    }
    if root.length as usize != bytes.len() {
        return Err(Error::malformed(
            "NIS document",
            0,
            format!(
                "root item length {} does not match file size {}",
                root.length,
                bytes.len()
            ),
        ));
    }

    let preset = root
        .find_by_name("BNISoundPreset")
        .ok_or_else(|| Error::malformed("NIS document", 0, "no BNISoundPreset item"))?;

    let encryption = preset
        .children
        .iter()
        .find(|item| item.data.kind_name() == "EncryptionItem")
        .ok_or_else(|| Error::malformed("NIS document", preset.offset, "no EncryptionItem"))?;

    let mut info = ContainerInfo {
        file_bytes: bytes.len(),
        root_item_length: root.length,
        container: "NISound",
        encrypted: false,
        subtree_compressed: false,
        subtree_expanded: None,
        subtree_compressed_size: None,
        preset_children: preset
            .children
            .iter()
            .map(|item| item.data.kind_name())
            .collect(),
        preset_data_layers: preset.data.chain().iter().map(|d| d.kind_name()).collect(),
        preset_chunk_trailing: Vec::new(),
    };

    // EncryptionItem's own property bytes: { u32 version = 1, u8 encrypted }.
    let props = &encryption.data.props;
    if props.len() < 5 {
        return Err(Error::malformed(
            "EncryptionItem",
            encryption.offset,
            format!("properties are {} bytes, need 5", props.len()),
        ));
    }
    let enc_version = u32::from_le_bytes(props[0..4].try_into().unwrap());
    if enc_version != 1 {
        return Err(Error::malformed(
            "EncryptionItem",
            encryption.offset,
            format!("unsupported version {enc_version}"),
        ));
    }
    info.encrypted = props[4] != 0;
    if info.encrypted {
        return Err(Error::EncryptedPreset);
    }

    // The SubtreeItem is an inner ItemData layer of the EncryptionItem, not a child.
    let subtree = encryption.data.find_layer("SubtreeItem").ok_or_else(|| {
        Error::malformed("EncryptionItem", encryption.offset, "no SubtreeItem layer")
    })?;
    let expanded = parse_subtree(&subtree.props, &mut info)?;

    let mut nested_reader = Reader::new(&expanded, "expanded subtree item");
    let nested_item = parse_item(&mut nested_reader)?;
    if nested_reader.remaining() != 0 {
        return Err(Error::malformed(
            "expanded subtree",
            nested_reader.position(),
            format!("{} trailing bytes", nested_reader.remaining()),
        ));
    }
    let chunk_item = nested_item.find_by_name("PresetChunkItem").ok_or_else(|| {
        Error::malformed(
            "expanded subtree",
            nested_item.offset,
            "no PresetChunkItem item",
        )
    })?;

    let (chunk_stream, chunk_trailing) =
        parse_preset_chunk_item(&chunk_item.data.props, chunk_item.offset)?;
    info.preset_chunk_trailing = chunk_trailing;
    Ok(Unwrapped { chunk_stream, info })
}

fn parse_item(reader: &mut Reader<'_>) -> Result<NisItem> {
    let offset = reader.position();
    let length = reader.u64()?;
    if length < ITEM_HEADER_SIZE + ITEM_DATA_HEADER_SIZE {
        return Err(Error::malformed(
            "ItemHeader",
            offset,
            format!("implausible item length {length}"),
        ));
    }
    let version = reader.u32()?;
    if version != 1 {
        return Err(Error::malformed(
            "ItemHeader",
            offset,
            format!("unsupported item header version {version}"),
        ));
    }
    let magic = reader.take(4)?;
    if magic != b"hsin" {
        return Err(Error::malformed(
            "ItemHeader",
            offset + 12,
            format!("bad magic {magic:?}, expected \"hsin\""),
        ));
    }
    let flags = reader.u32()?;
    let reserved = reader.u32()?;
    let mut uuid = [0u8; 16];
    uuid.copy_from_slice(reader.take(16)?);

    let body_len = usize::try_from(length - ITEM_HEADER_SIZE)
        .map_err(|_| Error::malformed("ItemHeader", offset, "item too large"))?;
    let body = reader.take(body_len)?;
    let mut body_reader = Reader::with_base(body, offset + ITEM_HEADER_SIZE as usize, "ItemData");
    let data = parse_item_data(&mut body_reader)?;

    let mut children = Vec::new();
    let mut trailing = Vec::new();
    if body_reader.remaining() >= 8 {
        let table_start = body_reader.position();
        let table_version = u32::from_le_bytes(body_reader.peek(4)?.try_into().unwrap());
        if table_version == 1 {
            body_reader.skip(4)?;
            let count = body_reader.u32()? as usize;
            // Every child needs at least a 12-byte row and a 48-byte empty item. Reject absurd
            // counts before allocating.
            let max_possible = body_reader.remaining() / (12 + 48);
            if count > max_possible {
                return Err(Error::malformed(
                    "child table",
                    table_start,
                    format!("child count {count} exceeds remaining bytes (max {max_possible})"),
                ));
            }
            for _ in 0..count {
                body_reader.skip(12)?;
                children.push(parse_item(&mut body_reader)?);
            }
        } else {
            // Unknown table generations are preserved verbatim instead of guessed at.
            trailing = body_reader.take(body_reader.remaining())?.to_vec();
        }
    }
    if !body_reader.is_empty() {
        trailing = body_reader.take(body_reader.remaining())?.to_vec();
    }

    Ok(NisItem {
        offset,
        length,
        version,
        flags,
        reserved,
        uuid,
        data,
        children,
        trailing,
    })
}

fn parse_item_data(reader: &mut Reader<'_>) -> Result<ItemData> {
    let offset = reader.position();
    let length = reader.u64()?;
    if length < ITEM_DATA_HEADER_SIZE {
        return Err(Error::malformed(
            "ItemData",
            offset,
            format!("implausible layer length {length}"),
        ));
    }
    let mut raw_domain = [0u8; 4];
    raw_domain.copy_from_slice(reader.take(4)?);
    let domain = [raw_domain[3], raw_domain[2], raw_domain[1], raw_domain[0]];
    let domain_name = String::from_utf8_lossy(&domain).into_owned();
    let item_id = reader.u32()?;
    let version = reader.u32()?;
    if version != 1 {
        return Err(Error::malformed(
            "ItemData",
            offset,
            format!("unsupported ItemData version {version}"),
        ));
    }
    let body_len = usize::try_from(length - ITEM_DATA_HEADER_SIZE)
        .map_err(|_| Error::malformed("ItemData", offset, "layer too large"))?;
    let body = reader.take(body_len)?;

    let is_leaf = domain_name == "NISD" && item_id == 0x01;
    if is_leaf {
        return Ok(ItemData {
            offset,
            length,
            domain,
            domain_name,
            item_id,
            version,
            props: body.to_vec(),
            inner: None,
        });
    }

    let mut body_reader =
        Reader::with_base(body, offset + ITEM_DATA_HEADER_SIZE as usize, "ItemData");
    let inner = parse_item_data(&mut body_reader)?;
    let props_start = body_reader.offset_in_slice();
    Ok(ItemData {
        offset,
        length,
        domain,
        domain_name,
        item_id,
        version,
        props: body[props_start..].to_vec(),
        inner: Some(Box::new(inner)),
    })
}

fn parse_subtree(props: &[u8], info: &mut ContainerInfo) -> Result<Vec<u8>> {
    let mut reader = Reader::new(props, "SubtreeItem");
    let version = reader.u32()?;
    if version != 1 {
        return Err(Error::malformed(
            "SubtreeItem",
            0,
            format!("unsupported version {version}"),
        ));
    }
    let compressed = reader.u8()?;
    info.subtree_compressed = compressed != 0;
    if compressed == 0 {
        let size = reader.u64()?;
        let size = usize::try_from(size)
            .map_err(|_| Error::malformed("SubtreeItem", 5, "subtree too large"))?;
        return Ok(reader.take(size)?.to_vec());
    }

    let expanded_size = reader.u32()? as usize;
    let compressed_size = reader.u32()? as usize;
    info.subtree_expanded = Some(expanded_size as u32);
    info.subtree_compressed_size = Some(compressed_size as u32);
    let compressed_bytes = reader.take(compressed_size)?;
    let out = fastlz::decompress(compressed_bytes, expanded_size)?;
    if out.len() != expanded_size {
        return Err(Error::FastLz {
            expected: expanded_size,
            produced: out.len(),
            reason: "expanded size does not match the declared expanded_size".into(),
        });
    }
    Ok(out)
}

fn parse_preset_chunk_item(props: &[u8], offset: usize) -> Result<(Vec<u8>, Vec<u8>)> {
    let mut reader = Reader::new(props, "PresetChunkItem");
    let version = reader.u32()?;
    if version != 1 {
        return Err(Error::malformed(
            "PresetChunkItem",
            offset,
            format!("unsupported properties version {version}"),
        ));
    }
    let _auth_checksum = reader.u32()?;
    let chunk_version = reader.u32()?;
    if chunk_version != 1 {
        return Err(Error::malformed(
            "PresetChunkItem",
            offset + 8,
            format!("unsupported BinaryChunk version {chunk_version}"),
        ));
    }
    let size = reader.u64()?;
    let size = usize::try_from(size)
        .map_err(|_| Error::malformed("PresetChunkItem", offset, "chunk stream too large"))?;
    let chunk_stream = reader.take(size)?.to_vec();
    // Retail files carry 8 bytes after the declared stream. The docs do not assign them a
    // meaning, so they are preserved rather than rejected or interpreted.
    let trailing = reader.take(reader.remaining())?.to_vec();
    Ok((chunk_stream, trailing))
}

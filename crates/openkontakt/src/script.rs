//! KSP script slots (`0x06`, version `0x60`).
//!
//! The parser preserves the private section and all persistent values. Script execution is a
//! separate concern; this module only turns the bytes into text.

use crate::chunk::read_structured_object;
use crate::error::{Error, Result};
use crate::reader::Reader;

#[derive(Clone, Debug, Default)]
pub struct PersistentValue {
    pub name: String,
    pub values: Vec<i64>,
    pub raw: String,
}

#[derive(Clone, Debug, Default)]
pub struct ScriptSlot {
    pub index: usize,
    pub version: u16,
    pub flag: u8,
    pub text: Option<String>,
    pub editor_open: bool,
    pub touched: bool,
    pub bypass: bool,
    pub password: Vec<u8>,
    pub description: Option<String>,
    pub textfile_name: Option<String>,
    pub persistent: Vec<PersistentValue>,
    pub private: Vec<u8>,
}

pub fn parse_script(index: usize, body: &[u8]) -> Result<ScriptSlot> {
    let object = read_structured_object(body, "BParScript")?;
    match object.version {
        0x60 => parse_v60(index, object.flag, object.public, object.private),
        0x50 => unimplemented!(
            "KSP script layout 0x50: the field layout beyond the first three strings has never been \
             observed, so this slot cannot be read"
        ),
        other => Err(Error::malformed(
            "BParScript",
            0,
            format!("unsupported script version 0x{other:02X}"),
        )),
    }
}

fn parse_v60(index: usize, flag: u8, public: &[u8], private: &[u8]) -> Result<ScriptSlot> {
    let mut reader = Reader::new(public, "BParScript v0x60");
    let text = reader.text_bytes_len_i32()?;
    let editor_open = reader.u8()? != 0;
    let touched = reader.u8()? != 0;
    let bypass = reader.u8()? != 0;
    let password_len = reader.u32()? as usize;
    let password = reader.take(password_len)?.to_vec();
    let description = reader.text_bytes_len_i32()?;
    let textfile_name = reader.text_bytes_len_i32()?;
    let persistent_count = reader.u32()? as usize;
    if persistent_count > reader.remaining() / 4 {
        return Err(Error::malformed(
            "BParScript v0x60",
            reader.position(),
            format!("persistent count {persistent_count} overruns"),
        ));
    }
    let mut persistent = Vec::with_capacity(persistent_count);
    for _ in 0..persistent_count {
        let len = reader.u32()? as usize;
        let raw_bytes = reader.take(len)?;
        let raw = String::from_utf8_lossy(raw_bytes).into_owned();
        let mut split = raw.split_whitespace();
        let name = split.next().unwrap_or_default().to_string();
        let values = split.filter_map(|v| v.parse::<i64>().ok()).collect();
        persistent.push(PersistentValue { name, values, raw });
    }
    if !reader.is_empty() {
        return Err(Error::malformed(
            "BParScript v0x60",
            reader.position(),
            format!("{} trailing bytes in public section", reader.remaining()),
        ));
    }
    Ok(ScriptSlot {
        index,
        version: 0x60,
        flag,
        text,
        editor_open,
        touched,
        bypass,
        password,
        description,
        textfile_name,
        persistent,
        private: private.to_vec(),
    })
}

impl ScriptSlot {
    pub fn is_empty(&self) -> bool {
        self.text.is_none()
            && self.password.is_empty()
            && self.description.is_none()
            && self.textfile_name.is_none()
            && self.persistent.is_empty()
    }

    pub fn persistent_value(&self, name: &str) -> Option<&[i64]> {
        self.persistent
            .iter()
            .find(|entry| entry.name == name)
            .map(|entry| entry.values.as_slice())
    }
}

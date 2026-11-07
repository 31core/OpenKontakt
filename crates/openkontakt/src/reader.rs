//! Bounds-checked little-endian cursor used by every layer.
//!
//! The format is completely unaligned; all reads go through this type so they are explicit.

use crate::error::{Error, Result};

#[derive(Clone, Debug)]
pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    base: usize,
    layer: &'static str,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8], layer: &'static str) -> Self {
        Self {
            data,
            pos: 0,
            base: 0,
            layer,
        }
    }

    pub fn with_base(data: &'a [u8], base: usize, layer: &'static str) -> Self {
        Self {
            data,
            pos: 0,
            base,
            layer,
        }
    }

    pub fn position(&self) -> usize {
        self.base + self.pos
    }

    pub fn offset_in_slice(&self) -> usize {
        self.pos
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    pub fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    fn require(&self, n: usize) -> Result<()> {
        if self.remaining() < n {
            return Err(Error::Truncated {
                layer: self.layer,
                offset: self.position(),
                needed: n,
                available: self.remaining(),
            });
        }
        Ok(())
    }

    pub fn slice_from(&self, start: usize) -> Result<&'a [u8]> {
        if start > self.pos {
            return Err(Error::malformed(
                self.layer,
                self.position(),
                "invalid slice start",
            ));
        }
        Ok(&self.data[start..self.pos])
    }

    pub fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        self.require(n)?;
        let start = self.pos;
        self.pos += n;
        Ok(&self.data[start..start + n])
    }

    pub fn peek(&self, n: usize) -> Result<&'a [u8]> {
        self.require(n)?;
        Ok(&self.data[self.pos..self.pos + n])
    }

    pub fn skip(&mut self, n: usize) -> Result<()> {
        self.take(n).map(|_| ())
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    pub fn i8(&mut self) -> Result<i8> {
        Ok(self.u8()? as i8)
    }

    pub fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub fn i16(&mut self) -> Result<i16> {
        Ok(self.u16()? as i16)
    }

    pub fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn i32(&mut self) -> Result<i32> {
        Ok(self.u32()? as i32)
    }

    pub fn u64(&mut self) -> Result<u64> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    pub fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_bits(self.u32()?))
    }

    pub fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_bits(self.u64()?))
    }

    /// Counted UTF-16LE string, with the count measured in code units.
    pub fn wstring(&mut self) -> Result<String> {
        let n = self.u32()? as usize;
        let bytes = n.checked_mul(2).ok_or_else(|| {
            Error::malformed(self.layer, self.position(), "UTF-16 string length overflow")
        })?;
        if bytes > self.remaining() {
            return Err(Error::Truncated {
                layer: self.layer,
                offset: self.position(),
                needed: bytes,
                available: self.remaining(),
            });
        }
        let raw = self.take(bytes)?;
        let units: Vec<u16> = raw
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        Ok(String::from_utf16_lossy(&units))
    }

    /// Counted UTF-16LE string. Returns `None` for a negative signed count.
    pub fn wstring_opt_i32(&mut self) -> Result<Option<String>> {
        let n = self.i32()?;
        if n < 0 {
            return Ok(None);
        }
        let n = n as usize;
        let bytes = n.checked_mul(2).ok_or_else(|| {
            Error::malformed(self.layer, self.position(), "UTF-16 string length overflow")
        })?;
        let raw = self.take(bytes)?;
        let units: Vec<u16> = raw
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        Ok(Some(String::from_utf16_lossy(&units)))
    }

    /// Length-prefixed bytes interpreted as UTF-8, with Windows-1252 as the documented fallback.
    pub fn text_bytes_len_i32(&mut self) -> Result<Option<String>> {
        let n = self.i32()?;
        if n < 0 {
            return Ok(None);
        }
        let n = n as usize;
        let raw = self.take(n)?;
        Ok(Some(decode_text(raw)))
    }

    pub fn bytes_len_u32(&mut self) -> Result<Vec<u8>> {
        let n = self.u32()? as usize;
        if n > self.remaining() {
            return Err(Error::Truncated {
                layer: self.layer,
                offset: self.position(),
                needed: n,
                available: self.remaining(),
            });
        }
        Ok(self.take(n)?.to_vec())
    }
}

/// Decode bytes as strict UTF-8, falling back to Windows-1252 exactly as the chapter specifies.
pub fn decode_text(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => {
            let (decoded, _, _) = encoding_rs::WINDOWS_1252.decode(bytes);
            decoded.into_owned()
        }
    }
}

/// Convert a signed file index to a usable `usize`, preserving "no file" as `None`.
pub fn file_index(id: i32) -> Option<usize> {
    if id < 0 { None } else { Some(id as usize) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_unaligned_fields() {
        let data = [0xAA, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        let mut r = Reader::new(&data, "test");
        assert_eq!(r.u8().unwrap(), 0xAA);
        assert_eq!(r.u32().unwrap(), 0x04030201);
        assert_eq!(r.u32().unwrap(), 0x08070605);
    }

    #[test]
    fn rejects_overrun() {
        let mut r = Reader::new(&[1, 2, 3], "test");
        assert!(r.u32().is_err());
    }

    #[test]
    fn decodes_windows_1252_fallback() {
        assert_eq!(decode_text(&[0xE9]), "é");
    }
}

//! `.ncw` — NI's compressed sample container.
//!
//! A packed library ships its audio as `.ncw` instead of `.wav`. The file is a header, a block
//! offset table, and then fixed-size blocks of bit-packed audio; there is no entropy coder, so
//! decoding is fast and bit-exact.
//!
//! Layout (all little-endian):
//!
//! ```text
//! file header   120 bytes
//! block table   u32 × (blocks + 1), relative to the data offset; last entry == data size
//! data          blocks, `data size` bytes
//! ```
//!
//! Every block covers 512 sample frames, and channels are interleaved **per block**: at each table
//! offset there is one sub-block per channel, in channel order.
//!
//! ```text
//! sub-block header  16 bytes
//!   +0x00  u32   signature 0x3E9A0C16
//!   +0x04  i32   base value
//!   +0x08  i16   bits
//!   +0x0a  u16   flags
//!   +0x0c  u32   unknown (zero in every observed file)
//! body     512 values of |bits| bits, packed LSB-first
//! ```
//!
//! `bits` is the width of the block's values and selects how the body is read:
//!
//! * `bits > 0` — delta coded. The first sample of the block is `base`; value `i` is added to it
//!   to produce sample `i + 1`. The body still holds 512 values, so the last delta produces a 512th
//!   sample that is never emitted: 512 samples come out of 511 used deltas.
//! * `bits < 0` — raw. The body holds 512 samples of `|bits|` width; `base` is ignored.
//!
//! `flags` bit 0 is mid/side (sub-block 0 is mid, sub-block 1 is side, `l = m + s`, `r = m - s`);
//! bit 1 says the samples are IEEE-754 single precision stored as raw bit patterns.
//!
//! The last block is padded to 512 frames and the padding is discarded using `frames` from the
//! header.
//!
//! Every rule above was checked against 5 stereo PCM24 files (9,814 sub-blocks): table length
//! always `ceil(frames / 512) + 1`, first entry 0, sentinel == data size, every terminal delta
//! zero, every width the narrowest that holds the block's deltas, and block-boundary steps the
//! same size as interior steps (so nothing is dropped at a boundary).

use crate::error::{Error, Result};

/// First four bytes of every observed file. The word after them is a version (`0x00000131` seen).
pub const FILE_MAGIC: [u8; 4] = [0x01, 0xA8, 0x9E, 0xD6];
const BLOCK_MAGIC: u32 = 0x3E9A_0C16;

const HEADER_LEN: usize = 120;
const TABLE_ENTRY: usize = 4;
const SUB_HEADER_LEN: usize = 16;
/// Frames covered by one block. Fixed: the table length is derived from it.
pub const FRAMES_PER_BLOCK: usize = 512;

/// File header fields this reader understands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NcwInfo {
    pub channels: u16,
    /// Declared bit depth of the source audio (16, 24 or 32 observed).
    pub bits: u16,
    pub sample_rate: u32,
    pub frames: usize,
    /// `1` in float files, `0` in PCM files. Informational: block flags decide decoding.
    pub format_word: u32,
}

/// Fully decoded audio, interleaved by frame.
#[derive(Clone, Debug)]
pub struct NcwAudio {
    pub channels: u16,
    pub sample_rate: u32,
    pub frames: usize,
    pub samples: Vec<f32>,
}

struct SubBlock {
    offset: usize,
    base: i32,
    bits: i16,
    flags: u16,
}

impl SubBlock {
    fn width(&self) -> usize {
        self.bits.unsigned_abs() as usize
    }

    fn is_delta(&self) -> bool {
        self.bits > 0
    }

    /// Offset just past this sub-block's body.
    fn end(&self) -> usize {
        self.offset + SUB_HEADER_LEN + (FRAMES_PER_BLOCK * self.width()).div_ceil(8)
    }
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    bytes
        .get(at..at + 4)
        .map(|slice| u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    bytes
        .get(at..at + 2)
        .map(|slice| u16::from_le_bytes([slice[0], slice[1]]))
}

/// Parse the 120-byte header. Does not look at the block table.
pub fn parse_ncw_info(bytes: &[u8]) -> Result<NcwInfo> {
    let magic = bytes
        .get(0..4)
        .ok_or_else(|| Error::malformed("NCW header", 0, "file is shorter than the signature"))?;
    if magic != FILE_MAGIC {
        return Err(Error::malformed(
            "NCW header",
            0,
            format!(
                "signature is {}, expected 01 A8 9E D6",
                magic
                    .iter()
                    .map(|b| format!("{b:02X}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
        ));
    }
    let channels = u16_at(bytes, 0x08)
        .ok_or_else(|| Error::malformed("NCW header", 0x08, "truncated channel count"))?;
    let bits = u16_at(bytes, 0x0A)
        .ok_or_else(|| Error::malformed("NCW header", 0x0A, "truncated bit depth"))?;
    let sample_rate = u32_at(bytes, 0x0C)
        .ok_or_else(|| Error::malformed("NCW header", 0x0C, "truncated sample rate"))?;
    let frames = u32_at(bytes, 0x10)
        .ok_or_else(|| Error::malformed("NCW header", 0x10, "truncated sample count"))?
        as usize;
    let format_word = u32_at(bytes, 0x20)
        .ok_or_else(|| Error::malformed("NCW header", 0x20, "truncated format word"))?;
    if channels == 0 || channels > 2 {
        return Err(Error::unsupported(format!(
            "NCW with {channels} channels (only mono and stereo are decoded)"
        )));
    }
    if !(1..=32).contains(&bits) {
        return Err(Error::unsupported(format!(
            "NCW declaring {bits} bits per sample"
        )));
    }
    Ok(NcwInfo {
        channels,
        bits,
        sample_rate,
        frames,
        format_word,
    })
}

/// A parsed `.ncw` header and block table.
///
/// Held separately from the bytes so the same reader can serve a memory-mapped file and a payload
/// slice inside an `.nkx`/`.nkr`. Every method takes the bytes as an argument for that reason.
pub struct NcwFile {
    info: NcwInfo,
    table: Vec<u32>,
    data_offset: usize,
    data_size: usize,
}

impl NcwFile {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let info = parse_ncw_info(bytes)?;
        let blocks_offset = u32_at(bytes, 0x14)
            .ok_or_else(|| Error::malformed("NCW header", 0x14, "truncated table offset"))?
            as usize;
        let data_offset = u32_at(bytes, 0x18)
            .ok_or_else(|| Error::malformed("NCW header", 0x18, "truncated data offset"))?
            as usize;
        let data_size = u32_at(bytes, 0x1C)
            .ok_or_else(|| Error::malformed("NCW header", 0x1C, "truncated data size"))?
            as usize;

        if blocks_offset < HEADER_LEN {
            return Err(Error::malformed(
                "NCW header",
                0x14,
                format!("block table at {blocks_offset} overlaps the {HEADER_LEN}-byte header"),
            ));
        }
        if data_offset < blocks_offset + TABLE_ENTRY {
            return Err(Error::malformed(
                "NCW header",
                0x18,
                format!("data offset {data_offset} overlaps the block table at {blocks_offset}"),
            ));
        }
        if data_offset + data_size > bytes.len() {
            return Err(Error::malformed(
                "NCW header",
                0x1C,
                format!(
                    "data region {data_size} bytes at {data_offset} runs past the end of the file ({} bytes)",
                    bytes.len()
                ),
            ));
        }

        let entries = (data_offset - blocks_offset) / TABLE_ENTRY;
        if entries < 2 {
            return Err(Error::malformed(
                "NCW block table",
                blocks_offset,
                "needs at least one block plus the sentinel",
            ));
        }
        let blocks = entries - 1;
        let expected = info.frames.div_ceil(FRAMES_PER_BLOCK);
        if blocks != expected {
            return Err(Error::malformed(
                "NCW block table",
                blocks_offset,
                format!(
                    "{blocks} blocks for {} frames, expected {expected} blocks of {FRAMES_PER_BLOCK}",
                    info.frames
                ),
            ));
        }

        let mut table: Vec<u32> = Vec::with_capacity(entries);
        for index in 0..entries {
            let at = blocks_offset + index * TABLE_ENTRY;
            let value = u32_at(bytes, at).ok_or(Error::Truncated {
                layer: "NCW block table",
                offset: at,
                needed: TABLE_ENTRY,
                available: bytes.len().saturating_sub(at),
            })? as usize;
            if index == 0 && value != 0 {
                return Err(Error::malformed(
                    "NCW block table",
                    at,
                    format!("first entry is {value}, expected 0"),
                ));
            }
            if let Some(&previous) = table.last()
                && value < previous as usize
            {
                return Err(Error::malformed(
                    "NCW block table",
                    at,
                    format!("entry {value} is below its predecessor {previous}"),
                ));
            }
            if value > data_size {
                return Err(Error::malformed(
                    "NCW block table",
                    at,
                    format!("entry {value} runs past the {data_size}-byte data region"),
                ));
            }
            table.push(value as u32);
        }
        let sentinel = table[blocks] as usize;
        if sentinel != data_size {
            return Err(Error::malformed(
                "NCW block table",
                blocks_offset + blocks * TABLE_ENTRY,
                format!("sentinel is {sentinel}, expected the data size {data_size}"),
            ));
        }

        Ok(Self {
            info,
            table,
            data_offset,
            data_size,
        })
    }

    pub fn info(&self) -> NcwInfo {
        self.info
    }

    pub fn blocks(&self) -> usize {
        self.table.len() - 1
    }

    pub fn frames(&self) -> usize {
        self.info.frames
    }

    /// Frames in one block: 512, less for the last one.
    pub fn block_frames(&self, block: usize) -> usize {
        (self.info.frames - block * FRAMES_PER_BLOCK).min(FRAMES_PER_BLOCK)
    }

    /// Decode one block into interleaved samples, appending to `out`.
    ///
    /// This is the unit of streaming: a caller that reads frames in order decodes one block per
    /// 512 frames and never touches the rest of the file.
    pub fn decode_block(&self, bytes: &[u8], block: usize, out: &mut Vec<f32>) -> Result<()> {
        if block >= self.blocks() {
            return Err(Error::malformed(
                "NCW block",
                block,
                format!("block {block} of {}", self.blocks()),
            ));
        }
        let data = &bytes[self.data_offset..self.data_offset + self.data_size];
        let channels = self.info.channels as usize;
        let subs = sub_blocks(
            data,
            self.table[block] as usize,
            self.table[block + 1] as usize,
            channels,
        )?;
        // The first channel's flags select the transform for the whole block.
        let flags = subs[0].flags;
        let mid_side = flags & 1 != 0;
        let float_mode = flags & 2 != 0;
        if mid_side && channels != 2 {
            return Err(Error::unsupported(
                "mid/side NCW sub-block in a mono file".to_string(),
            ));
        }
        if mid_side && float_mode {
            return Err(Error::unsupported(
                "float mid/side NCW block (never observed, not specified)".to_string(),
            ));
        }
        let scale = self.scale();
        // Two scratch buffers on the stack: allocating per block would put the allocator on the
        // playback path, and the mid/side transform needs both channels at once.
        let mut scratch = [[0i32; FRAMES_PER_BLOCK]; 2];
        for (index, sub) in subs.iter().enumerate() {
            fill_raw(data, sub, &mut scratch[index]);
        }
        let emitted = self.block_frames(block);
        if mid_side {
            for (mid, side) in scratch[0][..emitted]
                .iter()
                .zip(&scratch[1][..emitted])
            {
                out.push(to_float(mid.wrapping_add(*side), float_mode, scale));
                out.push(to_float(mid.wrapping_sub(*side), float_mode, scale));
            }
        } else if channels == 1 {
            out.extend(
                scratch[0][..emitted]
                    .iter()
                    .map(|value| to_float(*value, float_mode, scale)),
            );
        } else {
            for (left, right) in scratch[0][..emitted]
                .iter()
                .zip(&scratch[1][..emitted])
            {
                out.push(to_float(*left, float_mode, scale));
                out.push(to_float(*right, float_mode, scale));
            }
        }
        Ok(())
    }

    /// Decode the whole payload.
    pub fn decode_all(&self, bytes: &[u8]) -> Result<NcwAudio> {
        let channels = self.info.channels as usize;
        let mut samples = Vec::with_capacity(self.info.frames * channels);
        for block in 0..self.blocks() {
            self.decode_block(bytes, block, &mut samples)?;
        }
        let frames = self.info.frames.min(samples.len() / channels);
        Ok(NcwAudio {
            channels: self.info.channels,
            sample_rate: self.info.sample_rate,
            frames,
            samples,
        })
    }

    /// Reciprocal of the full scale, for the integer-to-float conversion.
    fn scale(&self) -> f32 {
        if self.info.bits >= 32 {
            1.0 / 2_147_483_648.0f32
        } else {
            1.0 / (1u32 << (self.info.bits - 1)) as f32
        }
    }
}

/// Read `out.len()` values of `width` bits, packed LSB-first and sign-extended.
///
/// One shift register, refilled a byte at a time: this runs once per value of every block, so the
/// inner loop is the hot path of playback.
fn unpack_into(body: &[u8], width: usize, out: &mut [i32]) {
    // A short body decodes as silence rather than panicking; the header checks already reported
    // the file, and this keeps the fill from being a bounds check per byte.
    if width == 0 || body.len() * 8 < out.len() * width {
        out.fill(0);
        return;
    }
    let mask = (1u64 << width) - 1;
    let mut acc = 0u64;
    let mut have = 0usize;
    let mut pos = 0usize;
    for slot in out.iter_mut() {
        while have < width {
            acc |= (body[pos] as u64) << have;
            pos += 1;
            have += 8;
        }
        let raw = acc & mask;
        acc >>= width;
        have -= width;
        *slot = sign_extend(raw, width) as i32;
    }
}

/// Test helper: the same unpack, returning `i64` so values can be compared directly.
#[cfg(test)]
fn unpack(body: &[u8], width: usize, count: usize) -> Vec<i64> {
    let mut raw = vec![0i32; count];
    unpack_into(body, width, &mut raw);
    raw.into_iter().map(i64::from).collect()
}

fn sign_extend(value: u64, width: usize) -> i64 {
    // Branchless: audio changes sign constantly, so a test-and-subtract would mispredict about
    // half the time. Shifting the sign bit up and reading it back as a signed value costs two
    // instructions and no branch.
    let shift = 64 - width as u32;
    ((value << shift) as i64) >> shift
}

fn sub_blocks(data: &[u8], start: usize, end: usize, channels: usize) -> Result<Vec<SubBlock>> {
    let mut subs = Vec::with_capacity(channels);
    let mut at = start;
    for channel in 0..channels {
        if at + SUB_HEADER_LEN > end {
            return Err(Error::malformed(
                "NCW sub-block",
                at,
                format!("channel {channel} header does not fit in the block"),
            ));
        }
        let magic = u32_at(data, at).unwrap_or(0);
        if magic != BLOCK_MAGIC {
            return Err(Error::malformed(
                "NCW sub-block",
                at,
                format!("signature is 0x{magic:08X}, expected 0x{BLOCK_MAGIC:08X}"),
            ));
        }
        let block = SubBlock {
            offset: at,
            base: u32_at(data, at + 4).unwrap_or(0) as i32,
            bits: u16_at(data, at + 8).unwrap_or(0) as i16,
            flags: u16_at(data, at + 10).unwrap_or(0),
        };
        let width = block.width();
        if !(1..=32).contains(&width) {
            return Err(Error::unsupported(format!(
                "NCW sub-block at {at} has value width {width}; widths 0 and 1 have no known meaning"
            )));
        }
        at = block.end();
        if at > end {
            return Err(Error::malformed(
                "NCW sub-block",
                at,
                format!(
                    "channel {channel} body of {width}-bit values runs past the end of the block"
                ),
            ));
        }
        subs.push(block);
    }
    Ok(subs)
}

/// Fill `out` with one sub-block's 512 raw values (bit patterns for float files).
///
/// The output is `i32` because every value the format can produce fits: samples are at most 32
/// bits and the accumulator wraps like the reference decoder's integer arithmetic.
fn fill_raw(data: &[u8], sub: &SubBlock, out: &mut [i32; FRAMES_PER_BLOCK]) {
    let body = &data[sub.offset + SUB_HEADER_LEN..sub.end()];
    if !sub.is_delta() {
        unpack_into(body, sub.width(), out);
        return;
    }
    // Delta blocks: unpack the deltas into the output, then accumulate in place. Each slot is
    // read before it is overwritten, so the running sum can move forward inside one buffer.
    unpack_into(body, sub.width(), out);
    let mut acc = sub.base;
    for frame in out.iter_mut() {
        let delta = *frame;
        *frame = acc;
        acc = acc.wrapping_add(delta);
    }
}

fn to_float(value: i32, float_mode: bool, scale: f32) -> f32 {
    if float_mode {
        let sample = f32::from_bits(value as u32);
        if sample.is_finite() { sample } else { 0.0 }
    } else {
        // `scale` is a power of two, so multiplying by its reciprocal is exact and avoids a
        // division per sample on the playback path.
        (value as f32 * scale).clamp(-1.0, 1.0)
    }
}

/// Decode a whole `.ncw` into interleaved samples.
pub fn decode_ncw(bytes: &[u8]) -> Result<NcwAudio> {
    NcwFile::parse(bytes)?.decode_all(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an NCW from interleaved samples, delta coded at the narrowest width per block.
    fn build(samples: &[i32], channels: u16, bits: u16, rate: u32) -> Vec<u8> {
        let frames = samples.len() / channels as usize;
        let blocks = frames.div_ceil(FRAMES_PER_BLOCK);
        let mut payload: Vec<u8> = Vec::new();
        let mut table: Vec<u32> = vec![0];
        for block in 0..blocks {
            for channel in 0..channels as usize {
                let first = (block * FRAMES_PER_BLOCK) * channels as usize + channel;
                let base = samples[first];
                let mut deltas = Vec::with_capacity(FRAMES_PER_BLOCK);
                for frame in 1..FRAMES_PER_BLOCK {
                    let at = (block * FRAMES_PER_BLOCK + frame) * channels as usize + channel;
                    let previous = at - channels as usize;
                    deltas.push(
                        samples.get(at).copied().unwrap_or(0)
                            - samples.get(previous).copied().unwrap_or(0),
                    );
                }
                let widest = deltas.iter().map(|d| d.unsigned_abs()).max().unwrap_or(0);
                let need = 32 - widest.leading_zeros() as i16;
                let width = (need + 1).max(2).min(bits as i16);
                deltas.push(0);

                payload.extend_from_slice(&BLOCK_MAGIC.to_le_bytes());
                payload.extend_from_slice(&base.to_le_bytes());
                payload.extend_from_slice(&width.to_le_bytes());
                payload.extend_from_slice(&0u16.to_le_bytes());
                payload.extend_from_slice(&0u32.to_le_bytes());

                let mask = (1u64 << width) - 1;
                let mut acc = 0u64;
                let mut used = 0u32;
                for delta in &deltas {
                    acc |= ((*delta as i64) as u64 & mask) << used;
                    used += width as u32;
                    while used >= 8 {
                        payload.push((acc & 0xFF) as u8);
                        acc >>= 8;
                        used -= 8;
                    }
                }
                if used > 0 {
                    payload.push((acc & 0xFF) as u8);
                }
            }
            table.push(payload.len() as u32);
        }
        let data_offset = HEADER_LEN + table.len() * TABLE_ENTRY;
        let mut file = vec![0u8; HEADER_LEN];
        file[0..4].copy_from_slice(&FILE_MAGIC);
        file[4..8].copy_from_slice(&0x131u32.to_le_bytes());
        file[0x08..0x0A].copy_from_slice(&channels.to_le_bytes());
        file[0x0A..0x0C].copy_from_slice(&bits.to_le_bytes());
        file[0x0C..0x10].copy_from_slice(&rate.to_le_bytes());
        file[0x10..0x14].copy_from_slice(&(frames as u32).to_le_bytes());
        file[0x14..0x18].copy_from_slice(&(HEADER_LEN as u32).to_le_bytes());
        file[0x18..0x1C].copy_from_slice(&(data_offset as u32).to_le_bytes());
        file[0x1C..0x20].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        for entry in &table {
            file.extend_from_slice(&entry.to_le_bytes());
        }
        file.extend_from_slice(&payload);
        file
    }

    fn tone(frames: usize, channels: u16) -> Vec<i32> {
        let mut samples = Vec::with_capacity(frames * channels as usize);
        for frame in 0..frames {
            let value = ((frame as f64 * 0.01).sin() * 6_000_000.0) as i32;
            for channel in 0..channels as usize {
                samples.push(if channel == 0 { value } else { -value });
            }
        }
        samples
    }

    #[test]
    fn reads_the_header() {
        let file = build(&tone(1000, 2), 2, 24, 44_100);
        let info = parse_ncw_info(&file).unwrap();
        assert_eq!(info.channels, 2);
        assert_eq!(info.bits, 24);
        assert_eq!(info.sample_rate, 44_100);
        assert_eq!(info.frames, 1000);
    }

    #[test]
    fn unpacks_values_lsb_first() {
        // 3-bit two's complement 1, -1, 0, 2 packed LSB-first.
        assert_eq!(unpack(&[0x39, 0x04], 3, 4), vec![1, -1, 0, 2]);
    }

    #[test]
    fn decodes_delta_blocks_exactly() {
        let source = tone(1500, 2);
        let file = build(&source, 2, 24, 44_100);
        let audio = decode_ncw(&file).unwrap();
        assert_eq!(audio.frames, 1500);
        assert_eq!(audio.samples.len(), 1500 * 2);
        for (index, expected) in source.iter().enumerate() {
            let got = audio.samples[index] * 8_388_608.0;
            assert!(
                (got - *expected as f32).abs() < 1.0,
                "sample {index}: {got} != {expected}"
            );
        }
    }

    #[test]
    fn a_block_boundary_is_continuous() {
        // Blocks are 512 frames; a decoder that dropped a sample per block would step here.
        let source = tone(1024, 1);
        let file = build(&source, 1, 24, 44_100);
        let audio = decode_ncw(&file).unwrap();
        for frame in 1..1024 {
            let step = (audio.samples[frame] - audio.samples[frame - 1]).abs() * 8_388_608.0;
            // A sine of 0.01 rad per frame moves at most 60,000 counts per frame.
            assert!(step < 70_000.0, "step {step} at frame {frame}");
        }
    }

    #[test]
    fn the_last_block_is_truncated_to_the_frame_count() {
        let audio = decode_ncw(&build(&tone(600, 1), 1, 16, 48_000)).unwrap();
        assert_eq!(audio.frames, 600);
        assert_eq!(audio.samples.len(), 600);
    }

    #[test]
    fn mono_files_stay_mono() {
        let audio = decode_ncw(&build(&tone(700, 1), 1, 24, 48_000)).unwrap();
        assert_eq!(audio.channels, 1);
        assert_eq!(audio.samples.len(), 700);
    }

    #[test]
    fn the_player_loader_reads_a_ncw_file() {
        let path = std::env::temp_dir().join(format!(
            "openkontakt-ncw-{}.ncw",
            std::process::id()
        ));
        let file = build(&tone(1200, 2), 2, 24, 48_000);
        std::fs::write(&path, &file).unwrap();
        let sample = crate::wav::open_sample(&path, 8).unwrap();
        assert_eq!(sample.frames, 1200);
        assert_eq!(sample.channels, 2);
        assert_eq!(sample.sample_rate, 48_000);
        assert!(sample.peak > 0.0);
        let (left, right) = sample.frame(700);
        assert!(
            left * right < 0.0,
            "the fixture encodes the two channels with opposite signs"
        );
        // `frame` is the cold accessor: it decodes the block on each call and must still agree.
        let expected = decode_ncw(&file).unwrap();
        for frame in [0, 511, 512, 1199] {
            assert!(
                (sample.frame(frame).0 - expected.samples[frame * 2]).abs() < 1e-6,
                "frame {frame}"
            );
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn streaming_matches_decoding_the_whole_file() {
        use crate::wav::{SampleCursor, map_ncw};
        use std::sync::Arc;

        let file = build(&tone(1500, 2), 2, 24, 48_000);
        let path = std::env::temp_dir().join(format!(
            "openkontakt-ncw-stream-{}.ncw",
            std::process::id()
        ));
        std::fs::write(&path, &file).unwrap();

        let sample = map_ncw(&path, 128).unwrap();
        assert!(sample.is_streaming(), "NCW must stay in the mapping");
        assert_eq!(sample.mapped_bytes(), file.len() as u64);
        assert_eq!(sample.frames, 1500);
        assert!(sample.peak > 0.25, "preload should measure a peak");

        let expected = decode_ncw(&file).unwrap();
        let mut cursor = SampleCursor::new(Arc::clone(&Arc::new(sample)));
        for frame in 0..1500 {
            let (left, right) = cursor.frame(frame);
            assert!(
                (left - expected.samples[frame * 2]).abs() < 1e-6
                    && (right - expected.samples[frame * 2 + 1]).abs() < 1e-6,
                "frame {frame} differs"
            );
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_cursor_can_jump_between_blocks() {
        use crate::wav::{SampleCursor, map_ncw};
        use std::sync::Arc;

        let file = build(&tone(2000, 1), 1, 24, 48_000);
        let path = std::env::temp_dir().join(format!(
            "openkontakt-ncw-jump-{}.ncw",
            std::process::id()
        ));
        std::fs::write(&path, &file).unwrap();
        let sample = map_ncw(&path, 0).unwrap();
        let expected = decode_ncw(&file).unwrap();
        let mut cursor = SampleCursor::new(Arc::new(sample));
        // Walk blocks backwards and forwards; a two-block cache must still be exact.
        for frame in [1536, 0, 1023, 512, 1999, 700, 1800] {
            let (value, _) = cursor.frame(frame);
            assert!(
                (value - expected.samples[frame]).abs() < 1e-6,
                "frame {frame}"
            );
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_record_inside_a_container_streams_too() {
        use crate::wav::{SampleCursor, map_ncw_range};
        use std::sync::Arc;

        // A packed sample is the same payload behind a 31-byte block header.
        let payload = build(&tone(900, 2), 2, 24, 48_000);
        let mut container = vec![0xA5u8; 31];
        container.extend_from_slice(&payload);
        container.extend_from_slice(&[0u8; 16]);
        let path = std::env::temp_dir().join(format!(
            "openkontakt-ncw-packed-{}.nkr",
            std::process::id()
        ));
        std::fs::write(&path, &container).unwrap();

        let sample = map_ncw_range(&path, &path, 31, payload.len(), 64).unwrap();
        assert_eq!(sample.frames, 900);
        let expected = decode_ncw(&payload).unwrap();
        let mut cursor = SampleCursor::new(Arc::new(sample));
        for frame in [0, 511, 512, 899] {
            let (left, _) = cursor.frame(frame);
            assert!(
                (left - expected.samples[frame * 2]).abs() < 1e-6,
                "frame {frame}"
            );
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn rejects_a_bad_signature() {
        let mut file = build(&tone(600, 1), 1, 16, 48_000);
        file[0] = 0;
        let error = decode_ncw(&file).unwrap_err();
        assert!(error.to_string().contains("signature"), "{error}");
    }

    #[test]
    fn rejects_a_table_that_does_not_match_the_frame_count() {
        let mut file = build(&tone(600, 1), 1, 16, 48_000);
        file[0x10..0x14].copy_from_slice(&6000u32.to_le_bytes());
        let error = decode_ncw(&file).unwrap_err();
        assert!(error.to_string().contains("blocks for"), "{error}");
    }

    #[test]
    fn rejects_a_data_region_past_the_end_of_the_file() {
        let mut file = build(&tone(600, 1), 1, 16, 48_000);
        file[0x1C..0x20].copy_from_slice(&9_999_999u32.to_le_bytes());
        let error = decode_ncw(&file).unwrap_err();
        assert!(error.to_string().contains("runs past"), "{error}");
    }
}

//! WAV access: header parsing, and a memory-mapped sample backing.
//!
//! Large libraries are tens of gigabytes, so samples are **not** read into RAM. A file is mapped
//! read-only and decoded on the fly; the first `preload` frames are touched at load time so the
//! first notes do not take page faults at full depth. Small files (impulse responses) still use a
//! plain in-memory decode.

use crate::error::{Error, Result};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleFormat {
    U8,
    I16,
    I24,
    I32,
    F32,
}

impl SampleFormat {
    fn bytes(self) -> usize {
        match self {
            SampleFormat::U8 => 1,
            SampleFormat::I16 => 2,
            SampleFormat::I24 => 3,
            SampleFormat::I32 | SampleFormat::F32 => 4,
        }
    }
}

/// Parsed `fmt `/`data` pair.
#[derive(Clone, Copy, Debug)]
pub struct WavInfo {
    pub channels: u16,
    pub sample_rate: u32,
    pub format: SampleFormat,
    pub data_offset: usize,
    pub data_bytes: usize,
    pub frames: usize,
}

enum Backing {
    Owned(Vec<f32>),
    Mapped {
        map: Arc<memmap2::Mmap>,
        data_offset: usize,
        format: SampleFormat,
    },
    /// Compressed audio decoded a block at a time, straight out of the mapping.
    ///
    /// `base..base + length` is the `.ncw` payload inside the map: the whole file for a loose
    /// sample, a record slice for one packed in an `.nkx`/`.nkr`.
    Ncw {
        map: Arc<memmap2::Mmap>,
        base: usize,
        length: usize,
        file: crate::ncw::NcwFile,
        /// The file that holds the payload: the `.ncw` itself, or the container it is packed in.
        source: PathBuf,
    },
}

pub struct AudioSample {
    pub path: PathBuf,
    pub sample_rate: u32,
    pub channels: u16,
    pub frames: usize,
    /// Peak of the preloaded region; 0 when nothing was preloaded.
    pub peak: f32,
    /// RMS of the preloaded region, which is what the loader measures for layer levelling.
    /// Zero when nothing was preloaded.
    pub rms: f32,
    /// Bytes touched at load time (the DFD preload), which is what the process actually holds.
    pub resident_bytes: u64,
    backing: Backing,
}

impl std::fmt::Debug for AudioSample {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioSample")
            .field("path", &self.path)
            .field("sample_rate", &self.sample_rate)
            .field("channels", &self.channels)
            .field("frames", &self.frames)
            .field(
                "backing",
                &match self.backing {
                    Backing::Owned(_) => "owned",
                    Backing::Mapped { .. } => "mapped",
                    Backing::Ncw { .. } => "ncw",
                },
            )
            .finish()
    }
}

impl AudioSample {
    /// Empty sample used as a placeholder by the engine's voice pool.
    pub fn silent() -> Self {
        Self {
            path: PathBuf::new(),
            sample_rate: 48_000,
            channels: 1,
            frames: 0,
            peak: 0.0,
            rms: 0.0,
            resident_bytes: 0,
            backing: Backing::Owned(Vec::new()),
        }
    }

    pub fn is_stereo(&self) -> bool {
        self.channels == 2
    }

    /// Bytes this sample keeps mapped (not resident).
    pub fn mapped_bytes(&self) -> u64 {
        match &self.backing {
            Backing::Owned(data) => (data.len() * std::mem::size_of::<f32>()) as u64,
            Backing::Mapped { map, .. } => map.len() as u64,
            Backing::Ncw { map, .. } => map.len() as u64,
        }
    }

    /// True when the audio stays on disk and is decoded as it is read.
    pub fn is_streaming(&self) -> bool {
        matches!(self.backing, Backing::Mapped { .. } | Backing::Ncw { .. })
    }

    /// The file this sample's bytes are mapped from, when they are mapped at all.
    ///
    /// Several packed samples share one container, so a caller summing `mapped_bytes` has to
    /// deduplicate on this or it counts the same mapping once per sample.
    pub fn mapped_source(&self) -> Option<&Path> {
        match &self.backing {
            Backing::Mapped { .. } => Some(self.path.as_path()),
            Backing::Ncw { source, .. } => Some(source.as_path()),
            Backing::Owned(_) => None,
        }
    }

    /// Frames per decoded block, for callers that want to read ahead.
    pub fn block_frames(&self) -> Option<usize> {
        match &self.backing {
            Backing::Ncw { .. } => Some(crate::ncw::FRAMES_PER_BLOCK),
            _ => None,
        }
    }

    #[inline]
    fn raw(&self, index: usize) -> f32 {
        match &self.backing {
            Backing::Owned(data) => data.get(index).copied().unwrap_or(0.0),
            // Cold path only: decodes the whole block for one value. Playback goes through
            // `SampleCursor`, which caches the block it is playing.
            Backing::Ncw { .. } => {
                let stride = crate::ncw::FRAMES_PER_BLOCK;
                let channels = self.channels.max(1) as usize;
                let frame = index / channels;
                let Some(decoded) = self.ncw_block(frame / stride) else {
                    return 0.0;
                };
                decoded
                    .get((frame % stride) * channels + index % channels)
                    .copied()
                    .unwrap_or(0.0)
            }
            Backing::Mapped {
                map,
                data_offset,
                format,
            } => {
                let width = format.bytes();
                let at = data_offset + index * width;
                let Some(bytes) = map.get(at..at + width) else {
                    return 0.0;
                };
                match format {
                    SampleFormat::U8 => (bytes[0] as f32 - 128.0) / 128.0,
                    SampleFormat::I16 => i16::from_le_bytes([bytes[0], bytes[1]]) as f32 / 32_768.0,
                    SampleFormat::I24 => {
                        // Sign-extend: the three bytes are the high 24 bits of an i32 that is shifted
                        // down afterwards, which is what makes the sign bit work.
                        let value = i32::from_le_bytes([0, bytes[0], bytes[1], bytes[2]]) >> 8;
                        value as f32 / 8_388_608.0
                    }
                    SampleFormat::I32 => {
                        i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f32
                            / 2_147_483_648.0
                    }
                    SampleFormat::F32 => {
                        let value = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                        if value.is_finite() { value } else { 0.0 }
                    }
                }
            }
        }
    }

    pub fn frame(&self, index: usize) -> (f32, f32) {
        match self.channels {
            1 => {
                let value = if index < self.frames {
                    self.raw(index)
                } else {
                    0.0
                };
                (value, value)
            }
            2 => {
                if index >= self.frames {
                    return (0.0, 0.0);
                }
                (self.raw(index * 2), self.raw(index * 2 + 1))
            }
            _ => (0.0, 0.0),
        }
    }

    /// Decode one `.ncw` block, or `None` when this sample is not compressed.
    fn ncw_block(&self, block: usize) -> Option<Vec<f32>> {
        let Backing::Ncw {
            map,
            base,
            length,
            file,
            ..
        } = &self.backing
        else {
            return None;
        };
        let mut decoded =
            Vec::with_capacity(crate::ncw::FRAMES_PER_BLOCK * self.channels.max(1) as usize);
        file.decode_block(&map[*base..*base + *length], block, &mut decoded)
            .ok()?;
        Some(decoded)
    }

    /// Read a frame at a fractional position with an 8-tap windowed-sinc kernel.
    ///
    /// Linear interpolation is not good enough here: pitch-shifting by a non-integer ratio leaves
    /// images that fold back into the audible band. Measured against a reference implementation,
    /// linear interpolation put ~25 dB too much energy at 14–16 kHz — a clearly audible "grainy"
    /// top end — while the sample itself only has −35 dB there.
    ///
    /// For a `.ncw` this decodes the block of every tap, which is only appropriate for callers
    /// that read a handful of positions. Playback uses [`SampleCursor`], which caches them.
    pub fn sample_at(&self, position: f64) -> (f32, f32) {
        sinc_read(self.frames, position, |index| self.frame(index))
    }

    /// Linearly interpolate between two frames. `position` is in source frames.
    pub fn sample_linear(&self, position: f64) -> (f32, f32) {
        if self.frames == 0 {
            return (0.0, 0.0);
        }
        let position = position.max(0.0);
        let index = position.floor() as usize;
        let frac = (position - index as f64) as f32;
        let a = self.frame(index.min(self.frames.saturating_sub(1)));
        let b = self.frame((index + 1).min(self.frames.saturating_sub(1)));
        (a.0 + (b.0 - a.0) * frac, a.1 + (b.1 - a.1) * frac)
    }
}

/// A playback cursor over one sample, with the decoded block cached.
///
/// A `.ncw` has to be decoded before it can be read, and the block a voice is playing stays the
/// same for 512 frames. Caching it here keeps the audio thread free of locks and atomics: the
/// cursor owns its cache, and a voice owns its cursor. Two blocks are kept because the 8-tap
/// window straddles a boundary and a loop jump re-reads the block it just left.
///
/// Uncompressed samples cost nothing extra: the cursor forwards to the mapping, which needs no
/// cache.
pub struct SampleCursor {
    sample: Arc<AudioSample>,
    blocks: [Option<(usize, Vec<f32>)>; 2],
    next: usize,
}

impl SampleCursor {
    pub fn new(sample: Arc<AudioSample>) -> Self {
        Self {
            sample,
            blocks: [None, None],
            next: 0,
        }
    }

    pub fn sample(&self) -> &Arc<AudioSample> {
        &self.sample
    }

    pub fn frames(&self) -> usize {
        self.sample.frames
    }

    /// Read one frame, decoding its block when it is not already cached.
    pub fn frame(&mut self, index: usize) -> (f32, f32) {
        let Some(stride) = self.sample.block_frames() else {
            return self.sample.frame(index);
        };
        if index >= self.sample.frames {
            return (0.0, 0.0);
        }
        let channels = self.sample.channels.max(1) as usize;
        let block = index / stride;
        let at = (index % stride) * channels;
        let Some(data) = self.cached_block(block) else {
            return (0.0, 0.0);
        };
        match channels {
            1 => {
                let value = data.get(at).copied().unwrap_or(0.0);
                (value, value)
            }
            2 => (
                data.get(at).copied().unwrap_or(0.0),
                data.get(at + 1).copied().unwrap_or(0.0),
            ),
            _ => (0.0, 0.0),
        }
    }

    /// Read a frame at a fractional position, interpolating from cached blocks.
    pub fn sample_at(&mut self, position: f64) -> (f32, f32) {
        let frames = self.sample.frames;
        sinc_read(frames, position, |index| self.frame(index))
    }

    fn cached_block(&mut self, block: usize) -> Option<&[f32]> {
        let slot = self
            .blocks
            .iter()
            .position(|slot| matches!(slot, Some((cached, _)) if *cached == block));
        let slot = match slot {
            Some(slot) => slot,
            None => {
                let decoded = self.sample.ncw_block(block)?;
                let slot = self.next;
                self.blocks[slot] = Some((block, decoded));
                self.next = (self.next + 1) % self.blocks.len();
                slot
            }
        };
        self.blocks[slot].as_ref().map(|(_, data)| data.as_slice())
    }
}

/// 8-tap windowed-sinc read. `read` supplies a frame, so the caller decides how to fetch it.
fn sinc_read(
    frames: usize,
    position: f64,
    mut read: impl FnMut(usize) -> (f32, f32),
) -> (f32, f32) {
    if frames == 0 {
        return (0.0, 0.0);
    }
    let position = position.max(0.0);
    let base = position.floor();
    let index = base as isize;
    let frac = position - base;
    let weights = sinc_weights(frac);
    let mut left = 0.0f32;
    let mut right = 0.0f32;
    for (tap, weight) in weights.iter().enumerate() {
        // Taps run from `index - 3` to `index + 4`.
        let offset = tap as isize - 3;
        let at = index + offset;
        if at < 0 || at as usize >= frames {
            continue;
        }
        let (l, r) = read(at as usize);
        left += l * weight;
        right += r * weight;
    }
    (left, right)
}

/// Interpolation weights for the fractional part of a read position.
///
/// A Blackman-windowed sinc over ±4 samples, tabulated at 1024 phases: the kernel is the same for
/// every voice and every sample, and computing eight `sin` calls per output sample would be far too
/// expensive in the audio callback.
fn sinc_weights(frac: f64) -> &'static [f32; 8] {
    const PHASES: usize = 1024;
    static TABLE: std::sync::OnceLock<Vec<[f32; 8]>> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        (0..PHASES)
            .map(|phase| {
                let frac = phase as f64 / PHASES as f64;
                let mut weights = [0.0f32; 8];
                let mut sum = 0.0f64;
                for (tap, weight) in weights.iter_mut().enumerate() {
                    let x = tap as f64 - 3.0 - frac;
                    let sinc = if x.abs() < 1e-9 {
                        1.0
                    } else {
                        (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x)
                    };
                    // Blackman window over |x| <= 4, zero at the edges.
                    let window = if x.abs() >= 4.0 {
                        0.0
                    } else {
                        let t = std::f64::consts::PI * x / 4.0;
                        0.42 + 0.5 * t.cos() + 0.08 * (2.0 * t).cos()
                    };
                    *weight = (sinc * window) as f32;
                    sum += (*weight) as f64;
                }
                // Preserve DC: the taps must sum to one for every phase.
                if sum.abs() > 1e-12 {
                    let scale = (1.0 / sum) as f32;
                    for weight in weights.iter_mut() {
                        *weight *= scale;
                    }
                }
                weights
            })
            .collect()
    });
    let phase = (frac * PHASES as f64) as usize;
    &table[phase.min(PHASES - 1)]
}

fn wav_error(path: &Path, message: impl Into<String>) -> Error {
    Error::Wav {
        path: path.to_path_buf(),
        message: message.into(),
    }
}

/// Parse the RIFF header far enough to locate and describe the `data` chunk.
pub fn parse_wav_info(path: &Path) -> Result<(WavInfo, std::fs::File)> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).map_err(|error| Error::Io {
        path: path.to_path_buf(),
        source: error,
    })?;
    let mut header = [0u8; 12];
    file.read_exact(&mut header)
        .map_err(|_| wav_error(path, "file is shorter than a RIFF header"))?;
    if &header[0..4] != b"RIFF" || &header[8..12] != b"WAVE" {
        return Err(wav_error(path, "not a RIFF/WAVE file"));
    }
    let file_len = file
        .seek(SeekFrom::End(0))
        .map_err(|error| wav_error(path, error.to_string()))?;
    file.seek(SeekFrom::Start(12))
        .map_err(|error| wav_error(path, error.to_string()))?;

    let mut channels = 0u16;
    let mut sample_rate = 0u32;
    let mut format = SampleFormat::I16;
    let mut data_offset = None;
    let mut data_bytes = 0usize;

    loop {
        let mut chunk = [0u8; 8];
        if file.read_exact(&mut chunk).is_err() {
            break;
        }
        let id = [chunk[0], chunk[1], chunk[2], chunk[3]];
        let size = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]) as usize;
        if &id == b"fmt " {
            let mut body = vec![0u8; size.min(64)];
            if file.read_exact(&mut body).is_err() {
                return Err(wav_error(path, "truncated fmt chunk"));
            }
            let tag = u16::from_le_bytes([body[0], body[1]]);
            channels = u16::from_le_bytes([body[2], body[3]]);
            sample_rate = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
            let bits = u16::from_le_bytes([body[14], body[15]]);
            let tag = if tag == 0xFFFE && body.len() >= 26 {
                // WAVE_FORMAT_EXTENSIBLE: the sub-format GUID starts with the real tag.
                u16::from_le_bytes([body[24], body[25]])
            } else {
                tag
            };
            format = match (tag, bits) {
                (1, 8) => SampleFormat::U8,
                (1, 16) => SampleFormat::I16,
                (1, 24) => SampleFormat::I24,
                (1, 32) => SampleFormat::I32,
                (3, 32) => SampleFormat::F32,
                _ => {
                    return Err(wav_error(
                        path,
                        format!("unsupported WAV encoding tag {tag} / {bits} bit"),
                    ));
                }
            };
            if size % 2 == 1 {
                let _ = file.seek(SeekFrom::Current(1));
            }
        } else if &id == b"data" {
            let position =
                file.seek(SeekFrom::Current(0))
                    .map_err(|error| wav_error(path, error.to_string()))? as usize;
            data_offset = Some(position);
            data_bytes = size.min((file_len as usize).saturating_sub(position));
            break;
        } else {
            // `size` is the chunk body; chunks are word aligned.
            let skip = size as i64 + (size % 2) as i64;
            if file.seek(SeekFrom::Current(skip)).is_err() {
                break;
            }
        }
    }

    let data_offset = data_offset.ok_or_else(|| wav_error(path, "no data chunk"))?;
    if channels != 1 && channels != 2 {
        return Err(wav_error(
            path,
            format!("{channels} channels are not supported (mono and stereo only)"),
        ));
    }
    if sample_rate == 0 {
        return Err(wav_error(path, "sample rate is zero"));
    }
    let bytes_per_frame = channels as usize * format.bytes();
    if bytes_per_frame == 0 {
        return Err(wav_error(path, "empty frame"));
    }
    let frames = data_bytes / bytes_per_frame;
    Ok((
        WavInfo {
            channels,
            sample_rate,
            format,
            data_offset,
            data_bytes,
            frames,
        },
        file,
    ))
}

/// Map a sample file without reading it.
///
/// `preload_bytes` is the amount of audio to touch up front so the first notes do not page-fault;
/// it is the value the program stores, in bytes, converted to frames here because the frame size
/// depends on the file's channel count and format.
pub fn map_wav(path: &Path, preload_bytes: usize) -> Result<AudioSample> {
    let (info, _file) = parse_wav_info(path)?;
    let file = std::fs::File::open(path).map_err(|error| Error::Io {
        path: path.to_path_buf(),
        source: error,
    })?;
    // SAFETY: the mapping is read-only and the file is not modified while it is mapped. A file
    // truncated underneath the mapping would fault, which is why the length is recorded and every
    // read is bounds-checked.
    let map = unsafe { memmap2::Mmap::map(&file) }
        .map_err(|error| wav_error(path, format!("mmap failed: {error}")))?;
    let map = Arc::new(map);

    let frame_bytes = (info.channels as usize).max(1) * info.format.bytes();
    let preload = (preload_bytes / frame_bytes).min(info.frames);
    let mut peak = 0.0f32;
    let mut rms = 0.0f32;
    let sample = AudioSample {
        path: path.to_path_buf(),
        sample_rate: info.sample_rate,
        channels: info.channels,
        frames: info.frames,
        peak: 0.0,
        rms: 0.0,
        // Touching a mapped WAV brings the file's own bytes in as file-backed pages, so the
        // resident cost is the source size, not an f32 copy.
        resident_bytes: (preload * frame_bytes) as u64,
        backing: Backing::Mapped {
            map: Arc::clone(&map),
            data_offset: info.data_offset,
            format: info.format,
        },
    };
    // Touch the preloaded region and measure its peak and level.
    let mut energy = 0.0f64;
    for index in 0..preload {
        let (left, right) = sample.frame(index);
        peak = peak.max(left.abs()).max(right.abs());
        energy += (left as f64).powi(2) + (right as f64).powi(2);
    }
    if preload > 0 {
        rms = (energy / (preload as f64 * 2.0)).sqrt() as f32;
    }
    let _ = &map;
    Ok(AudioSample {
        peak,
        rms,
        ..sample
    })
}

/// Open a sample file, picking the decoder from the extension.
///
/// `.ncw` is NI's compressed container; everything else is treated as RIFF/WAVE.
pub fn open_sample(path: &Path, preload_bytes: usize) -> Result<AudioSample> {
    let is_ncw = path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("ncw"));
    if is_ncw {
        map_ncw(path, preload_bytes)
    } else {
        map_wav(path, preload_bytes)
    }
}

/// Map a `.ncw` and decode it a block at a time.
///
/// The compressed bytes stay in the mapping: a packed library costs what the `.ncw` files take on
/// disk, not four bytes per sample. `preload_frames` frames are decoded at load time so the first
/// note does not wait for the first block and so `peak` means something.
pub fn map_ncw(path: &Path, preload_bytes: usize) -> Result<AudioSample> {
    let file = std::fs::File::open(path).map_err(|error| Error::Io {
        path: path.to_path_buf(),
        source: error,
    })?;
    let map = map_file(path, file)?;
    open_ncw_range(path, path.to_path_buf(), map, 0, None, preload_bytes)
}

/// Map a whole `.ncw` from bytes that are already in memory.
///
/// Used for packed samples: the record is a slice of an `.nkx`/`.nkr`, so the container is mapped
/// and the payload range is handed in.
pub fn map_ncw_range(
    path: &Path,
    container: &Path,
    base: usize,
    length: usize,
    preload_bytes: usize,
) -> Result<AudioSample> {
    let file = std::fs::File::open(container).map_err(|error| Error::Io {
        path: container.to_path_buf(),
        source: error,
    })?;
    let map = map_file(container, file)?;
    open_ncw_range(
        path,
        container.to_path_buf(),
        map,
        base,
        Some(length),
        preload_bytes,
    )
}

fn map_file(path: &Path, file: std::fs::File) -> Result<Arc<memmap2::Mmap>> {
    // SAFETY: the mapping is read-only and the file is not modified while it is mapped. A file
    // truncated underneath the mapping would fault, which is why every read is bounds-checked.
    let map = unsafe { memmap2::Mmap::map(&file) }
        .map_err(|error| wav_error(path, format!("mmap failed: {error}")))?;
    Ok(Arc::new(map))
}

fn open_ncw_range(
    path: &Path,
    source: PathBuf,
    map: Arc<memmap2::Mmap>,
    base: usize,
    length: Option<usize>,
    preload_bytes: usize,
) -> Result<AudioSample> {
    let length = length.unwrap_or(map.len().saturating_sub(base));
    let payload = map
        .get(base..base + length)
        .ok_or_else(|| wav_error(path, "NCW payload range is outside the file"))?;
    let file = crate::ncw::NcwFile::parse(payload).map_err(|error| Error::Ncw {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;
    let info = file.info();
    // The preload budget is in bytes of source audio; a frame is `channels` samples of `bits`.
    let frame_bytes = (info.channels as usize).max(1) * ((info.bits as usize).max(1) / 8);
    let preload = (preload_bytes / frame_bytes.max(1)).min(file.frames());
    let mut peak = 0.0f32;
    let mut energy = 0.0f64;
    let mut counted = 0usize;
    let mut decoded = Vec::with_capacity(crate::ncw::FRAMES_PER_BLOCK * info.channels as usize);
    let mut block = 0;
    let mut reached = 0;
    while reached < preload {
        decoded.clear();
        file.decode_block(payload, block, &mut decoded)
            .map_err(|error| Error::Ncw {
                path: path.to_path_buf(),
                message: error.to_string(),
            })?;
        for value in decoded.iter() {
            peak = peak.max(value.abs());
            energy += (*value as f64).powi(2);
            counted += 1;
        }
        reached += file.block_frames(block);
        block += 1;
    }
    Ok(AudioSample {
        path: path.to_path_buf(),
        sample_rate: info.sample_rate,
        channels: info.channels,
        frames: file.frames(),
        peak,
        rms: if counted > 0 {
            (energy / counted as f64).sqrt() as f32
        } else {
            0.0
        },
        resident_bytes: (decoded.len() * std::mem::size_of::<f32>()) as u64,
        backing: Backing::Ncw {
            map,
            base,
            length,
            file,
            source,
        },
    })
}

/// Read one `.ncw` record out of a container and decode it into memory.
///
/// Impulse responses are read once, end to end, so streaming them would decode every block twice.
pub fn load_ncw_range(container: &Path, base: usize, length: usize) -> Result<AudioSample> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(container).map_err(|error| Error::Io {
        path: container.to_path_buf(),
        source: error,
    })?;
    file.seek(SeekFrom::Start(base as u64))
        .map_err(|error| wav_error(container, error.to_string()))?;
    let mut bytes = vec![0u8; length];
    file.read_exact(&mut bytes)
        .map_err(|error| wav_error(container, format!("truncated record: {error}")))?;
    decode_ncw_bytes(container, &bytes, usize::MAX)
}

/// Decode a `.ncw` into memory. Kept for callers that want the samples up front.
pub fn load_ncw(path: &Path, preload_bytes: usize) -> Result<AudioSample> {
    let bytes = std::fs::read(path).map_err(|error| Error::Io {
        path: path.to_path_buf(),
        source: error,
    })?;
    decode_ncw_bytes(path, &bytes, preload_bytes)
}

/// Decode `.ncw` bytes that came from somewhere other than a file.
pub fn decode_ncw_bytes(path: &Path, bytes: &[u8], preload_bytes: usize) -> Result<AudioSample> {
    let audio = crate::ncw::decode_ncw(bytes).map_err(|error| Error::Ncw {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;
    // `preload_bytes` of source audio, converted to interleaved values.
    let bits = crate::ncw::parse_ncw_info(bytes)
        .map(|info| info.bits as usize)
        .unwrap_or(24);
    let frame_bytes = (audio.channels as usize).max(1) * (bits.max(1) / 8).max(1);
    let preload = (preload_bytes / frame_bytes)
        .saturating_mul(audio.channels as usize)
        .min(audio.samples.len());
    let mut peak = 0.0f32;
    for value in &audio.samples[..preload] {
        peak = peak.max(value.abs());
    }
    Ok(AudioSample {
        path: path.to_path_buf(),
        sample_rate: audio.sample_rate,
        channels: audio.channels,
        frames: audio.frames,
        peak,
        rms: if audio.samples.is_empty() {
            0.0
        } else {
            (audio
                .samples
                .iter()
                .map(|v| (*v as f64).powi(2))
                .sum::<f64>()
                / audio.samples.len() as f64)
                .sqrt() as f32
        },
        resident_bytes: (audio.samples.len() * std::mem::size_of::<f32>()) as u64,
        backing: Backing::Owned(audio.samples),
    })
}

/// Decode a whole file into memory. Used for small files (impulse responses).
pub fn load_wav(path: &Path) -> Result<AudioSample> {
    let (info, _file) = parse_wav_info(path)?;
    let bytes = std::fs::read(path).map_err(|error| Error::Io {
        path: path.to_path_buf(),
        source: error,
    })?;
    let data = &bytes
        .get(info.data_offset..info.data_offset + info.data_bytes)
        .ok_or_else(|| wav_error(path, "data chunk overruns the file"))?;
    let width = info.format.bytes();
    let count = data.len() / width;
    let mut decoded = Vec::with_capacity(count);
    let mut peak = 0.0f32;
    for index in 0..count {
        let at = index * width;
        let bytes = &data[at..at + width];
        let value = match info.format {
            SampleFormat::U8 => (bytes[0] as f32 - 128.0) / 128.0,
            SampleFormat::I16 => i16::from_le_bytes([bytes[0], bytes[1]]) as f32 / 32_768.0,
            SampleFormat::I24 => {
                (i32::from_le_bytes([0, bytes[0], bytes[1], bytes[2]]) >> 8) as f32 / 8_388_608.0
            }
            SampleFormat::I32 => {
                i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f32
                    / 2_147_483_648.0
            }
            SampleFormat::F32 => {
                let value = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                if value.is_finite() { value } else { 0.0 }
            }
        };
        let value = value.clamp(-1.0, 1.0);
        peak = peak.max(value.abs());
        decoded.push(value);
    }
    Ok(AudioSample {
        path: path.to_path_buf(),
        sample_rate: info.sample_rate,
        channels: info.channels,
        frames: info.frames,
        peak,
        rms: if decoded.is_empty() {
            0.0
        } else {
            (decoded.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / decoded.len() as f64).sqrt()
                as f32
        },
        resident_bytes: (decoded.len() * 4) as u64,
        backing: Backing::Owned(decoded),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_wav(path: &Path, channels: u16, sample_rate: u32, samples: &[i16]) {
        let data_len = samples.len() * 2;
        let mut file = std::fs::File::create(path).unwrap();
        file.write_all(b"RIFF").unwrap();
        file.write_all(&((36 + data_len) as u32).to_le_bytes())
            .unwrap();
        file.write_all(b"WAVEfmt ").unwrap();
        file.write_all(&16u32.to_le_bytes()).unwrap();
        file.write_all(&1u16.to_le_bytes()).unwrap();
        file.write_all(&channels.to_le_bytes()).unwrap();
        file.write_all(&sample_rate.to_le_bytes()).unwrap();
        file.write_all(&(sample_rate * channels as u32 * 2).to_le_bytes())
            .unwrap();
        file.write_all(&(channels * 2).to_le_bytes()).unwrap();
        file.write_all(&16u16.to_le_bytes()).unwrap();
        file.write_all(b"data").unwrap();
        file.write_all(&(data_len as u32).to_le_bytes()).unwrap();
        for sample in samples {
            file.write_all(&sample.to_le_bytes()).unwrap();
        }
    }

    fn write_wav24(path: &Path, channels: u16, sample_rate: u32, samples: &[i32]) {
        let data_len = samples.len() * 3;
        let mut file = std::fs::File::create(path).unwrap();
        file.write_all(b"RIFF").unwrap();
        file.write_all(&((36 + data_len) as u32).to_le_bytes())
            .unwrap();
        file.write_all(b"WAVEfmt ").unwrap();
        file.write_all(&16u32.to_le_bytes()).unwrap();
        file.write_all(&1u16.to_le_bytes()).unwrap();
        file.write_all(&channels.to_le_bytes()).unwrap();
        file.write_all(&sample_rate.to_le_bytes()).unwrap();
        file.write_all(&(sample_rate * channels as u32 * 3).to_le_bytes())
            .unwrap();
        file.write_all(&(channels * 3).to_le_bytes()).unwrap();
        file.write_all(&24u16.to_le_bytes()).unwrap();
        file.write_all(b"data").unwrap();
        file.write_all(&(data_len as u32).to_le_bytes()).unwrap();
        for sample in samples {
            let bytes = sample.to_le_bytes();
            file.write_all(&bytes[0..3]).unwrap();
        }
    }

    /// 24-bit is the format the corpus uses; a missing sign extension turns a string section
    /// into loud noise.
    #[test]
    fn twenty_four_bit_keeps_sign_and_scale() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("openkontakt-wav24-{}.wav", std::process::id()));
        let values: Vec<i32> = vec![0, 1, -1, 8_388_607, -8_388_608, 4_194_304, -4_194_304];
        write_wav24(&path, 1, 48_000, &values);

        let owned = load_wav(&path).unwrap();
        assert_eq!(owned.frames, values.len());
        for (index, expected) in values.iter().enumerate() {
            let (left, _) = owned.frame(index);
            let want = *expected as f32 / 8_388_608.0;
            assert!(
                (left - want).abs() < 1e-6,
                "frame {index}: got {left}, want {want}"
            );
        }
        assert!(owned.frame(1).0 < 0.001, "small positive must stay small");
        assert!(owned.frame(2).0 > -0.001, "small negative must stay small");

        let mapped = map_wav(&path, 4_096).unwrap();
        for index in 0..values.len() {
            assert!((mapped.frame(index).0 - owned.frame(index).0).abs() < 1e-6);
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn mapped_and_owned_reads_agree() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("openkontakt-wav-{}.wav", std::process::id()));
        let samples: Vec<i16> = (0..64).map(|i| (i as i16) * 500 - 16_000).collect();
        write_wav(&path, 2, 48_000, &samples);

        let owned = load_wav(&path).unwrap();
        let mapped = map_wav(&path, 4_096).unwrap();
        assert_eq!(owned.frames, mapped.frames);
        assert_eq!(owned.channels, mapped.channels);
        assert_eq!(owned.sample_rate, mapped.sample_rate);
        for frame in 0..owned.frames {
            let a = owned.frame(frame);
            let b = mapped.frame(frame);
            assert!((a.0 - b.0).abs() < 1e-6, "left frame {frame}");
            assert!((a.1 - b.1).abs() < 1e-6, "right frame {frame}");
        }
        assert!(mapped.peak > 0.0);
        let _ = std::fs::remove_file(&path);
    }
}

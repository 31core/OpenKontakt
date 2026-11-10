//! Offline rendering: instrument + MIDI file in, a WAV out, no audio device and no GUI.
//!
//! This is the other half of a differential test against Kontakt. A reference render from Kontakt
//! is captured once (any host will do); everything after that — our render, the comparison, the
//! next iteration — runs from the command line and is bit-for-bit reproducible.

use crate::engine::Engine;
use crate::smf::{Message, read_midi_file};
use std::path::Path;
use std::sync::Arc;

pub struct RenderOptions {
    pub sample_rate: f32,
    /// Extra seconds rendered after the last MIDI event so releases and reverb tails are captured.
    pub tail_seconds: f32,
    /// Master gain applied to the rendered buffer.
    pub gain: f32,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            sample_rate: 48_000.0,
            tail_seconds: 3.0,
            gain: 1.0,
        }
    }
}

pub struct Render {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
    pub sample_rate: f32,
    /// Diagnostics from the loader, so a render says why it is silent instead of just being silent.
    pub diagnostics: Vec<String>,
}

impl Render {
    pub fn frames(&self) -> usize {
        self.left.len()
    }

    pub fn peak(&self) -> f32 {
        self.left
            .iter()
            .chain(self.right.iter())
            .fold(0.0f32, |peak, value| peak.max(value.abs()))
    }
}

#[derive(Debug)]
pub struct RenderError(pub String);

impl std::fmt::Display for RenderError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

impl std::error::Error for RenderError {}

pub fn render(
    instrument_path: &Path,
    midi_path: &Path,
    options: &RenderOptions,
) -> Result<Render, RenderError> {
    let loaded = crate::state::load_instrument(instrument_path)
        .map_err(|source| RenderError(format!("{}: {source}", instrument_path.display())))?;
    let diagnostics: Vec<String> = loaded
        .diagnostics
        .iter()
        .map(|diagnostic| {
            format!(
                "{:?}: {}: {}",
                diagnostic.severity, diagnostic.area, diagnostic.message
            )
        })
        .collect();
    let midi = read_midi_file(midi_path)
        .map_err(|source| RenderError(format!("{}: {source}", midi_path.display())))?;

    let sample_rate = options.sample_rate.max(1.0);
    let mut engine = Engine::default();
    engine.set_sample_rate(sample_rate);
    // The plugin drives the interpreter on a worker thread; the blocking path exists for
    // determinism. Being able to render both ways is how a difference between the plugin and an
    // offline render gets attributed to the threading.
    // Keep the memory figures before the loaded instrument moves into the engine.
    let sample_count = loaded.samples.len();
    let streaming_count = loaded
        .samples
        .values()
        .filter(|sample| sample.is_streaming())
        .count();
    let loaded_bytes = loaded.bytes_loaded;
    let mapped_bytes = loaded.bytes_mapped;
    let threaded = std::env::var("OPENKONTAKT_THREADED").is_ok();
    let loaded = Arc::new(loaded);
    if threaded {
        engine.set_instrument(Some(Arc::clone(&loaded)));
    } else {
        engine.set_instrument_blocking(Arc::clone(&loaded));
    }
    // Same reason as the interpreter: the impulse response is built here, not on a loader thread,
    // so the render is deterministic and the reverb is present from the first sample.
    engine.load_rack_ir_blocking();
    // Panel controls can be pressed before rendering, which is how an effect switch that only
    // exists behind a KSP `on ui_control` handler is exercised without a UI.
    if let Ok(name) = std::env::var("OPENKONTAKT_PRESS") {
        let value: i64 = std::env::var("OPENKONTAKT_PRESS_VALUE")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(1);
        for slot in 0..loaded.ksp.slot_count() {
            if loaded.ksp.set_control_value(slot, &name, value) {
                eprintln!("debug: pressed {name}={value} on slot {slot}");
            }
        }
    }
    if std::env::var("OPENKONTAKT_NO_REVERB").is_ok() {
        engine.disable_reverb();
    }
    if std::env::var("OPENKONTAKT_RENDER_DEBUG").is_ok() {
        let (ir, conv, send) = engine.reverb_debug();
        eprintln!("debug: rack_ir={ir:?} convolver={conv} send_level={send:.4}");
        // Streaming is only worth anything if the samples are not decoded up front, so say
        // plainly how much of each is held.
        eprintln!(
            "debug: samples={sample_count} streaming={streaming_count} loaded={:.2} MB mapped={:.2} MB",
            loaded_bytes as f64 / (1024.0 * 1024.0),
            mapped_bytes as f64 / (1024.0 * 1024.0)
        );
    }

    let frames =
        ((midi.duration + options.tail_seconds as f64) * sample_rate as f64).ceil() as usize;
    let mut left = Vec::with_capacity(frames);
    let mut right = Vec::with_capacity(frames);
    let mut pending = midi.messages.iter().peekable();
    let block = 64usize;
    let mut frame = 0usize;
    while frame < frames {
        if frame % block == 0 {
            engine.prepare_block();
        }
        if threaded {
            engine.poll_script();
        }
        // Everything due at or before this sample, in file order. Two events that land on the same
        // sample keep their order because the list is already sorted by time.
        while let Some(message) = pending.peek() {
            let at = (message.seconds * sample_rate as f64).round() as usize;
            if at > frame {
                break;
            }
            let message = message.message;
            pending.next();
            match message {
                Message::NoteOn { key, velocity } => {
                    if threaded {
                        engine.note_on(key, velocity as f32 / 127.0);
                    } else {
                        engine.note_on_blocking(key, velocity as f32 / 127.0);
                    }
                    if std::env::var("OPENKONTAKT_RENDER_DEBUG").is_ok() {
                        eprintln!(
                            "debug: note {key} vel {velocity} -> {:?}",
                            engine.debug_voices()
                        );
                    }
                }
                Message::NoteOff { key, .. } => {
                    if threaded {
                        engine.note_off(key);
                    } else {
                        engine.note_off_blocking(key);
                    }
                }
                Message::ControlChange { number, value } => {
                    engine.controller_blocking(number as i64, value as i64)
                }
                // The engine has no use for the rest yet; consuming them keeps the schedule honest.
                Message::PitchBend { .. }
                | Message::ProgramChange { .. }
                | Message::ChannelPressure { .. } => {}
            }
        }
        if std::env::var("OPENKONTAKT_RENDER_DEBUG").is_ok() && frame % 24_000 == 0 && frame > 0 {
            eprintln!(
                "debug: t={:.2}s voices {:?}",
                frame as f32 / sample_rate,
                engine.debug_voices()
            );
        }
        let (l, r) = engine.render_sample();
        left.push(l * options.gain);
        right.push(r * options.gain);
        frame += 1;
    }

    if std::env::var("OPENKONTAKT_RENDER_DEBUG").is_ok() {
        let (_, conv, send) = engine.reverb_debug();
        eprintln!("debug: after render convolver={conv} send_level={send:.4}");
    }
    Ok(Render {
        left,
        right,
        sample_rate,
        diagnostics,
    })
}

/// Write 32-bit float stereo PCM. Float keeps the diff honest: no dither, no clipping surprises,
/// and no question about what the reference's container did to the sample values.
pub fn write_wav(path: &Path, render: &Render) -> std::io::Result<()> {
    use std::io::Write;

    let frames = render.frames();
    let channels = 2u16;
    let bits = 32u16;
    let block_align = channels * bits / 8;
    let data_bytes = frames as u32 * block_align as u32;
    let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);
    file.write_all(b"RIFF")?;
    file.write_all(&(36 + data_bytes).to_le_bytes())?;
    file.write_all(b"WAVE")?;
    file.write_all(b"fmt ")?;
    file.write_all(&16u32.to_le_bytes())?;
    file.write_all(&3u16.to_le_bytes())?; // IEEE float
    file.write_all(&channels.to_le_bytes())?;
    file.write_all(&(render.sample_rate as u32).to_le_bytes())?;
    file.write_all(&((render.sample_rate as u32) * block_align as u32).to_le_bytes())?;
    file.write_all(&block_align.to_le_bytes())?;
    file.write_all(&bits.to_le_bytes())?;
    file.write_all(b"data")?;
    file.write_all(&data_bytes.to_le_bytes())?;
    let mut buffer = Vec::with_capacity(4096);
    for index in 0..frames {
        for value in [render.left[index], render.right[index]] {
            buffer.extend_from_slice(&value.to_le_bytes());
        }
        if buffer.len() >= 4096 {
            file.write_all(&buffer)?;
            buffer.clear();
        }
    }
    file.write_all(&buffer)?;
    file.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A library whose program rack holds a convolution module must render its reverb tail. Before
    /// the `0x16` module was decoded the send went nowhere: the render ended in digital silence
    /// the instant the last release finished, which is exactly what a differential against Kontakt
    /// reports as a −90 dB tail.
    #[test]
    #[ignore = "requires a library whose program rack has a convolution slot"]
    fn renders_the_program_reverb_tail() {
        let instrument = std::env::var("OPENKONTAKT_REVERB_NKI")
            .expect("set OPENKONTAKT_REVERB_NKI to such a library");
        let directory = std::env::temp_dir().join("openkontakt-reverb-test");
        std::fs::create_dir_all(&directory).unwrap();
        let midi_path = directory.join("note.mid");

        // One note, one beat, 480 ticks per quarter at 120 bpm.
        let mut track = Vec::new();
        track.extend_from_slice(&[0x00, 0x90, 60, 100]);
        track.extend_from_slice(&[0x83, 0x60, 0x80, 60, 0]);
        track.extend_from_slice(&[0x00, 0xFF, 0x2F, 0x00]);
        let mut midi = b"MThd".to_vec();
        midi.extend_from_slice(&6u32.to_be_bytes());
        midi.extend_from_slice(&1u16.to_be_bytes());
        midi.extend_from_slice(&1u16.to_be_bytes());
        midi.extend_from_slice(&480u16.to_be_bytes());
        midi.extend_from_slice(b"MTrk");
        midi.extend_from_slice(&(track.len() as u32).to_be_bytes());
        midi.extend_from_slice(&track);
        std::fs::write(&midi_path, midi).unwrap();

        let options = RenderOptions {
            sample_rate: 48_000.0,
            tail_seconds: 2.0,
            gain: 1.0,
        };
        let render =
            render(std::path::Path::new(&instrument), &midi_path, &options).expect("render");

        // The note is released after 0.5 s and its own release ends well before 1.0 s, so
        // anything left after 1.5 s can only be the convolution return.
        let from = (0.8 * 48_000.0) as usize;
        let to = (1.0 * 48_000.0) as usize;
        let tail = &render.left[from.min(render.frames())..to.min(render.frames())];
        let energy: f64 = tail.iter().map(|value| (*value as f64).powi(2)).sum();
        let rms = (energy / tail.len().max(1) as f64).sqrt();
        let dbfs = 20.0 * rms.max(1e-12).log10();
        assert!(
            dbfs > -70.0,
            "the reverb tail is silent ({dbfs:.1} dBFS); the convolution module is not being fed"
        );
    }

    #[test]
    fn writes_a_float_wav_with_the_right_header() {
        let directory = std::env::temp_dir().join("openkontakt-offline-test");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("render.wav");
        let render = Render {
            left: vec![0.25, -0.5, 0.0],
            right: vec![-0.25, 0.5, 0.0],
            sample_rate: 48_000.0,
            diagnostics: Vec::new(),
        };
        write_wav(&path, &render).unwrap();
        let parsed = openkontakt::wav::parse_wav_info(&path).unwrap().0;
        assert_eq!(parsed.sample_rate, 48_000);
        assert_eq!(parsed.channels, 2);
        assert_eq!(parsed.frames, 3);
        let mapped = openkontakt::wav::map_wav(&path, 0).unwrap();
        assert!((mapped.frame(0).0 - 0.25).abs() < 1e-6);
        assert!((mapped.frame(1).1 - 0.5).abs() < 1e-6);
    }
}

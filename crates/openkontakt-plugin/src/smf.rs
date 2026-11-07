//! Minimal Standard MIDI File reader, for offline rendering.
//!
//! Only what a sampler needs: note on/off, controllers, and the tempo map. Everything else is
//! skipped after being framed correctly (a parser that desynchronises turns a file into noise).

use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Message {
    NoteOn {
        key: u8,
        velocity: u8,
    },
    NoteOff {
        key: u8,
        velocity: u8,
    },
    ControlChange {
        number: u8,
        value: u8,
    },
    PitchBend {
        value: i32,
    },
    ProgramChange {
        program: u8,
    },
    /// `(channel, key, pressure)`.
    ChannelPressure {
        pressure: u8,
    },
}

#[derive(Clone, Copy, Debug)]
pub struct TimedMessage {
    /// Seconds from the start of the sequence.
    pub seconds: f64,
    pub message: Message,
}

#[derive(Clone, Debug, Default)]
pub struct MidiFile {
    pub messages: Vec<TimedMessage>,
    /// The sequence length, i.e. the time of the last event plus its own duration.
    pub duration: f64,
    pub ticks_per_quarter: u16,
    pub tempo_changes: usize,
}

#[derive(Debug)]
pub struct MidiError(pub String);

impl std::fmt::Display for MidiError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

impl std::error::Error for MidiError {}

fn error(message: impl Into<String>) -> MidiError {
    MidiError(message.into())
}

pub fn read_midi_file(path: &Path) -> Result<MidiFile, MidiError> {
    let bytes =
        std::fs::read(path).map_err(|source| error(format!("{}: {source}", path.display())))?;
    parse_midi(&bytes)
}

/// A raw event with a tick timestamp, before the tempo map is applied.
enum Raw {
    Ticks(u64, Message),
    Tempo(u64, u32),
}

pub fn parse_midi(bytes: &[u8]) -> Result<MidiFile, MidiError> {
    if bytes.len() < 14 || &bytes[0..4] != b"MThd" {
        return Err(error("not a Standard MIDI File"));
    }
    let header_length = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
    if header_length < 6 || bytes.len() < 8 + header_length {
        return Err(error("truncated MIDI header"));
    }
    let format = u16::from_be_bytes([bytes[8], bytes[9]]);
    let tracks = u16::from_be_bytes([bytes[10], bytes[11]]) as usize;
    let division = i16::from_be_bytes([bytes[12], bytes[13]]);
    if format > 2 {
        return Err(error(format!("unsupported MIDI format {format}")));
    }
    if division <= 0 {
        return Err(error("SMPTE time division is not supported"));
    }
    let ticks_per_quarter = division as u16;

    let mut at = 8 + header_length;
    let mut raw: Vec<Raw> = Vec::new();
    for _ in 0..tracks {
        if at + 8 > bytes.len() || &bytes[at..at + 4] != b"MTrk" {
            return Err(error("expected an MTrk chunk"));
        }
        let length =
            u32::from_be_bytes([bytes[at + 4], bytes[at + 5], bytes[at + 6], bytes[at + 7]])
                as usize;
        let start = at + 8;
        let end = start
            .checked_add(length)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(|| error("truncated MIDI track"))?;
        parse_track(&bytes[start..end], &mut raw)?;
        at = end;
    }

    // Tempo map: every tempo change applies until the next one.
    raw.sort_by_key(|event| match event {
        Raw::Ticks(tick, _) | Raw::Tempo(tick, _) => *tick,
    });
    let mut seconds = 0.0f64;
    let mut last_tick = 0u64;
    let mut micros_per_quarter = 500_000f64;
    let mut tempo_changes = 0usize;
    let mut messages = Vec::new();
    let mut duration = 0.0f64;
    for event in raw {
        match event {
            Raw::Ticks(tick, message) => {
                seconds += tick_duration(tick - last_tick, micros_per_quarter, ticks_per_quarter);
                last_tick = tick;
                duration = duration.max(seconds);
                messages.push(TimedMessage { seconds, message });
            }
            Raw::Tempo(tick, tempo) => {
                seconds += tick_duration(tick - last_tick, micros_per_quarter, ticks_per_quarter);
                last_tick = tick;
                micros_per_quarter = tempo as f64;
                tempo_changes += 1;
            }
        }
    }

    Ok(MidiFile {
        messages,
        duration,
        ticks_per_quarter,
        tempo_changes,
    })
}

fn tick_duration(ticks: u64, micros_per_quarter: f64, ticks_per_quarter: u16) -> f64 {
    if ticks == 0 || ticks_per_quarter == 0 {
        return 0.0;
    }
    (ticks as f64 / ticks_per_quarter as f64) * (micros_per_quarter / 1_000_000.0)
}

fn parse_track(bytes: &[u8], out: &mut Vec<Raw>) -> Result<(), MidiError> {
    let mut at = 0usize;
    let mut tick = 0u64;
    let mut running_status: Option<u8> = None;
    while at < bytes.len() {
        let (delta, consumed) = read_varlen(&bytes[at..])?;
        at += consumed;
        tick += delta as u64;
        if at >= bytes.len() {
            break;
        }
        let first = bytes[at];
        let status = if first & 0x80 != 0 {
            at += 1;
            if first < 0xF0 {
                running_status = Some(first);
            }
            first
        } else {
            running_status.ok_or_else(|| error("MIDI running status without a status byte"))?
        };

        match status {
            0xFF => {
                let kind = *bytes.get(at).ok_or_else(|| error("truncated meta event"))?;
                at += 1;
                let (length, consumed) = read_varlen(&bytes[at..])?;
                at += consumed;
                let end = at + length as usize;
                if end > bytes.len() {
                    return Err(error("truncated meta event payload"));
                }
                if kind == 0x51 && length == 3 {
                    let tempo = u32::from_be_bytes([0, bytes[at], bytes[at + 1], bytes[at + 2]]);
                    out.push(Raw::Tempo(tick, tempo.max(1)));
                }
                at = end;
            }
            0xF0 | 0xF7 => {
                let (length, consumed) = read_varlen(&bytes[at..])?;
                at += consumed + length as usize;
                if at > bytes.len() {
                    return Err(error("truncated sysex"));
                }
            }
            _ => {
                let data_length = match status & 0xF0 {
                    0xC0 | 0xD0 => 1,
                    _ => 2,
                };
                if at + data_length > bytes.len() {
                    return Err(error("truncated channel message"));
                }
                let a = bytes[at];
                let b = if data_length > 1 { bytes[at + 1] } else { 0 };
                at += data_length;
                if let Some(message) = channel_message(status & 0xF0, a, b) {
                    out.push(Raw::Ticks(tick, message));
                }
            }
        }
    }
    Ok(())
}

fn channel_message(kind: u8, a: u8, b: u8) -> Option<Message> {
    Some(match kind {
        0x80 => Message::NoteOff {
            key: a & 0x7F,
            velocity: b & 0x7F,
        },
        // A note-on with velocity 0 is a note-off; several exporters rely on it.
        0x90 if b & 0x7F == 0 => Message::NoteOff {
            key: a & 0x7F,
            velocity: 0,
        },
        0x90 => Message::NoteOn {
            key: a & 0x7F,
            velocity: b & 0x7F,
        },
        0xB0 => Message::ControlChange {
            number: a & 0x7F,
            value: b & 0x7F,
        },
        0xD0 => Message::ChannelPressure { pressure: a & 0x7F },
        0xC0 => Message::ProgramChange { program: a & 0x7F },
        0xE0 => Message::PitchBend {
            value: (((b & 0x7F) as i32) << 7 | (a & 0x7F) as i32) - 8192,
        },
        _ => return None,
    })
}

fn read_varlen(bytes: &[u8]) -> Result<(u32, usize), MidiError> {
    let mut value = 0u32;
    for (index, byte) in bytes.iter().take(4).enumerate() {
        value = (value << 7) | (byte & 0x7F) as u32;
        if byte & 0x80 == 0 {
            return Ok((value, index + 1));
        }
    }
    Err(error("unterminated variable-length quantity"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(events: &[u8]) -> Vec<u8> {
        let mut out = b"MTrk".to_vec();
        out.extend_from_slice(&(events.len() as u32).to_be_bytes());
        out.extend_from_slice(events);
        out
    }

    fn file(tracks: &[Vec<u8>], division: u16) -> Vec<u8> {
        let mut out = b"MThd".to_vec();
        out.extend_from_slice(&6u32.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&(tracks.len() as u16).to_be_bytes());
        out.extend_from_slice(&division.to_be_bytes());
        for track in tracks {
            out.extend_from_slice(track);
        }
        out
    }

    #[test]
    fn reads_notes_against_the_tempo_map() {
        // 480 ticks per quarter, 500 000 us per quarter -> 480 ticks = 0.5 s.
        let events = [
            0x00, 0x90, 60, 100, // tick 0: note on
            0x83, 0x60, 0x80, 60, 0, // +480 ticks: note off
            0x00, 0xFF, 0x51, 0x03, 0x07, 0xA1, 0x20, // tempo 500 000
            0x83, 0x60, 0x90, 62, 100, // +480 ticks: another note on
            0x00, 0xFF, 0x2F, 0x00,
        ];
        let parsed = parse_midi(&file(&[track(&events)], 480)).unwrap();
        assert_eq!(parsed.tempo_changes, 1);
        let notes: Vec<&TimedMessage> = parsed
            .messages
            .iter()
            .filter(|m| matches!(m.message, Message::NoteOn { .. } | Message::NoteOff { .. }))
            .collect();
        assert_eq!(notes.len(), 3);
        assert!((notes[0].seconds - 0.0).abs() < 1e-9);
        assert!((notes[1].seconds - 0.5).abs() < 1e-9);
        assert!((notes[2].seconds - 1.0).abs() < 1e-9);
    }

    #[test]
    fn running_status_and_velocity_zero_note_off() {
        let events = [
            0x00, 0x90, 60, 100, // note on
            0x60, 62, 100, // running status: another note on
            0x60, 60, 0, // running status, velocity 0 == note off
            0x00, 0xFF, 0x2F, 0x00,
        ];
        let parsed = parse_midi(&file(&[track(&events)], 96)).unwrap();
        assert!(matches!(
            parsed.messages[0].message,
            Message::NoteOn { key: 60, .. }
        ));
        assert!(matches!(
            parsed.messages[1].message,
            Message::NoteOn { key: 62, .. }
        ));
        assert!(matches!(
            parsed.messages[2].message,
            Message::NoteOff { key: 60, .. }
        ));
    }

    #[test]
    fn a_broken_file_is_an_error_not_a_panic() {
        assert!(parse_midi(b"junk").is_err());
        let truncated = file(&[track(&[0x00, 0x90, 60])], 480);
        assert!(parse_midi(&truncated).is_err());
    }
}

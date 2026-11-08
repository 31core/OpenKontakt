use std::path::PathBuf;

/// Errors produced while reading an instrument. Every error names the layer it came from so a
/// malformed file can be diagnosed without a debugger.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("not a Kontakt instrument: {0}")]
    NotAnInstrument(String),

    #[error("unsupported: {0}")]
    Unsupported(String),

    #[error("malformed {layer} at 0x{offset:X}: {message}")]
    Malformed {
        layer: &'static str,
        offset: usize,
        message: String,
    },

    #[error("truncated {layer} at 0x{offset:X}: need {needed} bytes, have {available}")]
    Truncated {
        layer: &'static str,
        offset: usize,
        needed: usize,
        available: usize,
    },

    #[error(
        "FastLZ decompression failed: expected {expected} bytes, produced {produced}: {reason}"
    )]
    FastLz {
        expected: usize,
        produced: usize,
        reason: String,
    },

    #[error("encrypted preset: the library key is required")]
    EncryptedPreset,

    #[error("no Program (0x28) chunk found")]
    NoProgram,

    #[error("no filename table found")]
    NoFileNameTable,

    #[error("WAV decode error for {path}: {message}")]
    Wav { path: PathBuf, message: String },

    #[error("NCW decode error for {path}: {message}")]
    Ncw { path: PathBuf, message: String },
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn malformed(layer: &'static str, offset: usize, message: impl Into<String>) -> Self {
        Self::Malformed {
            layer,
            offset,
            message: message.into(),
        }
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::Unsupported(message.into())
    }
}

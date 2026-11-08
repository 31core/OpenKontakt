#![allow(clippy::collapsible_if)]

//! OpenKontakt core: a clean-room reader for Kontakt `.nki` instruments with loose WAV samples.
//!
//! The supported path is intentionally narrow and explicit:
//!
//! ```text
//! .nki NISound item tree
//!   -> EncryptionItem -> SubtreeItem (FastLZ) -> PresetChunkItem
//!   -> chunk stream
//!       0x28 Program -> groups 0x33, zones 0x34, scripts 0x06
//!       0x4B FileNameTable -> resolved .wav files
//! ```
//!
//! NKX monoliths, NKR resources, NCW-compressed samples and encrypted libraries are out of scope
//! and are marked with `unimplemented!()` / explicit errors rather than guessed at.

pub mod archive;
pub mod chunk;
pub mod dsp;
pub mod effects;
pub mod envelope;
pub mod error;
pub mod fastlz;
pub mod filename;
pub mod future;
pub mod group;
pub mod instrument;
pub mod ksp;
pub mod ksp_ui;
pub mod ncw;
pub mod nis;
pub mod nkc;
pub mod program;
pub mod reader;
pub mod resolve;
pub mod script;
pub mod wav;
pub mod zone;

pub use envelope::Ahdsr;
pub use error::{Error, Result};
pub use instrument::{
    Diagnostic, DiagnosticSeverity, Instrument, InstrumentStats, LoadedSamples, OtherResource,
    load_samples,
};
pub use ksp::{EngineState, KspRuntime, UiControl};
pub use ksp_ui::{KspControl, KspControlKind, KspPanel};
pub use program::ProgramParams;
pub use wav::{AudioSample, SampleCursor};

/// Version of the core reader.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

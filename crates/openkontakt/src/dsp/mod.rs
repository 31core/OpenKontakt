//! Real-time DSP building blocks for Kontakt-style insert effects.
//!
//! The modules here are intentionally self-contained and allocation-free while processing:
//! [`Biquad`] provides a stereo direct-form-1 filter and [`Convolver`] provides a uniform
//! partitioned overlap-save convolution reverb.
//!
//! This module is not yet wired into `lib.rs`; it is expected to be exposed as `pub mod dsp;`
//! by the crate root once consumers exist.
#![allow(dead_code)]

pub mod biquad;
pub mod convolution;
pub mod voice_filter;

pub use biquad::{Biquad, BiquadKind, MAX_CHANNELS};
pub use convolution::Convolver;
pub use voice_filter::cutoff_hz as voice_filter_cutoff_hz;

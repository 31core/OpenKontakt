//! Stubs for the containers that are still out of scope.
//!
//! These deliberately panic instead of guessing at binary layouts that are not specified yet. The
//! surrounding APIs are shaped so they can be implemented later without changing the reader model.
//!
//! `.nkr` resource archives are **not** fully unimplemented: `archive.rs` reads their file table,
//! which is what the artwork needs. What is still missing is sample data stored inside an archive
//! rather than in loose `.wav` files.

use std::path::Path;

/// A sample that lives inside `.nkx` monolith instead of a loose `.wav` on disk.
pub fn load_nkx_sample(_archive: &Path, _member: &str) -> ! {
    unimplemented!("NKX sample monoliths are not implemented in OpenKontakt yet")
}

/// Sample data pulled out of a `.nkr` resource archive rather than a loose `.wav` on disk.
///
/// The archive's *file table* is read (`archive.rs`) for artwork; its sample payloads are not.
pub fn load_nkr_sample(_archive: &Path, _member: &str) -> ! {
    unimplemented!("sample data inside NKR archives is not implemented in OpenKontakt yet")
}

/// `.ncw` compressed sample decoding.
pub fn load_ncw_sample(_path: &Path) -> ! {
    unimplemented!("NCW sample decoding is not implemented in OpenKontakt yet")
}

//! Stubs for the documented-but-out-of-scope containers.
//!
//! These deliberately panic instead of guessing at binary layouts that are not specified yet. The
//! surrounding APIs are shaped so they can be implemented later without changing the reader model.

use std::path::Path;

/// `.nkx` monolith member lookup (`docs/05-nkx-monolith.md`).
pub fn load_nkx_sample(_archive: &Path, _member: &str) -> ! {
    unimplemented!("NKX monoliths are documented but not implemented in OpenKontakt yet")
}

/// `.nkr` resource archive lookup (`docs/06-nkr-resources.md`).
pub fn load_nkr_resource(_archive: &Path, _member: &str) -> ! {
    unimplemented!("NKR resource archives are documented but not implemented in OpenKontakt yet")
}

/// `.ncw` compressed sample decoding (`docs/07-ncw-samples.md`).
pub fn load_ncw_sample(_path: &Path) -> ! {
    unimplemented!("NCW sample decoding is documented but not implemented in OpenKontakt yet")
}

/// KSP bytecode/text execution and effect-parameter binding.
pub fn execute_ksp_script(_source: &str) -> ! {
    unimplemented!(
        "KSP execution is out of scope. The parser exposes script text and a read-only UI \
         declaration mirror; a future interpreter must implement this function"
    )
}

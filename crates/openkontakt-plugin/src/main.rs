#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use nice_plug::prelude::*;
use openkontakt_plugin::OpenKontakt;

fn main() {
    // The standalone has no host to restore state from. Accept the instrument path as the first
    // positional argument, then hand the remaining arguments to nice-plug's standalone wrapper.
    let mut args: Vec<String> = std::env::args().collect();
    if let Some(index) = args
        .iter()
        .position(|arg| !arg.starts_with('-') && arg.to_ascii_lowercase().ends_with(".nki"))
    {
        let path = args.remove(index);
        // SAFETY: this happens before any audio or GUI thread is spawned by the wrapper.
        unsafe { std::env::set_var("OPENKONTAKT_INSTRUMENT", path) };
    }
    nice_export_standalone_with_args::<OpenKontakt, _>(args);
}

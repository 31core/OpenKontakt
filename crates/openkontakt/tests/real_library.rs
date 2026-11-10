use openkontakt::{Instrument, load_samples};

/// Optional end-to-end check against a retail strings library. Run with:
///
/// ```text
/// OPENKONTAKT_STRINGS_NKI="…/instrument.nki" cargo test -p openkontakt --test real_library -- --ignored
/// ```
#[test]
#[ignore = "requires a retail .nki + loose WAV library"]
fn strings_instrument_matches_verified_fixture() {
    let path = std::env::var("OPENKONTAKT_STRINGS_NKI")
        .expect("set OPENKONTAKT_STRINGS_NKI to the instrument to test");
    let instrument = Instrument::load(&path).expect("instrument should parse");
    assert_eq!(instrument.groups.len(), 8);
    assert_eq!(instrument.zones.len(), 96);
    assert_eq!(instrument.program.version, 0xAB);
    assert_eq!(
        instrument.program.params.name,
        std::path::Path::new(&path)
            .file_stem()
            .map(|stem| stem.to_string_lossy().to_string())
            .unwrap_or_default()
    );
    assert_eq!(instrument.program.private.len(), 356);
    assert_eq!(
        instrument.scripts.iter().filter(|s| !s.is_empty()).count(),
        1
    );
    assert_eq!(instrument.resolved_samples.len(), 96);
    let first = &instrument.zones[0];
    assert_eq!(first.group_index, 0);
    assert_eq!(first.low_key, 24);
    assert_eq!(first.high_key, 24);
    assert_eq!(first.root_key, 24);
    assert_eq!(first.sample.as_ref().unwrap().filename_id, 0);
    assert_eq!(first.sample.as_ref().unwrap().frames, 660_608);

    assert_eq!(instrument.program.params.wallpaper_filename, Some(1));
    let wallpaper = instrument
        .wallpaper_path
        .as_ref()
        .expect("the library should ship a wallpaper");
    assert!(
        wallpaper
            .to_string_lossy()
            .to_ascii_lowercase()
            .contains("wallpaper")
    );
    assert!(wallpaper.is_file());

    let loaded = load_samples(&instrument);
    assert_eq!(loaded.samples.len(), 96);
    // Samples are streamed: the whole library is mapped, but only the DFD preload is resident.
    assert!(
        loaded.bytes_mapped > 300_000_000,
        "mapped {}",
        loaded.bytes_mapped
    );
    assert!(
        loaded.bytes_loaded < 100_000_000,
        "resident {}",
        loaded.bytes_loaded
    );
    assert!(loaded.bytes_loaded > 0);
}

/// The panel script must actually run: `on init` declares the controls, and moving a mic slider
/// must reach the engine's bus volume.
///
/// ```text
/// OPENKONTAKT_STRINGS_NKI="…/instrument.nki" cargo test -p openkontakt --test real_library -- --ignored
/// ```
#[test]
#[ignore = "requires a retail .nki + loose WAV library"]
fn strings_panel_script_drives_bus_volume() {
    let path = std::env::var("OPENKONTAKT_STRINGS_NKI")
        .expect("set OPENKONTAKT_STRINGS_NKI to the instrument to test");
    let instrument = Instrument::load(&path).expect("instrument should parse");
    let root = instrument
        .source_path
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let texts: Vec<Option<String>> = instrument
        .scripts
        .iter()
        .map(|slot| slot.text.clone())
        .collect();
    let runtime = openkontakt::KspRuntime::compile(&texts, &root);
    runtime.initialize();
    assert!(
        runtime.diagnostics().is_empty(),
        "{:?}",
        runtime.diagnostics()
    );

    let controls = runtime.controls(0);
    assert!(
        controls.len() >= 20,
        "the panel declares 21 controls, got {}",
        controls.len()
    );
    let mic = controls
        .iter()
        .find(|c| c.name == "close_mic_slider")
        .expect("close mic slider");
    assert_eq!(mic.max, 630_000);

    // Restore the saved values, exactly as the loader does.
    for slot in &instrument.scripts {
        runtime.apply_persistent(
            slot.index,
            &slot
                .persistent
                .iter()
                .map(|entry| (entry.name.clone(), entry.values.clone()))
                .collect::<Vec<_>>(),
        );
    }
    // The saved state persists close=0, stereo pair=630000, outriggers=0, room=0.
    assert!(runtime.engine.bus_gain(1) > 0.99, "stereo pair bus");
    assert!(runtime.engine.bus_gain(0) < 0.01, "close bus muted");

    // Drag the close mic slider to the top: bus 0 must open up.
    assert!(runtime.set_control_normalized(0, "close_mic_slider", 1.0));
    assert!(runtime.engine.bus_gain(0) > 0.99, "close bus after drag");
}

/// A multi-microphone panel is the harder script: arrays, `on note` / `on release`, `select`, and
/// pgs persistence keys. It must compile and initialize cleanly even though playback callbacks are
/// not wired yet.
///
/// ```text
/// OPENKONTAKT_MULTIMIC_NKI="…/instrument.nki" cargo test -p openkontakt --test real_library -- --ignored
/// ```
#[test]
#[ignore = "requires a retail .nki + loose WAV library"]
fn multimic_panels_compile_and_initialize() {
    let path = std::env::var("OPENKONTAKT_MULTIMIC_NKI")
        .expect("set OPENKONTAKT_MULTIMIC_NKI to the instrument to test");
    let instrument = Instrument::load(&path).expect("instrument should parse");
    assert_eq!(instrument.zones.len(), 1263);
    let root = instrument
        .source_path
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let texts: Vec<Option<String>> = instrument
        .scripts
        .iter()
        .map(|slot| slot.text.clone())
        .collect();
    let runtime = openkontakt::KspRuntime::compile(&texts, &root);
    runtime.initialize();
    let diagnostics = runtime.diagnostics();
    let errors: Vec<&String> = diagnostics
        .iter()
        .filter(|message| message.starts_with("KSP parse") || message.starts_with("KSP on init"))
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
    let controls = runtime.controls(0);
    assert!(controls.len() >= 8, "declared controls: {}", controls.len());
    for slot in &instrument.scripts {
        runtime.apply_persistent(
            slot.index,
            &slot
                .persistent
                .iter()
                .map(|entry| (entry.name.clone(), entry.values.clone()))
                .collect::<Vec<_>>(),
        );
    }
}

/// `on note` / `on release` must actually route groups. That panel splits its eight
/// groups into four sustained and two-and-a-half… specifically: it disallows the release-trigger
/// groups on note-on and allows only those on release.
///
/// ```text
/// OPENKONTAKT_MULTIMIC_NKI="…/instrument.nki" cargo test -p openkontakt --test real_library -- --ignored
/// ```
#[test]
#[ignore = "requires a retail .nki + loose WAV library"]
fn multimic_note_and_release_route_groups() {
    let path = std::env::var("OPENKONTAKT_MULTIMIC_NKI")
        .expect("set OPENKONTAKT_MULTIMIC_NKI to the instrument to test");
    let instrument = Instrument::load(&path).expect("instrument should parse");
    let root = instrument
        .source_path
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let texts: Vec<Option<String>> = instrument
        .scripts
        .iter()
        .map(|slot| slot.text.clone())
        .collect();
    let groups: Vec<openkontakt::ksp::GroupInfo> = instrument
        .groups
        .iter()
        .map(|group| openkontakt::ksp::GroupInfo {
            release_trigger: group.release_trigger,
            envelope: openkontakt::envelope::group_amp_envelope(group),
        })
        .collect();
    let release_groups: Vec<usize> = groups
        .iter()
        .enumerate()
        .filter(|(_, group)| group.release_trigger)
        .map(|(index, _)| index)
        .collect();
    assert_eq!(
        release_groups.len(),
        2,
        "the patch has two release-trigger groups"
    );

    let runtime = openkontakt::KspRuntime::compile_with_groups(&texts, &root, groups);
    runtime.initialize();
    for slot in &instrument.scripts {
        runtime.apply_persistent(
            slot.index,
            &slot
                .persistent
                .iter()
                .map(|entry| (entry.name.clone(), entry.values.clone()))
                .collect::<Vec<_>>(),
        );
    }

    let note = runtime.dispatch(
        "note",
        openkontakt::ksp::EventContext {
            note: 60,
            velocity: 100,
            id: 1,
            status: 1,
        },
    );
    let mask = note.allowed_groups.expect("on note routes groups");
    assert_eq!(mask.len(), instrument.groups.len());
    for index in &release_groups {
        assert!(
            !mask[*index],
            "release group {index} must be muted on note-on"
        );
    }
    assert!(mask.iter().filter(|allowed| **allowed).count() >= 1);

    let release = runtime.dispatch(
        "release",
        openkontakt::ksp::EventContext {
            note: 60,
            velocity: 100,
            id: 1,
            status: 0,
        },
    );
    let mask = release.allowed_groups.expect("on release routes groups");
    for index in &release_groups {
        assert!(
            mask[*index],
            "release group {index} must be allowed on release"
        );
    }
    assert!(
        release
            .actions
            .iter()
            .any(|action| matches!(action, openkontakt::ksp::ScriptAction::PlayNote { .. })),
        "the half-pedal path must play a release sample: {:?}",
        release.actions
    );
}

/// Kontakt 8-era panel with a `ui_table`, `ui_switch` and conditional bus maths. It failed to
/// compile until `ui_table %steps[16](…)` was supported, which silently disabled the whole panel.
#[test]
#[ignore = "requires a retail .nki + loose WAV library"]
fn drone_panel_compiles_with_table_control() {
    let path = std::env::var("OPENKONTAKT_DRONE_NKI")
        .expect("set OPENKONTAKT_DRONE_NKI to the instrument to test");
    let instrument = Instrument::load(&path).expect("instrument should parse");
    let root = instrument
        .source_path
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let texts: Vec<Option<String>> = instrument
        .scripts
        .iter()
        .map(|slot| slot.text.clone())
        .collect();
    let runtime = openkontakt::KspRuntime::compile(&texts, &root);
    runtime.initialize();
    assert!(
        runtime.diagnostics().is_empty(),
        "{:?}",
        runtime.diagnostics()
    );
    let controls = runtime.controls(0);
    assert!(
        controls.len() >= 35,
        "declared controls: {}",
        controls.len()
    );
    assert!(controls.iter().any(|c| c.name == "steps"));
    assert!(controls.iter().any(|c| c.name == "gate_onoff_switch"));
}

/// A knob has two separate texts: `set_text` names it ("Attack") and `set_knob_label` shows the
/// engine value ("0.00 ms"). Combining them into one field made the name disappear.
///
/// ```text
/// OPENKONTAKT_STRINGS_NKI="…/instrument.nki" cargo test -p openkontakt --test real_library -- --ignored
/// ```
#[test]
#[ignore = "requires a retail .nki + loose WAV library"]
fn knobs_show_name_and_engine_value_separately() {
    let path = std::env::var("OPENKONTAKT_STRINGS_NKI")
        .expect("set OPENKONTAKT_STRINGS_NKI to the instrument to test");
    let instrument = Instrument::load(&path).expect("instrument should parse");
    let root = instrument
        .source_path
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let texts: Vec<Option<String>> = instrument
        .scripts
        .iter()
        .map(|slot| slot.text.clone())
        .collect();
    let groups: Vec<openkontakt::ksp::GroupInfo> = instrument
        .groups
        .iter()
        .map(|group| openkontakt::ksp::GroupInfo {
            release_trigger: group.release_trigger,
            envelope: openkontakt::envelope::group_amp_envelope(group),
        })
        .collect();
    let runtime = openkontakt::KspRuntime::compile_with_groups(&texts, &root, groups);
    runtime.initialize();
    let controls = runtime.controls(0);
    let knob = |name: &str| controls.iter().find(|c| c.name == name).unwrap();
    assert_eq!(knob("attack_knob").label, "Attack");
    assert_eq!(knob("attack_knob").value_text.as_deref(), Some("0.00 ms"));
    assert_eq!(knob("decay_knob").label, "Decay");
    assert_eq!(knob("decay_knob").value_text.as_deref(), Some("500.0 ms"));
    assert_eq!(knob("sustain_knob").label, "Sustain");
    assert_eq!(knob("sustain_knob").value_text.as_deref(), Some("0.0 dB"));
    assert_eq!(knob("release_knob").value_text.as_deref(), Some("501.6 ms"));
}

/// Dragging an ADSR knob must update its displayed value: the panel calls
/// `set_knob_label(..., get_engine_par_disp($ENGINE_PAR_ATTACK, 0, 0, -1))` from `on ui_control`.
#[test]
#[ignore = "requires a retail .nki + loose WAV library"]
fn attack_knob_readout_follows_the_knob() {
    let path = std::env::var("OPENKONTAKT_STRINGS_NKI")
        .expect("set OPENKONTAKT_STRINGS_NKI to the instrument to test");
    let instrument = Instrument::load(&path).expect("instrument should parse");
    let root = instrument
        .source_path
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let texts: Vec<Option<String>> = instrument
        .scripts
        .iter()
        .map(|slot| slot.text.clone())
        .collect();
    let groups: Vec<openkontakt::ksp::GroupInfo> = instrument
        .groups
        .iter()
        .map(|group| openkontakt::ksp::GroupInfo {
            release_trigger: group.release_trigger,
            envelope: openkontakt::envelope::group_amp_envelope(group),
        })
        .collect();
    let runtime = openkontakt::KspRuntime::compile_with_groups(&texts, &root, groups);
    runtime.initialize();
    let before = runtime
        .controls(0)
        .into_iter()
        .find(|c| c.name == "attack_knob")
        .unwrap()
        .value_text
        .clone();
    assert_eq!(before.as_deref(), Some("0.00 ms"));

    assert!(runtime.set_control_normalized(0, "attack_knob", 0.5));
    let after = runtime
        .controls(0)
        .into_iter()
        .find(|c| c.name == "attack_knob")
        .unwrap()
        .value_text
        .clone();
    let expected = format!("{:.1} ms", openkontakt::envelope::normalized_to_ms(0.5));
    assert_eq!(after.as_deref(), Some(expected.as_str()));
}

/// The streaming decoder must produce exactly what the old in-memory decoder (hound) produced.
/// These samples are 24-bit, so this is the regression that made the library sound like noise.
#[test]
#[ignore = "requires a retail .nki + loose WAV library"]
fn sample_decodes_like_hound() {
    let path = std::env::var("OPENKONTAKT_STRINGS_NKI")
        .expect("set OPENKONTAKT_STRINGS_NKI to the instrument to test");
    let instrument = Instrument::load(&path).expect("instrument should parse");
    let sample_path = instrument
        .resolved_samples
        .values()
        .next()
        .expect("one resolved sample")
        .clone();

    let ours = openkontakt::wav::map_wav(&sample_path, 0).expect("mmap decode");
    let mut reader = hound::WavReader::open(&sample_path).expect("hound");
    let spec = reader.spec();
    assert_eq!(spec.bits_per_sample, 24, "this corpus is 24-bit");
    let hound: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => {
            let scale = 1.0f32 / (1u64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.unwrap() as f32 * scale)
                .collect()
        }
        hound::SampleFormat::Float => reader.samples::<f32>().map(|s| s.unwrap()).collect(),
    };
    assert_eq!(ours.frames * spec.channels as usize, hound.len());
    let mut worst = 0.0f32;
    for index in 0..hound.len().min(20_000) {
        let frame = index / spec.channels as usize;
        let channel = index % spec.channels as usize;
        let (left, right) = ours.frame(frame);
        let value = if channel == 0 { left } else { right };
        worst = worst.max((value - hound[index]).abs());
    }
    assert!(worst < 1e-5, "worst difference from hound: {worst}");
}

/// Some libraries build their whole panel at runtime from a picture that only exists inside the
/// resource archive (`.nkr`), with no `Resources/pictures/` directory at all. The panel is blank
/// without those images, so reading the archive is what makes the instrument usable.
#[test]
#[ignore = "requires a retail .nki whose artwork lives in a .nkr archive"]
fn nkr_archive_yields_the_panel_pictures() {
    let path = std::env::var("OPENKONTAKT_NKR_NKI")
        .expect("set OPENKONTAKT_NKR_NKI to an instrument whose pictures live in a .nkr");
    let instrument = Instrument::load(&path).expect("instrument should parse");
    assert!(
        instrument.wallpaper_path.is_none(),
        "this fixture must not have a loose wallpaper, otherwise it does not test the archive"
    );
    let archive = instrument
        .resource_archive_path()
        .expect("a resource archive next to the instrument");
    assert_eq!(archive.extension().and_then(|e| e.to_str()), Some("nkr"));
    let assets = instrument.picture_assets();
    assert!(
        assets.len() >= 3,
        "expected the panel, a knob and an icon, got {}",
        assets.len()
    );
    for asset in &assets {
        assert!(
            !asset.image.is_empty(),
            "picture {} came back empty",
            asset.stem
        );
        assert!(
            asset.image.starts_with(b"\x89PNG") || asset.image.starts_with(b"\xff\xd8"),
            "picture {} is not an image",
            asset.stem
        );
    }
    assert!(
        assets.iter().any(|asset| asset.text.is_some()),
        "the .txt sprite layouts must come back too, otherwise animated knobs fall back to frame 1"
    );

    // The script draws the panel from one of these pictures at runtime.
    let runtime = openkontakt::KspRuntime::compile_with_groups(
        &instrument
            .scripts
            .iter()
            .map(|s| s.text.clone())
            .collect::<Vec<_>>(),
        &instrument.library_root.display().to_string(),
        Vec::new(),
    );
    runtime.initialize();
    let name = runtime
        .engine
        .wallpaper_picture()
        .expect("the script should assign $INST_WALLPAPER_ID");
    assert!(
        assets
            .iter()
            .any(|asset| asset.stem.eq_ignore_ascii_case(&name)),
        "the assigned panel picture {name} must be one of the archive pictures"
    );
}

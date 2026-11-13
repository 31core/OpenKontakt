# OpenKontakt

A clean-room, [nice-plug](https://codeberg.org/RustAudio/nice-plug) sampler that loads Kontakt
`.nki` instruments, whether their audio is shipped as **loose `.wav` files** or as **packed `.ncw`
files**. It builds as a **VST3**, a **CLAP**, and a **standalone application**, with a
Kontakt-inspired custom GUI.

The reader unwraps the real Kontakt container rather than looking for chunk ids in the raw file:

```
.nki NISound item tree
  RepositoryRoot -> BNISoundPreset -> EncryptionItem -> SubtreeItem (FastLZ level 2)
    -> PresetChunkItem -> chunk stream
       0x28 Program -> 0x33 GroupList, 0x34 ZoneList, 0x06 KSP slots
       0x4B / 0x3D FileNameTable -> sample paths -> .wav / .ncw
```

Two documents stand on their own, independent of this code:

* [docs/format.md](docs/format.md) — the byte-level description of the files: container framing, the
  chunk stream, the catalogue, the monolith archives and the sample codec.
* [docs/playback.md](docs/playback.md) — how the original player turns those bytes into sound:
  velocity, the amplitude envelope, tuning, gain staging, round robin.
* [docs/ksp.md](docs/ksp.md) — the script language a player has to run: execution order, the event
  object, group routing, the builtins that change the sound.
* [docs/ksp-panel.md](docs/ksp-panel.md) — how a script draws its interface: the grid, sprites and
  sidecars, pages made of hide bits, and where the wallpaper comes from.

## Status

Implemented and validated against retail libraries from several vendors — pianos, a drum kit, two
electric guitars and a string collection:

- NISound `.nki` item tree, exact-consumption checks at every layer
- FastLZ level 1/2, including the level-2 16-bit-distance escape
- Program (`0x28`) prefix for every accepted schema version, with raw private/suffix preservation
- GroupList (`0x33`) and ZoneList (`0x34`), loops, slices, sparse fields kept raw where undocumented
- FileNameTable v2/v3 and legacy `0x3D`, with the documented ancestor walk, lexical/canonical root
  containment, case-insensitive basename fallback, and ambiguity diagnostics
- Five KSP `0x06` slots, persistent values, UTF-8/Windows-1252 decoding
- **KSP execution** (interpreter, not a declaration scraper): `on init` declares the controls and
  pushes the script's defaults into the engine, and every control in the panel runs its
  `on ui_control` handler live. `set_engine_par` reaches the mixer, and `$NI_BUS_OFFSET + n` maps
  to the bus each group is routed to, so a multi-mic library's faders change the balance of
  sounding notes
- Saved persistent values are restored after `on init` and replayed through `on ui_control`, so a
  reloaded instrument comes back with the user's mix, not the script defaults
- UI and table callbacks deliver cross-slot PGS notifications and queue sound-producing actions;
  saved slots restore in a batch before notification. Group gain/pan/tuning update active voices,
  event gain/pan/tuning select the event id, and scripted notes return independent ids and honour
  their offset and duration. Randomisation and menu relabelling builtins are supported.
- Three-band parametric EQ subtype `0x18/24` is decoded and rendered, with live band gain/frequency
  controls. Its knob mappings and RBJ DSP are approximations pending reference sweeps.
- `on note` / `on release` are dispatched with the event variables and the group-routing builtins
  (`allow_group`, `disallow_group`, `play_note`, `note_off`, `change_note`, `ignore_event`), so
  panels that split sustained and release-trigger groups select groups per event — one ambisonic
  patch takes 6 of its 8 groups on note-on and exactly its 2 release groups on release.
  `change_note` rewrites the event's key for the rest of its life, which is how a drum kit that
  stores its samples on hidden notes still plays from the keys the panel advertises
- A read-only extraction of KSP `ui_*` declarations used to render the panel
- Native KSP grid geometry when rendering that panel: `move_control(x,y)` uses the fixed
  `92 × 21 px` cells, `x = ((x-1)*92)+66`, `y = ((y-1)*21)+2`, with `set_ui_height_px()` setting the
  performance-view height
- `move_control_px(x,y)` panels are rendered in native pixels with the script's own
  `set_ui_width_px`/`set_ui_height_px` viewport, instead of being mistaken for grid cells and pushed
  off-screen
- **Custom pictures**: `set_control_par_str($id, $CONTROL_PAR_PICTURE, name)` is resolved against
  the library's own `Resources/pictures/<name>.png` plus its `.txt` sprite-sheet sidecar, so a
  control is sized and animated from the real frame size (`BlueDot` 32×32, `DamperLine` 32×142,
  `ActionLine` 32×167, `SimpleSlider` 100×15) instead of a guessed grid cell
- **Panel geometry**: the wallpaper is drawn at its own aspect scaled by width with its bottom edge
  on the panel bottom — so the controls line up with the labels baked into the image. Panel pages
  (`$CONTROL_PAR_HIDE` / `hide_part`) and stacked page sheets are handled, so a paged panel shows
  one page instead of every page at once
- **Linked controls**: writing a `ui_*` variable moves the widget without firing `on ui_control`,
  exactly as Kontakt does. A panel with dot handles relies on this: dragging a slider moves the
  dot, and dragging the dot moves the slider
- **Menus**: a menu opens a drop-down and reports the chosen item through `on ui_control`, and its
  range grows as rows are added. `$CONTROL_PAR_MOUSE_BEHAVIOUR` is parsed and kept, but relative
  dragging is not implemented — a control whose script asks for it drags absolutely, so a custom
  dot handle feels like a slider instead of a knob
- **AHDSR amplitude envelopes**: read from each group's `0x3B → 0x0D → 0x07 → 0x3F` chain and
  applied per voice, with `$ENGINE_PAR_ATTACK/DECAY/SUSTAIN/RELEASE` overriding the stored values.
  A group can hold several envelopes (a filter one and a volume one); the amplifier envelope is
  chosen by its target, not by its position, or a plucked instrument inherits the filter's 3.6 s
  attack. `get_engine_par`/`get_engine_par_disp` answer from the same envelope, so a panel's knobs
  read `0.00 ms / 500.0 ms / 0.0 dB / 501.6 ms` instead of zeros
- **Per-bus sends**: voices accumulate into their 16 instrument buses, each bus is gained
  individually, and every bus contributes to the send in proportion to its own
  `$ENGINE_PAR_SENDLEVEL_n`, feeding the convolution return
- **Streaming samples**: WAV files are memory-mapped, not read into RAM. Only the instrument's DFD
  preload (`Program.dfd_preload`) is touched up front, so a ~500 MB string library costs 47 MB
  resident and loads in 0.3 s instead of several seconds — a 10 GB library maps the same way.
  Packed `.ncw` samples decode on read with the same preload rule
- **Off-thread scripting**: the KSP interpreter runs on its own worker thread. The audio callback
  only posts note/CC commands and applies results (polled every sample, so a note starts within a
  few samples), and impulse responses are loaded on a separate thread as well
- Full `.wav` loading (PCM 8/16/24/32-bit and 32-bit float, mono/stereo), with the WAV header taking
  precedence over the zone's stored sample metadata
- **`.ncw` loading**: header, block table, 512-frame blocks, LSB-first packing, delta-coded blocks
  and the mid/side and float flags, so a library that ships no `.wav` at all still plays
- **Keyboard colours**: `set_key_color()` and the `$KEY_COLOR_*` swatches. A script can colour the
  played range and hide the rest, which is how a drum kit shows eight octaves of hidden mapping as
  one row of keys; the 1 px rail above the keys takes the key's colour too
- **`.nkr` resource archives** are read for artwork: the file table yields picture sprite sheets and
  their `.txt` sidecars, in both layouts found in retail libraries — a bare name list with blocks in
  the same order, and a directory whose rows carry the absolute offset of each block. A script's
  `$INST_WALLPAPER_ID` picture resolves through this path when the library ships no loose
  `Resources/pictures/` folder
- Instrument wallpaper read from the `.nki`: Program terminal filename reference `F2` → v2
  `other` list / v3 flat table, with no bundled fallback artwork
- Sampler engine with key/velocity ranges, root key, group and zone gain/pan/tune, loops, reverse,
  polyphonic playback, note-off release, and a playable on-screen/computer keyboard
- VST3, CLAP, and standalone exports

### Not implemented yet

The project deliberately refuses to guess. Undocumented paths are either preserved verbatim or
marked with `unimplemented!()` / a clear diagnostic:

- **Sample data inside NKX monoliths and NKR archives** — a sample that lives in one of those is
  reported as a diagnostic and stays silent. `.nkr` is read for its *artwork* (see above); what is
  missing there is only the sample payloads. Loose `.ncw` files are decoded, but one that is packed
  into an `.nkx`/`.nkr` is not reachable yet.
- **Encrypted libraries** (`EncryptionItem.is_encrypted == 1`) — require the library key; loading
  fails explicitly.
- **Legacy NKS containers** (Kontakt 1–4 `BPatchHeaderV42`) — the decompressed-length offset is not
  specified yet; detected and marked `unimplemented!()`.
- **Modulation DSP and coroutine waits** — named `0x3B`/`0x3C` records, supported controller curves
  and `find_mod`/`find_target` are read, but native modulation/time laws, gate/sidechain and the full
  CC-to-envelope DSP remain unverified. External-envelope rendering is off by default; the
  `OPENKONTAKT_EXTERNAL_ENVELOPES=1` research switch uses an experimental mapping. `wait()` still
  advances a virtual clock rather than suspending/resuming callbacks.
- **Script state locks** — dispatch itself now runs on the worker thread, but the GUI still shares
  the runtime's per-slot mutex with it. The audio callback only touches lock-free result queues and
  the send/parameter atomics.
- **Effects** — racks and slots are parsed and the insert chain is rendered: **filter**, **gainer**,
  **three-band parametric EQ**, **delay**, **chorus**/**flanger**, plus the **convolution send**
  (send rack + `load_ir_sample` / `_load_ir_sample`).
  Every other module is preserved but silent, and LFO/follower modulators and start criteria are
  still not applied. Sends are approximated by one global send level rather than the stored 8-way
  matrix.
- **Round-robin group selection** — the `0x38` start-criteria record is *inferred*: groups that
  share a record apart from its position word are treated as a cycle and take turns. A library whose
  RR lives in KSP rather than in that record will still double-strike.

## Building

Requirements: Rust (edition 2024; tested with 1.99), `pkg-config`, and Xcode command-line tools on
macOS.

```bash
# Standalone
cargo run --release -p openkontakt-plugin --bin openkontakt -- "<library>/<instrument>.nki"

# Debug without audio hardware, for UI work
cargo run -p openkontakt-plugin --bin openkontakt -- --backend dummy -- "<library>/<instrument>.nki"

# All Rust tests
cargo test --workspace
```

The standalone accepts the `.nki` path anywhere on the command line; it is stripped before
nice-plug's own argument parser runs.

### VST3 / CLAP bundles

The plugin crate exports both formats with one macro call each:

```rust
nice_export_clap!(OpenKontakt);
nice_export_vst3!(OpenKontakt);
```

Create macOS bundles with:

```bash
./scripts/bundle-macos.sh      # release build, then target/bundled/*
```

The script writes `OpenKontakt.vst3`, `OpenKontakt.clap`, `OpenKontakt.app` (standalone), and copies
the raw standalone binary to `target/bundled/standalone/openkontakt`.

For Windows and Linux the standalone is a single executable, so only the VST3 needs a bundle:

```bash
./scripts/bundle-linux.sh      # run on Linux
./scripts/bundle-windows.sh    # run on Windows (Git Bash or MSYS2)
```

Both write `OpenKontakt.vst3/Contents/<arch>/…` and copy the raw standalone binary next to it, then
pack the VST3 into `target/OpenKontakt-<version>-linux-<arch>.tar.gz` /
`target/OpenKontakt-<version>-windows-x86_64.zip`.

`bundle-windows.sh` also cross-compiles from Linux: install `cargo-xwin` once
(`cargo install cargo-xwin`), then run the same script. It builds against
`x86_64-pc-windows-msvc` and reads the artifacts from
`target/x86_64-pc-windows-msvc/<profile>/`.

Accepting a `.nki` dropped or passed on the command line works the same on all platforms.

## Loading an instrument

- Click the **OPENKONTAKT** logo and choose **Load…**; this opens the native macOS file
  chooser. The old in-window path field has been removed.
- Or drag a `.nki` from Finder onto the window.
- The standalone also accepts a path argument and the `OPENKONTAKT_INSTRUMENT` environment variable.
- The plugin persists the last path in its host state and reloads it after activation.

Play with the on-screen keyboard or the computer keyboard (`A W S E D F T G Y H U J K …`, starting
at C4). The MIDI input receives normal note on/off/choke events from the host.

## Tests and conformance

Differential-render experiments can override the amplitude envelope duration with
`OPENKONTAKT_ENV_SCALE` (positive finite multiplier, default `1`) and the script volume mapping
with `OPENKONTAKT_VOLUME_LAW=linear` (default retains the existing dB law). Combine these with the
existing `OPENKONTAKT_VELOCITY_EXP` and `OPENKONTAKT_GAIN_MODE=no_program` switches to test one
hypothesis at a time. These are diagnostic settings, not established format rules or universal
instrument corrections.

```bash
cargo test -p openkontakt

OPENKONTAKT_STRINGS_NKI="<library>/<instrument>.nki" \
  cargo test -p openkontakt --test real_library -- --ignored
```

The optional end-to-end tests parse a whole instrument and load every sample; each is skipped
unless its environment variable is set. To look at what a reader made of a file, print the parsed
structure:

```bash
cargo run -q -p openkontakt --bin ok-inspect -- "<library>/<instrument>.nki" --dump
```

## Source layout

| path | purpose |
| --- | --- |
| `crates/openkontakt/src/nis.rs` | NISound item tree and preset unwrapping |
| `crates/openkontakt/src/fastlz.rs` | FastLZ level 1/2 |
| `crates/openkontakt/src/program.rs` | Program chunk and public prefix |
| `crates/openkontakt/src/group.rs` | GroupList and group objects |
| `crates/openkontakt/src/zone.rs` | ZoneList, loops, slices |
| `crates/openkontakt/src/filename.rs` | FileNameTable v2/v3/legacy |
| `crates/openkontakt/src/resolve.rs` | path resolution rules |
| `crates/openkontakt/src/archive.rs` | `.nkr` file table (artwork) |
| `crates/openkontakt/src/ncw.rs` | `.ncw` block codec |
| `crates/openkontakt/src/nkc.rs` | `.nkc` catalogue |
| `crates/openkontakt/src/script.rs` | KSP slot parser |
| `crates/openkontakt/src/ksp_ui.rs` | read-only KSP UI extraction |
| `crates/openkontakt/src/wav.rs` | WAV loader |
| `crates/openkontakt/src/envelope.rs` | group AHDSR envelopes |
| `crates/openkontakt/src/effects.rs` | effect module decoding |
| `crates/openkontakt-plugin/src/engine.rs` | sampler engine |
| `crates/openkontakt-plugin/src/editor.rs` | custom egui interface |
| `crates/openkontakt/src/future.rs` | `unimplemented!()` stubs for out-of-scope containers |

## Contributing

[CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) is short and mostly about the work: be civil, say how sure
you are about a format claim, do not guess at undocumented bytes, and **update the section that
documents whatever you change, in the same commit**. A capability that only exists in the code is a
feature nobody finds; one that exists only in the README is a lie.

## License

MIT. The UI code is original to this project. Instrument artwork is loaded from the user's own
Kontakt library and is not redistributed with OpenKontakt. Kontakt is a trademark of Native
Instruments; this project is not affiliated with or endorsed by Native Instruments.

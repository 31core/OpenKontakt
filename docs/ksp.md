# KSP behaviour in Kontakt

An instrument can carry up to five KSP scripts, and they are not decoration: a drum kit's mapping,
a guitar's keyswitches and a piano's microphone mixer all live in them. A player that reads the
script text but does not run it will load the instrument and play the wrong thing.

This document describes Kontakt’s script language: what runs when, what a
script can do to a note, and which builtins change the sound. It does not describe the file layout
of a script slot (`docs/format.md` §3.5) and it does not describe how a script draws its panel
(`docs/ksp-panel.md`).

## 1. Execution model

**1.1 Slots.** Up to five scripts per instrument, each with its own callback set and its own
variables. They share the engine: whichever script sets the volume last wins.

**1.2 `on init`** runs once, when the instrument is loaded, before any note. It declares the UI
controls, sets engine parameters to their defaults and prepares arrays. A player that skips it gets
a silent or uninitialised instrument.

**1.3 Event callbacks** run per event, in an order that matters:

* `on note` for a note-on, before the zone lookup, because the script can rewrite the note;
* `on release` for a note-off, with the event's identity so the script can find what it started;
* `on ui_control($x)` when the user changes a control; script assignments update its display
  without recursively invoking the callback;
* `on controller` for a MIDI CC;
* `on listener` on that script's own timer, whether or not anything else is happening.

**1.4 Persistence.** A saved instrument carries the user's settings for every variable marked
`make_persistent`, as text. They are restored **after** `on init` and before the instrument is
playable, and the `on ui_control` handler of every restored control is re-run so the engine ends up
matching the panel. Skipping that replay loads the right numbers and plays the wrong sound — a
mixer whose faders show the saved mix while the engine sits at the script's defaults.

The exact ordering of persistence restoration, UI callbacks and cross-slot notifications during
loading remains unestablished; it must not be inferred from a particular interpreter’s strategy.

## 2. Variables and types

| sigil | type |
| --- | --- |
| `$` | integer |
| `%` | integer array |
| `~` | real |
| `?` | real array |
| `@` | string |
| `!` | string array |

`declare` creates a variable at its default (0 or empty), optionally with an initialiser list.
`const` declares a compile-time constant. A `polyphonic $x` is one value per voice, which is how a
script tracks per-note state. Variables are scoped to the slot, not shared between slots — a script
that wants to talk to another one uses `pgs_*` instead.

`$ENGINE_PAR_*`, `$CONTROL_PAR_*`, `$EVENT_PAR_*` and `$KEY_COLOR_*` are integer constants, not
variables: a player has to know their values, because scripts compute with them.

## 3. Control flow

`if … else … end if`, `while … end while` with `inc`/`dec`, `select $x … case … end select`,
`function … end function` with `call`, and `exit`. Functions can update slot-scoped variables;
these updates remain visible after the function returns.

## 4. The event object

A note callback sees `$EVENT_NOTE`, `$EVENT_VELOCITY`, `$EVENT_ID` and the other `$EVENT_*` fields,
and can change what happens to it:

* `ignore_event()` — the note does not play;
* `change_note(id, note)` — the event **is** now the other note, for the rest of its life. This is
  the one that surprises people: it applies to the note-on, to how zones are matched, and to the
  `on release` callback, which sees the *new* note. A drum kit stores its kick at note 0 and plays
  it from C1 by rewriting the event; a choke test written as `if ($EVENT_NOTE = 16)` only fires if
  the rewrite survived to the release.
* `play_note(note, velocity, offset, release)` — start an extra event and return its unique id.
  Offset and positive duration are in microseconds; zero duration lets the sample finish, and
  `-1` follows the parent event's note-off;
* `note_off(id)` — stop a voice started earlier;
* `change_vol` / `change_tune` / `change_pan` / `change_velo` — modify this event, in millidecibels,
  millicents and so on. Humanisation lives here.

## 5. Group routing

`disallow_group($ALL_GROUPS)` followed by a list of `allow_group(n)` is how a script decides which
groups a note is played from — the mechanism behind a drum kit playing only the snare groups for
D1, and behind a multi-mic patch dropping its release groups on note-on.

Three rules keep this working:

* the routing is per event, not global, and a note that never called either builtin plays every
  group;
* `$ALL_GROUPS` is a sentinel value (`-1`), not "all group indices" — a script that loops over
  groups still needs `allow_group` afterwards;
* `on release` routes the **release-trigger groups**, and it routes them with the same two
  builtins. A piano declares one release set per articulation, so a player that ignores the
  release routing fires the release samples of every articulation at once — two voices per
  note-off can become ten, and the wrong articulation's release is the one heard.

## 6. Engine and panel builtins

**6.1 Engine parameters.** `set_engine_par(param, value, group, slot, generic)` is the whole mixer
API: volume, pan, tune, filter cutoff, sends, effect parameters. `value` is a normalised integer in
`0…1 000 000` except where the parameter has its own range. `get_engine_par` and
`engine_par_disp` read it back — a panel's knob labels come from there, and a player that returns 0
makes every panel read "0.00 ms".

`group = -1` means "the instrument", and `generic = $NI_BUS_OFFSET + n` addresses bus `n` rather
than a group.

**6.2 Panel.** `set_control_par` / `set_control_par_str` move, resize, hide, relabel and repaint a
control; `get_ui_id` turns a control variable into an id. These form the panel-control API —
see `docs/ksp-panel.md`.

**6.3 Persistence.** `make_persistent($x)` marks a variable, `read_persistent_var($x)` asks for it
back. The stored form is text: a name, then the values, one entry per variable or array.

**6.4 Other.** `message()` writes to Kontakt's status line, `get_folder()` returns library paths,
`load_ir_sample()` asks for a convolution impulse, `pgs_*` is cross-script shared state, `wait()`
suspends for a moment.

PGS keys are symbolic bare identifiers: `COUNTER` and `LEVEL` address separate shared arrays.
`pgs_create_key`, `pgs_set_key_val`, `pgs_get_key_val` and `pgs_key_exists` operate on these names.
Changes can notify other slots through `on pgs_changed`.

`random(low, high)` produces integer values in the specified range. The native generator and
seeding behaviour are unestablished. `set_menu_item_str` changes a menu row’s text.

`find_mod` identifies a named modulator, and `find_target` identifies a named target. The native
numbering and behaviour for unresolved references require separate verification. Controller
assignments, target depth and transfer curves participate in modulation; their complete numerical
laws remain unestablished.

**6.5 The listener clock.** A panel's LFO, arpeggiator and step display are one callback,
`on listener`, run on a timer that belongs to that script alone. The interval is set from inside
the callback:

```
change_listener_par($NI_SIGNAL_TIMER_MS, 60000000 / $Tempo)
```

with `$Tempo` derived from `ms_to_ticks()`. The tick is not tied to notes: it runs while the
instrument is loaded, and the scripts gate their own work on `$keys_down` or the sustain pedal. A
player that never runs the callback leaves every such panel frozen on step 1 — the table shows its
stored values, but the playhead never moves and nothing is sent to the engine.

Two numbers matter. `ms_to_ticks` counts **1280 ticks per quarter note**, so `ms_to_ticks(500)` is
1280 at 120 BPM; and the scripts' `60000000 / $Tempo` arithmetic comes out in milliseconds, which is
what `change_listener_par` expects. The tick rate is not documented anywhere: it only becomes
visible through the rate the panels run at, and there it falls out exactly — the arithmetic reduces
to `640 × ms_per_quarter / ticks`, and a 32-step table that the original sweeps in **eight seconds
at 120 BPM** is 250 ms per step, an eighth note, which is 1280 ticks. 960 would give 10.7 s and
2880 3.6 s. [strong: the cycle time was read off the original's playhead; the interval a script has
not asked for yet remains open]

**6.6 Controllers are a bus.** `set_controller(number, value)` writes the same table the host's MIDI
input writes, and `%CC[number]` reads it. Panels use controller numbers that are not connected to
anything audible as a bus between slots: one script computes a value — an LFO step, a macro
position — and the other slots read it back from `%CC`. Because the table is shared rather than
per-script, it also reaches the engine, which is how a script's controller can drive a modulation.

## 7. Unestablished behaviour

* The exact ordering and reentrancy of cross-slot PGS notifications.
* The listener interval before a script explicitly requests one.
* The scheduling and resumption semantics of long callbacks containing `wait()`.
* Native modulation curves, polarity, depth and normalised parameter mappings.
* The precise semantics of engine targets not independently measured.
* The native random generator, seed and event-to-event sequence.

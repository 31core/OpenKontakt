# KSP: the script language a player has to run

An instrument can carry up to five KSP scripts, and they are not decoration: a drum kit's mapping,
a guitar's keyswitches and a piano's microphone mixer all live in them. A player that reads the
script text but does not run it will load the instrument and play the wrong thing.

This document is about the language as far as a player must implement it: what runs when, what a
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
* `on ui_control($x)` when a control changes, whether the user moved it or the script wrote it;
* `on controller` for a MIDI CC.

**1.4 Persistence.** A saved instrument carries the user's settings for every variable marked
`make_persistent`, as text. They are restored **after** `on init` and before the instrument is
playable, and the `on ui_control` handler of every restored control is re-run so the engine ends up
matching the panel. Skipping that replay loads the right numbers and plays the wrong sound — a
mixer whose faders show the saved mix while the engine sits at the script's defaults.

**1.5 Isolation.** A script that fails to parse is reported and skipped; it must not take the other
four with it, and it must not take the instrument with it.

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
`function … end function` with `call`, and `exit`. The forms are ordinary; the two that a
hand-written interpreter usually gets wrong are `select` (fall-through and `case` ranges) and
functions, which have their own local scope and are the reason a script can call the same page
switch from six places.

**A script is not bounded by the player.** `while` loops that never terminate exist in real
libraries — usually waiting on a controller that has not arrived. An interpreter that runs a script
on the audio thread and does not bound it will hang the audio callback.

## 4. The event object

A note callback sees `$EVENT_NOTE`, `$EVENT_VELOCITY`, `$EVENT_ID` and the other `$EVENT_*` fields,
and can change what happens to it:

* `ignore_event()` — the note does not play;
* `change_note(id, note)` — the event **is** now the other note, for the rest of its life. This is
  the one that surprises people: it applies to the note-on, to how zones are matched, and to the
  `on release` callback, which sees the *new* note. A drum kit stores its kick at note 0 and plays
  it from C1 by rewriting the event; a choke test written as `if ($EVENT_NOTE = 16)` only fires if
  the rewrite survived to the release.
* `play_note(note, velocity, offset, release)` — start an extra voice, independent of the event;
* `note_off(id)` — stop a voice started earlier;
* `change_vol` / `change_tune` / `change_pan` / `change_velo` — modify this event, in millidecibels,
  millicents and so on. Humanisation lives here.

## 5. Group routing

`disallow_group($ALL_GROUPS)` followed by a list of `allow_group(n)` is how a script decides which
groups a note is played from — the mechanism behind a drum kit playing only the snare groups for
D1, and behind a multi-mic patch dropping its release groups on note-on.

Two rules keep this working:

* the routing is per event, not global, and a note that never called either builtin plays every
  group;
* `$ALL_GROUPS` is a sentinel value (`-1`), not "all group indices" — a script that loops over
  groups still needs `allow_group` afterwards.

## 6. Engine and panel builtins

**6.1 Engine parameters.** `set_engine_par(param, value, group, slot, generic)` is the whole mixer
API: volume, pan, tune, filter cutoff, sends, effect parameters. `value` is a normalised integer in
`0…1 000 000` except where the parameter has its own range. `get_engine_par` and
`engine_par_disp` read it back — a panel's knob labels come from there, and a player that returns 0
makes every panel read "0.00 ms".

`group = -1` means "the instrument", and `generic = $NI_BUS_OFFSET + n` addresses bus `n` rather
than a group.

**6.2 Panel.** `set_control_par` / `set_control_par_str` move, resize, hide, relabel and repaint a
control; `get_ui_id` turns a control variable into an id. These are the panel's whole runtime API —
see `docs/ksp-panel.md`.

**6.3 Persistence.** `make_persistent($x)` marks a variable, `read_persistent_var($x)` asks for it
back. The stored form is text: a name, then the values, one entry per variable or array.

**6.4 Other.** `message()` writes to Kontakt's status line, `get_folder()` returns library paths,
`load_ir_sample()` asks for a convolution impulse, `pgs_*` is cross-script shared state, `wait()`
suspends for a moment.

## 7. What to do with a function you do not implement

Ignore it, keep going, and say so once. A script calling an unknown function is common — libraries
use functions from newer Kontakt versions than the file was authored for — and the alternatives are
worse: aborting the callback loses the whole note, and pretending the function did something
invents behaviour.

The exception is a function whose *return value* feeds a comparison: `search(%array, $x)` returning
0 instead of −1 makes `if (search(...) = -1)` false, which is how a group-routing loop silently
selects nothing and the instrument plays no sound at all. Those builtins have to be right.

## 8. What is not established

* `on controller`, `on pgs_changed` and the listener callbacks: parsed and compiled, dispatch
  semantics unverified.
* The exact meaning of several `$ENGINE_PAR_*` targets beyond volume, pan, tune, filter and sends.
* Whether `wait()` suspends the callback without suspending the voice, and for how long a player
  should let it run.
* How a script's `set_engine_par` interacts with a parameter the player models with a different
  range.
* Round-robin selection that lives in a script rather than in the start-criteria record.

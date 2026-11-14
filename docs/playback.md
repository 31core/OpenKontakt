# How the original player turns a file into sound

`docs/format.md` describes the bytes. This document describes the other half: what the original
player (Kontakt) does with those bytes between a note arriving and audio leaving — how it picks a
layer, how it shapes the amplitude, how it tunes the sample, how the gains add up.

It covers Kontakt’s playback behaviour. File layouts are described separately in `docs/format.md`.

## How these numbers were obtained

Almost nothing here is published by Native Instruments, so everything is empirical. Two methods,
and the grade next to each rule says which one produced it:

* **differential renders.** The same MIDI file is rendered by the reference and by comparison renderers, and
  the two files are compared: level, spectral centroid, crest factor, envelope correlation. A
  number that moves by less than the noise floor of the comparison is not a finding.
* **synthetic input.** Libraries are built from known signals (a sine sweep, a single tone with a
  known envelope) and loaded as instruments, so the player's contribution can be subtracted from
  the sample's.

Two cautions that cost time to learn:

* one measured host setup introduced a 15 ms offset between DAW and standalone renders.
  Alignment must be measured for each setup; this is not a fixed Kontakt latency.
* the velocity curve of the *sampled material* is not the velocity curve of the player. The samples
  of one library already slope at `v^1.03` on their own; the player's output sloped at `v^1.18`, so
  the player contributes about `v^0.15`.

Grades are the same three as in the format document: **[verified]** measured and reproducible,
**[strong]** fitted to measurements but not uniquely determined, **[open]** assumed.

## 1. Velocity

**1.1 Layer selection.** A zone covers a velocity range. Overlap can be defined by the authored
ranges, fade widths or script routing. Adjacent ranges alone do not establish a crossfade. A
constant-carrier reference of Main Lite at velocity 80 selects the release recording covering
60–80 without the additional, longer recording covering 81–100. [verified for this capture]

**1.2 Crossfade laws.** Stored velocity-fade widths describe overlapping layers. Whether a
particular preset uses linear, equal-power or another fade law requires isolated reference
sweeps. No universal fade width is established. [open]

**1.3 Velocity → gain.** One measured library’s output followed approximately `velocity^1.18`,
while its recordings alone followed `velocity^1.03`. Their difference suggests an additional
exponent near 0.15 for that measurement. This is an empirical fit, not a universal velocity law;
script processing, modulation and saved state must be controlled independently. [open]

## 2. The amplitude envelope

**2.1 The record.** A group's AHDSR stores an attack curve in −1…+1, then four times in
milliseconds (attack, decay, hold, release), then a sustain level as a linear gain.
`docs/format.md` §3.6 has the layout; this section is about what the numbers mean.

**2.2 Which envelope.** A group may carry several envelopes — a `filterCutoff` one and a `volume`
one. The amplitude envelope is the one whose modulator targets `volume`, and it is **not**
necessarily the first in the array. Reading the first one gives a plucked instrument the filter
envelope's 3.6 s attack and zero sustain. [verified]

**2.3 Stage order.** Attack rises from silence to full, hold stays there, decay falls to the
sustain level, and the note stays there until it is released; then the release stage runs. A
sustain of 0 is a legal, deliberate "one-shot" envelope.

**2.4 Curve shape.** The stored attack curve selects the shape of the attack segment rather than
its length: −1 bends it one way, +1 the other, 0 is linear. The exact law is **[open]**.
Constant-carrier measurements distinguish curve families that ordinary sampled material can
conceal; sharing endpoints does not establish equivalence to a power curve.

**2.5 Normalised knob values.** KSP exposes these times as 0…1 (`$ENGINE_PAR_ATTACK` and friends)
and panels display them in milliseconds. One observed preset pairs a 500 ms decay with a saved
controller value of 586069. This single correspondence does not determine the native conversion
curve or its limits. [open]

**2.6 Release.** On note-off the level falls from its current level; it does not jump to the
sustain level first. Controller routes can change the effective duration from the stored base
value. A release longer than the remaining sample is cut short by the sample, not extended.

**2.7 Controlled carrier capture.** Main Lite was measured with fixed-amplitude sine samples,
separate frequencies for the two microphones and separate markers for release-trigger samples.
At MIDI note 69, velocity 80, CC84 changes the attack and CC72 changes the main-note release.
For CC values 0, 32, 64, 96 and 127, the first microphone's main tone reaches 20 dB below its held
level approximately within 20, 21, 34, 64 and 149 ms of note-off. The carrier extraction uses a
30 Hz low-pass, so the shortest times are resolution-limited, not exact envelope parameter
readouts. Release-trigger markers have their own attack and level response and must be measured
separately from the main note. These observations constrain the preset's response but do not
establish a universal parameter conversion or envelope formula. [verified capture / open law]

## 3. Pitch and playback rate

**3.1 Tuning.** A sample is repitched by

```
ratio = 2 ^ ((played_key + program.transpose − zone.root_key) / 12)
ratio = ratio × program.tune × zone.tune × group.tune
```

where every `tune` is a linear ratio (`1.0` unchanged, `0.5` one octave down). Key tracking can be
switched off per group, in which case the key does not affect pitch at all.

**3.2 Sample rate.** The rate conversion factor is `source_rate / device_rate`, and it multiplies
the pitch ratio. Getting the direction wrong transposes everything — 44.1 kHz content in a 48 kHz
stream would come out 1.47 semitones sharp.

**3.3 Sample start, end, loops.** A zone can start and end inside the sample, and can loop a region
forward, backward or alternately, with a crossfade length and a loop count. `end = 0` means "to the
end of the file".

## 4. Note-off, release triggers and chokes

**4.1 Release-trigger groups.** A group marked `release_trigger` plays its sample when the key is
let go, not when it is pressed. It must be excluded from note-on, or the release recording fires in
the attack — audible as a click and as a level that does not belong to the note. Measured on one
library, its release samples were firing on note-on at gain 0.12–0.27 while the note's own samples
sat at 0.019. [verified]

**4.2 Chokes.** A script stopping another note uses `fade_out(event_id, time_µs, relative)`. The
time is in **microseconds**, so a 70 ms overlap on a snare roll reads `70000`. A choke that is
applied to the wrong event id either does nothing or cuts the wrong note.

**4.3 Damper pedal and key release.** In normal sustain-pedal mode, CC64 values 64–127
keep a released main note sounding; values 0–63 let its amplitude envelope enter release.
Notes whose keys remain held are not released by lifting the pedal. The physical Note Off still
invokes `on release`; pedal-up is a controller event, not a second key release. Recorded pedal
noise can therefore be triggered independently by `on controller`. Continuous half-pedal damping
and repedalling depend on the instrument and must not be inferred from the binary threshold alone.
See the KSP [callback reference](https://docs.native-instruments.com/ni-tech-manuals/ksp-manual/en/callbacks).

## 5. Gain staging

**5.1 Everything is a ratio.** Program volume, group volume, zone volume and pan are all linear,
multiplied on the way to the output. `0.5` is −6 dB. [verified]

**5.2 Pan.** A constant-power law gives a centred mono signal a −3 dB gain in each channel.
The exact native laws for mono pan, stereo balance and width require separate measurements.

**5.3 Zone private fields.** A power-of-two field in the zone private section has been considered
as a possible trim. Its playback meaning is not established; it must not be described as a gain
solely because applying it changes the measured level. [open]

**5.4 Buses and sends.** Voices accumulate into up to sixteen instrument buses; each bus has its own
gain and its own send level into the instrument's send effects, in proportion to its
`$ENGINE_PAR_SENDLEVEL_n`.

## 6. Round robin and randomisation

**6.1 Round robin.** Groups that share a start-criteria record apart from one position word are one
cycle, and the takes play in turn. This reading is **[open]**: two libraries agree on it, which is
evidence rather than proof. A multi-microphone library has one cycle per microphone, so a note
starts one take from *each* cycle, not one take overall.

Script group routing can select corresponding takes across several microphones. Its interaction
with stored start criteria requires independent reference measurements.

**6.2 Randomisation.** Humanisation is script-driven and arrives as `change_vol`/`change_tune` with
values in millidecibels and millicents. A player that ignores those calls sounds correct but
mechanical; one that applies them to the wrong event id randomises the wrong note.

## 7. Effects and the send

Kontakt provides insert and send effects, including convolution driven by impulse responses.
Effect parameters are addressed by their group, slot and routing context. The native EQ/filter
algorithms, parameter mappings, delay synchronisation and convolution processing limits remain
unestablished here; no particular DSP approximation or optimisation defines their behaviour.

## 8. Voices and polyphony

**8.1 One voice per matched zone.** A voice is one sample playing. A note-on that matches several
zones — one per microphone, and a second velocity layer while a crossfade is in progress — starts
one voice for each, and each of the group's release-trigger zones adds another on note-off. A
two-microphone piano therefore spends four to six voices per note-on and two to four per note-off,
which means a dense passage asks for far more than the number of keys it is playing. [verified]

**8.2 Voice limits.** Reaching a voice limit can require voice stealing. The native priority,
selection criteria and transition fade have not been independently established. [open]

**8.3 Silent voices.** A zero-sustain envelope can become inaudible while the key remains held.
Kontakt’s internal accounting and retirement of such voices remain unestablished.

## 9. The per-voice filter

**9.1 A group can carry a low-pass, and a panel can sweep it.** The filter is second order: the
ratio between the original's output with a panel sweeping the cutoff and without it is a clean
12 dB/octave slope. A fourth-order fit is 2 dB worse everywhere and needs a cutoff two to twelve
times higher to explain the same slope. [verified]

**9.2 The controller-to-cutoff curve.** The cutoff follows the controller exponentially:

```
cutoff Hz = 2 ^ (7.73 + 0.0405 × controller value)
```

fitted to one library's sweep, whose stored table runs from 127 down to 4 and back. Two ends land
at about 210 Hz and 7.5 kHz. Treat the absolute numbers as provisional: the take it was fitted from
is almost all release tail in the region where the filter moves (its level there is −60 to −75
dBFS), so the fit describes the shape — exponential in the controller value rather than linear in a
normalised knob — much better than it pins the endpoints, and a sustained-note reference would move
the constants. [open]

**9.3 One filter per voice.** A shared filter would smear a sweep across notes that are
independently swept, and the cutoff has to be able to rise again while older notes still sound, so
each voice carries its own coefficients and state.

**9.4 Resonance.** Mild — the reference's spectra show no resonant peak at the cutoff, so the
stored resonance of the group module the cutoff belongs to (0.1, about Q 0.6) is enough. [open]

## 10. What is not established

* The exact curve law of an AHDSR stage; only its endpoints and monotonicity are pinned.
* The normalised-knob mapping away from the one anchor point (0.586 ↔ 500 ms).
* Whether the velocity exponent is a player law or a property of how libraries are monitored; the
  fit is good, the mechanism is not known.
* Round robin from start criteria (§6.1).
* The numerical behaviour of engine-parameter targets not independently measured.
* The exact native filter, delay, chorus and EQ algorithms.

# How the original player turns a file into sound

`docs/format.md` describes the bytes. This document describes the other half: what the original
player (Kontakt) does with those bytes between a note arriving and audio leaving — how it picks a
layer, how it shapes the amplitude, how it tunes the sample, how the gains add up.

It covers behaviour a player has to reproduce to sound like the reference. It does not cover the
file layouts, and it does not describe any particular implementation.

## How these numbers were obtained

Almost nothing here is published by Native Instruments, so everything is empirical. Two methods,
and the grade next to each rule says which one produced it:

* **differential renders.** The same MIDI file is rendered by the reference and by our player, and
  the two files are compared: level, spectral centroid, crest factor, envelope correlation. A
  number that moves by less than the noise floor of the comparison is not a finding.
* **synthetic input.** Libraries are built from known signals (a sine sweep, a single tone with a
  known envelope) and loaded as instruments, so the player's contribution can be subtracted from
  the sample's.

Two cautions that cost time to learn:

* a DAW render is **15 ms later** than a standalone render of the same event; comparisons have to
  be aligned, and an unaligned comparison looks like a 15 ms attack.
* the velocity curve of the *sampled material* is not the velocity curve of the player. The samples
  of one library already slope at `v^1.03` on their own; the player's output sloped at `v^1.18`, so
  the player contributes about `v^0.15`.

Grades are the same three as in the format document: **[verified]** measured and reproducible,
**[strong]** fitted to measurements but not uniquely determined, **[open]** assumed.

## 1. Velocity

**1.1 Layer selection.** A zone covers a velocity range. The zone whose range contains the velocity
plays; when two ranges abut, the neighbours are faded into each other rather than switched.

**1.2 The width of that fade.** Kontakt switches layer boundaries smoothly. Measured over a
127-step sweep, hard switching produces steps of up to 12 dB between adjacent velocities; a fade
width of **8 velocity units** leaves a single 2.1 dB step where Kontakt itself shows 2.3 dB.
Wider fades start flattening the response inside a layer (12 units already drags the median down by
0.5 dB). [strong]

**1.3 Velocity → gain.** The player's output follows `velocity^1.18` over the whole sweep, while the
recorded samples alone slope at `v^1.03`, so the player adds roughly **`(v/127)^0.15`**. A linear
`v/127` makes quiet notes about 20 dB too quiet, and the dynamic range collapses to 14 dB against
Kontakt's 35.3 dB. With `0.15` the range is 34.1 dB. [strong, fitted over one library's sweep]

The exponent is a fit, not a constant read out of a file: another library with differently
recorded layers would move it. Treat `0.15` as a good default and `1.18` as the thing to match.

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
its length: −1 bends it one way, +1 the other, 0 is linear. The exact law is **[open]**; a power
curve through the same endpoints is indistinguishable at the sample level for the libraries
measured here.

**2.5 Normalised knob values.** KSP exposes these times as 0…1 (`$ENGINE_PAR_ATTACK` and friends)
and panels display them in milliseconds, so a player needs the mapping. The corpus pins exactly one
point of it: one library stores a **500 ms** decay and persists a controller value of **586 069**
for that same knob, so 0.586 reads as 500 ms. A quadratic rise below that point and an exponential
run to 20 s above it satisfies that anchor and gives the fine control at the short end that an
envelope knob has. [strong — one anchor, so the shape away from it is a choice]

**2.6 Release.** On note-off the level falls from wherever it is to zero over the stored release
time; it does not jump to the sustain level first. A release that is longer than the remaining
sample is cut short by the sample, not extended.

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

## 5. Gain staging

**5.1 Everything is a ratio.** Program volume, group volume, zone volume and pan are all linear,
multiplied on the way to the output. `0.5` is −6 dB. [verified]

**5.2 Pan.** Constant power: a centred voice sits at **−3 dB**, not unity. Normalising the centre to
unity was tried and closed a 2.7 dB gap against a synthetic reference, while putting the player
2.9 dB above a real-sample reference — the real-sample reference wins, because the files it compares
are byte-identical. [verified]

**5.3 The zone private trim is not a playback gain.** The zone's private section carries a
power-of-two value that looks like an output trim. Applying it put one library 13.9 dB under the
reference and another 5.8 dB under, and it made round-robin takes alternate by 7 dB, because one
take in a cycle stores `0.5` and its siblings store `1.0`. Dropping it brings both libraries to
+1.9 dB and +0.1 dB. [verified]

**5.4 Buses and sends.** Voices accumulate into up to sixteen instrument buses; each bus has its own
gain and its own send level into the instrument's send effects, in proportion to its
`$ENGINE_PAR_SENDLEVEL_n`.

## 6. Round robin and randomisation

**6.1 Round robin.** Groups that share a start-criteria record apart from one position word are one
cycle, and the takes play in turn. This reading is **[open]**: two libraries agree on it, which is
evidence rather than proof. A multi-microphone library has one cycle per microphone, so a note
starts one take from *each* cycle, not one take overall.

**6.2 Randomisation.** Humanisation is script-driven and arrives as `change_vol`/`change_tune` with
values in millidecibels and millicents. A player that ignores those calls sounds correct but
mechanical; one that applies them to the wrong event id randomises the wrong note.

## 7. Effects and the send

The insert chain and the convolution send are rendered; the reverb send is one impulse response per
instrument, loaded from the library (`load_ir_sample`) or picked from the send rack.

**7.1 Impulse responses can be absurdly long.** A 100-second IR needs thousands of FFT partitions
and will not run in real time. Truncating at the point where the response falls 72 dB below its peak
and capping the length at 12 s made one such instrument's output bit-identical (a −209 dB
difference) and cut a 20-second render from 61.5 s to 6.3 s. [verified]

## 8. Voices and polyphony

**8.1 One voice per matched zone.** A voice is one sample playing. A note-on that matches several
zones — one per microphone, and a second velocity layer while a crossfade is in progress — starts
one voice for each, and each of the group's release-trigger zones adds another on note-off. A
two-microphone piano therefore spends four to six voices per note-on and two to four per note-off,
which means a dense passage asks for far more than the number of keys it is playing. [verified]

**8.2 The limit is a limit, not a queue.** When the polyphony is reached, some voice has to go, and
which one is audible. The voice to lose is the one nobody can hear: a released voice before a
sounding one, and among those the quietest. Choosing by age does the opposite, because the oldest
voice is the note held longest — the sustained bass. Measured on a two-microphone piano, age-based
stealing cut a held note at 88 % of its level on a passage that also had a hundred release tails
open at −45 dB, and the cut is heard exactly as a release that fell to zero. [verified]

**8.3 A stolen voice fades.** Stopping a sounding voice at a sample boundary is a step, and a step is
a click. A few milliseconds of fade removes it and is too short to be heard as part of a note;
**5 ms** is used here. [open — the length is a choice, not a measurement]

**8.4 Silent voices give up their slots.** A one-shot envelope decays to a zero sustain and stays
there; a player that keeps such a voice "active" slowly fills its pool with inaudible voices, and
the polyphony collapses long before the note count says it should.

## 9. What is not established

* The exact curve law of an AHDSR stage; only its endpoints and monotonicity are pinned.
* The normalised-knob mapping away from the one anchor point (0.586 ↔ 500 ms).
* Whether the velocity exponent is a player law or a property of how libraries are monitored; the
  fit is good, the mechanism is not known.
* Round robin from start criteria (§6.1).
* What a script's `$ENGINE_PAR_*` writes do to a parameter the player does not model.
* Whether Kontakt's own filter, delay and chorus match the simple implementations here — only their
  presence and rough response have been compared.

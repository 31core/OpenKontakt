# The Kontakt file formats

This is a description of the files a Kontakt library is made of: the instrument (`.nki`), its
catalogue (`.nkc`), the resource archives (`.nkr`), the monolith that holds packed samples
(`.nkx`), and the compressed sample container itself (`.ncw`).

It describes the bytes, not any particular program that reads them: the KSP language, how a sampler
should sound and how a player is organised are somebody else's document — the playback side is
[playback.md](playback.md). Everything here was
measured against files that ship in commercial libraries; where something is a guess rather than a
measurement it says so. Nothing here is copied from a Native Instruments document — none is
published for these formats.

Source comments cite this file by section number (`docs/format.md §2.6`). If a section moves,
`grep -rn "docs/format.md §"` finds every citation that needs renumbering.

Confidence marks used throughout:

| mark | meaning |
| --- | --- |
| **[verified]** | checked against every file of that kind on hand, and the check is exact (a walk consumes the file byte for byte, a field round-trips, a payload matches its declared type) |
| **[strong]** | consistent across many files, one plausible alternative reading exists |
| **[open]** | unproven; the field may be larger, smaller, or mean something else |

---

## 1. Conventions that apply to every file

### 1.1 Byte order, alignment, padding

All multi-byte integers are **little-endian**. There is **no alignment requirement anywhere**:
records begin at arbitrary offsets, so a `u64` can start at an odd address and a header can begin
at `0x2E`. A reader has to be strictly sequential and must never round an offset up to a multiple
of four. [verified]

There is also no padding between records. Where a structure is counted or length-prefixed, the next
record starts immediately after the last byte of the previous one.

### 1.2 Four-character tags

Container tags are four ASCII bytes stored **reversed** relative to the identifier they spell:

```
bytes in the file   44 53 49 4E
read as a u32 LE    0x4E495344
as text              "DSIN"  ->  the identifier is "NISD"
```

Two tags matter: `NISD` (a generic container item) and `NIK4` (a Kontakt preset item). A third,
`hsin`, appears inside a 40-byte item header; it is lower-case in the file, and reading it as a
little-endian integer gives `0x6E697368`. Compare integers, not strings, and do not "normalise" the
case. [verified]

### 1.3 Strings

Two encodings exist and mixing them up is the most common way to misread these files. [verified]

**Counted UTF-16LE.** A `u32` holding the number of **UTF-16 code units** (not bytes), then that
many units. Adjacent fields follow immediately after the last unit — there is no terminator and no
length in bytes. Used by everything inside an `.nki` chunk and by `.nkc` names.

**Zero-terminated UTF-16LE.** A run of code units ending with `00 00`. Used by the directory
records of `.nkx` and `.nkr`, whose records are self-sizing.

A counted length of zero is a legal **empty string**, not an absent field. Kontakt writes the
literal text `(null)` when it stored a null C string, so a "credits" field reading `(null)` is
real content, not a bug.

### 1.4 Ratios, not decibels

Where a Kontakt file stores a gain, a pan or a tuning, it stores a **linear ratio**: volume `0.5`
is −6 dB, pan `0` is centre and `−1…+1` maps to hard left…hard right, tune `1.0` means unchanged
and `0.5` means one octave down. Conversions are the obvious ones:

```
dB    = 20 · log10(ratio)
cents = 1200 · log2(ratio)
```

MIDI note numbers are plain integers in `0…127`; key ranges may contain `−1`, which means "unset".

---

## 2. The container: a NISound document (`.nki`)

An `.nki` is not a chunk stream with a header. It is a tree of **items**, each of which declares
its own size, so any item can be skipped without understanding it.

```
+0x0000  ItemHeader(40)          <- the root item, its length is the file size
           ItemData(...)         <- identity layers, then the item's own bytes
           ChildTable
             row(12) + Item
             row(12) + Item
             ...
```

### 2.1 Item header — 40 bytes [verified]

```c
struct ItemHeader {
    u64 length;      // +0x00  total size of this item, header included, children included
    u32 version;     // +0x08  always 1
    u8  magic[4];    // +0x0C  "hsin"
    u32 flags;       // +0x10  1 in every observed item
    u32 reserved;    // +0x14  0
    u8  uuid[16];    // +0x18  per-item instance id, never a type tag
};
```

The root item starts at offset 0 and its `length` equals the file size, so a file that has been
truncated or extended can be recognised before anything else is parsed. The UUID is regenerated
every time an instrument is saved; two presets of the same instrument share no UUIDs.

### 2.2 ItemData — the record inside the item — 20 bytes [verified]

```c
struct ItemDataHeader {
    u64 length;      // +0x00  size of this data layer, header included
    u8  domain[4];   // +0x08  ASCII, stored reversed: "DSIN" = NISD, "4KIN" = NIK4
    u32 item_id;     // +0x0C  record kind within the domain
    u32 version;     // +0x10  always 1
};
```

Then `length − 20` bytes of layer body, in one of two shapes:

* **a leaf** (`NISD` / `0x01`): the whole body is the item's bytes;
* **anything else**: the body *begins with another ItemDataHeader*, recursively. What remains
  after the innermost layer is the item's own property bytes.

So an item is really a short stack of named layers, and raw dumps show `DSIN` two or three times
in a row before any payload. This nesting is the identity of the item, not a wrapper around a
payload — a reader that treats the first ItemData as the only one will read properties from the
wrong offset.

### 2.3 The child table and the size identity [verified]

After the property bytes, unless the item is a leaf:

```c
struct ChildTable {
    u32 version;     // 1
    u32 count;
    repeat count {
        u8  row[12]; // { u32 index; u8 domain[4] reversed; u32 item_id; }
        Item child;  // a complete item: header, data layers, its own children
    }
};
```

The 12-byte row repeats the child's domain and id so a reader can list children without parsing
them. It carries no offset and no size: children follow their rows back to back.

Every item satisfies:

```
item.length = 40 + ItemData.length + 8 + 12·count + Σ child.length
```

An item with no children therefore has `length = 48 + ItemData.length`. This identity is what makes
the walk self-checking: if a parse does not consume exactly `item.length` bytes, the offset is
wrong, and continuing would produce plausible-looking garbage.

### 2.4 Walking a file

```text
walk(offset) -> end:
    h = ItemHeader at offset
    require h.magic == "hsin" and h.version == 1
    end = offset + h.length

    # The ItemData layers nest: a non-leaf layer's body starts with the next layer.
    # Properties therefore begin after the innermost leaf, while the *outermost* length
    # says where the data layers as a whole end.
    outer = ItemData at offset + 40
    after = offset + 40 + outer.length

    if after + 8 <= end and u32(after) == 1:  # a child table is present
        n = u32(after + 4)
        q = after + 8
        repeat n:
            q += 12                            # the row
            q = walk(q)
        require q == end
    else:
        require after == end
    return end
```

`end` is authoritative: an unknown `item_id` is skipped, not guessed at, and every level asserts
exact consumption.

### 2.5 The shape of an instrument [verified]

```
Root item (NISD RepositoryRoot, 0x76)
└── child: NIK4 BNISoundPreset (0x03)
    │   data layers: Preset (0x65) -> Authorization (0x6A) -> Item (0x01)
    ├── child: NISD SoundInfoItem        (0x6C)   NKS browser tags
    ├── child: NISD ControllerAssignments(0x79)
    ├── child: NISD EncryptionItem       (0x74)   carries the compressed preset
    └── child: NIK4 BNISoundHeader       (0x04)   222-byte legacy patch header
```

The root's own data chain is `RepositoryRoot (0x76) → Authorization (0x6A) → Item (0x01)`; the
`Authorization` layer appears at several depths and carries an 8-byte value that the preset
repeats. `[strong]`

### 2.6 EncryptionItem and the compressed subtree

`EncryptionItem` (0x74) has exactly five property bytes:

```c
struct EncryptionItem {
    u32 version;        // 1
    u8  is_encrypted;   // 0 = plaintext, 1 = needs the library key
};
```

Its data chain is `EncryptionItem → SubtreeItem → Item`, and the `SubtreeItem` (0x73) is where the
actual preset lives, compressed:

```c
struct SubtreeItem {
    u32 version;         // always 1
    u8  compressed;      // 0 or 1
    // compressed == 0:
    u64 size;            // then `size` raw bytes
    // compressed == 1:
    u32 expanded_size;   // decompressed length
    u32 compressed_size; // bytes that follow
    u8  data[compressed_size];
};
```

The compressor is **FastLZ**. The level is encoded in the top three bits of the first byte, so
`level = (data[0] >> 5) + 1`. Level 2 is what Kontakt writes and it uses two features a level-1
decoder cannot handle: match lengths that continue over any number of `0xFF` bytes, and a 16-bit
distance escape (a control byte with its five-bit offset field at maximum re-bases the
back-reference at 8191). A level-1 decoder reading a level-2 stream **does not fail loudly**: it
stops early and returns a truncated buffer. Always compare the produced length with
`expanded_size`. [verified]

What comes out is *another complete item*, starting with its own `ItemHeader` whose `length` equals
`expanded_size`:

```
NISD Item                       the decompressed subtree
└── child: NISD PresetChunkItem (0x6D)
        props: u32 version              // 1
               u32 authorization word   // copied from the Authorization layer
               u32 chunk_version        // 1
               u64 chunk_size
               u8  chunk[chunk_size]    // the chunk stream
               u8  trailing[...]        // 8 bytes observed, meaning unknown
```

The trailing bytes are real: retail files carry eight bytes after the declared stream. They are
not padding and not a checksum that has been identified; a reader keeps them, a writer must copy
them back. [strong]

`is_encrypted = 1` means the bytes are further transformed with key material that is not in the
file; a reader without the library's key can only report that. [strong]

### 2.7 The chunk stream

Inside the decompressed preset is the classic Kontakt chunk stream, and this is the part most
tools call "the `.nki` format":

```c
{ u16 id; u32 length; u8 body[length]; }   // repeated to the end, no padding, no trailer
```

A modern instrument (Kontakt 5–7) has three top-level chunks:

| id | object | role |
| --- | --- | --- |
| `0x28` | Program | everything musical: name, gain, groups, zones, scripts, effects |
| `0x47` | SaveSettings | editor state; not needed to play [strong] |
| `0x4B` | FileNameTable | sample and resource paths |

`0x3D` (`FileNameListPreK51`) is the Kontakt 4 form of the same table; when a file has both, `0x4B`
is the one to trust.

Most chunk bodies are a **StructuredObject**, which frames counted sections:

```c
struct StructuredObject {
    u8  flag;           // 0 or 1
    u16 version;
    if flag == 0:       // no section lengths at all: the rest of the body is the public area
        u8 public[];
    else:
        u32 private_len;    u8 private[private_len];
        u32 public_len;     u8 public[public_len];
        u32 children_len;   u8 children[children_len];
};
```

The `children` bytes are themselves a chunk stream. The `flag == 0` form is not a corner case: a
KSP script slot (`0x06`) is always written that way, which is why a reader that insists on the
three section lengths fails on every instrument that has a script.

---

## 3. What is inside a preset

### 3.1 Program — `0x28`

Program is a StructuredObject whose public bytes begin with a fixed 23-field prefix that is
identical in every version:

| # | width | field | meaning |
| --- | --- | --- | --- |
| 1 | `u32`+utf16 | `name` | instrument name |
| 2 | `f64` | `sample_bytes_total` | sum of sample bytes, for the browser |
| 3 | `i8` | `transpose` | semitones |
| 4 | `f32` | `volume` | gain ratio |
| 5 | `f32` | `pan` | −1…+1 |
| 6 | `f32` | `tune` | ratio |
| 7–10 | `u8`×4 | `low/high_velocity`, `low/high_key` | the instrument's own range |
| 11 | `i16` | `default_keyswitch` | −1 = none |
| 12 | `i32` | `dfd_preload` | frames to preload from each sample |
| 13 | `i32` | `library_id` | 0 outside encoded libraries |
| 14 | `u32` | `fingerprint` | |
| 15 | `u32` | `loading_flags` | bit 0 is a runtime flag |
| 16 | `u8` | `group_solo` | |
| 17 | `i32` | `category_icon` | |
| 18–20 | `u32`+utf16 | `credits`, `author`, `url` | |
| 21–23 | `i16`×3 | `category_1…3` | |

The fixed part is 54 bytes, and

```
prefix_bytes = 70 + 2 · (len(name) + len(credits) + len(author) + len(url))
```

After the prefix each schema version appends its own field sequence ("the suffix"), which is where
one has to consult a per-version table. The rule that keeps a reader honest: parse the prefix,
then parse the suffix only for a version you actually have a table for, and assert the public
section is consumed exactly. `[strong]`

Program's children are what a player reads next:

| id | object |
| --- | --- |
| `0x33` | GroupList |
| `0x34` | ZoneList |
| `0x06` | script slot, up to 5 of them |
| `0x3A` | effect rack (`BParameterArraySer<BParFX, 8>`); insert and send |
| `0x45` | insert bus routing |
| `0x32` | VoiceGroups |
| `0x4E` | QuickBrowseData |

### 3.2 Groups — `0x33`

GroupList is a counted array of group objects. A group's public block starts with a counted
UTF-16LE **name**, then a fixed 36-byte tail:

| field | width | note |
| --- | --- | --- |
| `volume`, `pan`, `tune` | `f32`×3 | ratios |
| `key_tracking` | `u8` | repitch samples with the played note |
| `reverse` | `u8` | play samples backwards |
| `release_trigger` | `u8` | group fires on note-off, not note-on |
| `release_trigger_note_monophonic` | `u8` | |
| `rls_trig_counter` | `i32` | |
| `midi_channel` | `i16` | −1 = omni |
| `voice_group_index` | `i32` | −1 = none |
| `fx_idx_amp_split_point` | `i32` | |
| `muted`, `soloed` | `u8`×2 | |
| `interp_quality` | `i32` | |

so `public_len = 40 + 2·len(name)`; a name of 8 characters gives 56 bytes, of 15 gives 70, which is
the check that the widths are right. [verified]

Groups carry children of their own: `0x3B` internal modulators (16 slots), `0x3C` external
modulators (32), `0x38` start criteria (the record that round-robin and keyswitch logic is built
on), `0x4A` group dynamics.

### 3.3 Zones — `0x34`

ZoneList is a `u32` count followed by that many entries, and **each entry starts with a `u32`
holding the index of the group the zone belongs to**, before the zone object itself. The zone does
not store that index; it is the list that does.

A zone object's public block begins with 42 bytes that are identical across every schema version,
and these 42 bytes are the single most important table in the format — they are what makes a zone
playable:

| offset | width | field |
| --- | --- | --- |
| +0x00 | `i32` | `sample_start` — frames into the sample |
| +0x04 | `i32` | `sample_end` — 0 means "to the end" |
| +0x08 | `i32` | `sample_start_mod_range` — −1 = unused |
| +0x0C | `i16` | `low_velocity` |
| +0x0E | `i16` | `high_velocity` |
| +0x10 | `i16` | `low_key` |
| +0x12 | `i16` | `high_key` |
| +0x14 | `i16` | `fade_low_velocity` |
| +0x16 | `i16` | `fade_high_velocity` |
| +0x18 | `i16` | `fade_low_key` |
| +0x1A | `i16` | `fade_high_key` |
| +0x1C | `i16` | `root_key` — the sample's own pitch |
| +0x1E | `f32` | `volume` |
| +0x22 | `f32` | `pan` |
| +0x26 | `f32` | `tune` |

From offset 42 the layout becomes version dependent. Up to zone schema `0x99` the next field is a
plain `i32 filename_id`; from `0x9A` on, a 6-byte prefix comes first (its second byte is a
"sample present" flag) and `filename_id` follows only when that flag is set. [verified]

A negative `filename_id` means "no sample". Index `0` is a real sample: the first one.

When the flag allows it, a copy of the sample's WAV header follows: data type, sample rate,
channels, frame count, and a `root_note` that is **not** the zone's `root_key` (it is 60 in every
observed file while `root_key` follows the key grid). Treat the copy as a hint for allocating
before opening the file, and the file's own header as the truth when they disagree.

### 3.4 The file name table — `0x4B`

A zone does not name its sample; it stores an index into this table. Version 2 has three lists:

```c
struct FileNameTableV2 {
    u16 version;                     // 2
    u32 n_special;  FileName special[n_special];
    u32 n_samples;  FileName samples[n_samples];
    u64 sample_timestamp[n_samples];
    u32 sample_aux[n_samples];
    u32 n_other;    FileName other[n_other];
    u8  trailing[...];               // 0–6 bytes, meaning unknown, preserve
};
```

| list | holds | referenced by |
| --- | --- | --- |
| `special` | library anchors, resource containers | preset-level resource lookup |
| `samples` | every mapped sample | zone `filename_id` |
| `other` | the instrument itself, wallpapers, impulse responses, script data files | effect and UI indices |

A `FileName` is a segment array, which is how Kontakt stores a path without storing a string:

```c
struct FileName {
    i32 segment_count;               // signed: negative means corrupt
    repeat segment_count {
        u8 kind;
        if kind in {1,2,4,5,8,9}:  u32 units; u16 text[units];
        if kind in {3,6,11}:       // no payload
    }
};
```

Joining the segments with `/` gives the path, and the kinds are not interchangeable:

| kind | payload | contributes |
| --- | --- | --- |
| 1 | text | `text:` — a drive anchor, `C:` |
| 2, 4, 5, 8, 9 | text | the text, as one path component |
| 3 | none | `..` |
| 6 | none | an empty component |
| 11 | none | an empty component, which is what gives an absolute path its leading `/` |

A record with zero segments is legal and costs four bytes. [strong]

Version 3 (Kontakt 8) drops the three lists for one flat namespace: a `u32` count, then per record
an 8-byte header, a `FileName`, and 20 bytes of metadata. The segment encoding is unchanged, and the
instrument's own filename reference addresses the flat list instead of `other`. [strong]

### 3.4.1 From a name to a file

The table stores a path, the zones store an index into the table, and that is as far as the file
goes. Turning the resulting string into bytes is a convention, and libraries rely on it:

* the path is relative to the instrument's own folder, not to the library root or the current
  directory;
* a leading `/` is the exception: it addresses Kontakt's shared content root, which is where factory
  impulse responses live;
* matching is exact and case-sensitive first; only when that fails is a case-insensitive basename
  lookup tried, because libraries ship with the table's capitalisation and the filesystem's out of
  step;
* a basename that matches more than one file is an error to report, not a choice to guess;
* a resolved path that escapes the library root is rejected.

A library is allowed to have names that resolve to nothing: a preset can reference samples the user
did not install, and the rest of the instrument still has to load. [strong]

### 3.5 Script slots — `0x06`

Up to five KSP slots per instrument, each a flat record:

```c
struct BParScriptV60 {
    i32  text_len;      // negative = absent, otherwise byte length
    u8   text[text_len];// KSP source, UTF-8 (Windows-1252 fallback)
    u8   editor_open;
    u8   touched;
    u8   bypass;        // 1 = the script does not run
    u32  password_len;
    u8   password[password_len];
    i32  desc_len;      // negative = absent
    u8   description[desc_len];
    i32  textfile_len;  // negative = absent
    u8   textfile_name[textfile_len];
    u32  persistent_count;
    repeat persistent_count {
        u32 entry_len;
        u8  entry[entry_len];        // "<name> <value> <value>…", UTF-8
    }
};
```

The three "text" lengths are signed and any negative value means absent; the password and the
persistent entries use unsigned lengths, where a negative value is corruption rather than
omission. An empty slot is therefore exactly 23 bytes: four bytes of `−1`, three flag bytes, a
zero password, two more `−1`, and a zero count.

`password` is not the password. It is a fixed-size digest of one, identical on every slot of a
library that locked its scripts with the same password; its presence is what makes Kontakt refuse
to show or edit the source. Editing the source of a locked slot without knowing how to recompute
that value produces a preset that Kontakt rejects as corrupt. **[open]** as to the digest.

`persistent` is where a saved instrument remembers a user's settings: one entry per
`make_persistent` variable or array, as text.

### 3.6 Envelopes — a group's `0x3B`

Modulation records occupy presence-prefixed slots in `0x3B`/`0x3C` arrays
with the `00 12 00` prefix. Internal `0x0D` objects use version `0x80`; external `0x0C` objects use
`0x100`. Their counted public sections contain a target count and target names/depths/aliases,
then the source name. External source kind 4 includes a CC byte and four opaque assignment bytes;
kind 6 identifies velocity. The meanings of other observed source-kind numbers are unestablished. **[strong]**: checked
across the supplied instrument's groups, not established for every writer/version.

Observed external point curves store a mode/polarity byte, a bounded point count, a flag byte,
the first point's Y/shape with implicit X=0, then X/Y/shape triplets. AHDSR destinations include
their internal-envelope slot; amplitude and filter-envelope targets must stay separate. Native curve curvature, polarity interpretation and parameter time laws
remain **[open]**. The field layouts of other targets, including play-position variants, are
unestablished. The internal header before its final counted source name remains opaque.

A group's amplitude envelope is not a direct child of the group. It sits inside the modulator
array:

```
group
└── 0x3B BParameterArraySer<BParInternalMod, 16>
    └── 0x0D modulator                       one per modulated parameter
        └── 0x07 envelope wrapper
            └── 0x3F BParEnv_AHDSR
```

The envelope record is flat: a one-byte flag, a `u16` version (`0x10` or `0x11`), then six `f32`s:

```c
struct Ahdsr {
    f32 attack_curve;   // −1…+1, 0 = linear
    f32 attack_ms;
    f32 decay_ms;
    f32 hold_ms;
    f32 release_ms;
    f32 sustain;        // linear gain 0…1
};
```

**One array slot is one modulated parameter.** A group that shapes its tone with an envelope holds
*two* modulators — a `volume` one and a `filterCutoff` one — in whatever order they were added. The
envelope that belongs to the amplifier is the one whose modulator names `volume`; taking the first
envelope in the array instead is a real trap, because a guitar patch that stores the filter
envelope first then inherits a 3.6-second attack and zero sustain. [verified]

### 3.7 Effect racks — `0x3A`

An instrument has two of them (insert and send), and each is an array of **eight** slots even when
the file stores fewer: Kontakt always allocates eight.

```c
struct EffectSlot {
    u8  private[22];   // bypass, output gain, dry level, and five unestablished bytes
    u8  module[...];   // exactly one child chunk: id = the module's class id
};
```

The slot's child chunk id is the module's class id, and the module body is its parameter block in
read order. Two things are worth knowing before writing a decoder:

For module `0x18`, subtype 24 has a 44-byte public block: two little-endian `u32` type words, both
24, followed by three `(frequency_hz, Q, gain_db)` float triplets. This is a three-band parametric
EQ, not the short low/high-pass record ending in normalised cutoff/resonance. **[strong]**: the
triplets have consistent physical ranges in the inspected preset; native knob and DSP equivalence
have not been established. Other subtypes remain separate and unknown layouts stay opaque.

The short `0x18` block with type words `(7, 7)` is treated as high-pass rather than low-pass;
its tail remains the normalised cutoff/resonance pair. **[strong]**: its saved position and the
script's independently addressed HP controls agree; the exact native filter algorithm is open.

* the parameter block is **per module** and most of the layouts are undocumented, so a reader that
  keeps it raw can still round-trip what it does not understand;
* file references live in the module object's *private* section, not in the parameter block — that
  is where an impulse-response path comes from, which is why a decoder that only reads parameters
  cannot find the IR.

Class ids observed in slots: `0x10` delay, `0x13` gainer, `0x18` filter, `0x21` distortion, `0x44`
compressor. `0x45` (`InsertBus`) is the routing record for the insert chain, not a module. [strong]

---

## 4. `.nkc` — the catalogue

A packed library ships a catalogue that maps every sample and resource name to the offset of its
record in the payload container. `.nkc` has its own framing; it is not a NISound document.

```
+0x0000  header (48 bytes)
+0x0030  root node: kind, sub, version, child count, name
   ...   entries: a 20-byte tail, then a name, repeated
```

### 4.1 Header — 48 bytes [strong]

```c
struct NkcHeader {
    u32 magic;         // +0x00  0x3FE1107A
    u32 version;       // +0x04  0x00000110 (272) in every observed file
    u32 zero[5];       // +0x08
    u32 one1, one2;    // +0x1C, +0x20  both 1
    u32 zero2;         // +0x24
    u32 body_offset;   // +0x28  0x32 (50)
    u32 zero3;         // +0x2C
};
```

`body_offset` does **not** point at the root node: it reads 50 while the root sits at `0x30` (48).
Treat it as a sanity value rather than a pointer — a reader that jumps to it lands inside the
root's header.

The version word `0x110` (272) is close to the `0x0111` (273) that the `.nkx`/`.nkr` container
carries, which suggests a shared format generation rather than a coincidence — but they are not the
same number, so do not compare them.

### 4.2 Root node — at `0x30`

```c
struct NkcRoot {
    u16 kind;          // +0x00  always 1
    u16 sub;           // +0x02  0x4E69/0x4E6A in older files, 0x0014 in newer ones  [open]
    u32 version;       // +0x04  always 1
    u32 child_count;   // +0x08  immediate children; *not* the length of the flat list
    u32 name_len;      // +0x0C  UTF-16 code units
    u16 name[name_len];// +0x10  "Samples" or "Resources"
};
```

The root has no tail; the first entry's tail follows its name immediately.

### 4.3 Entries

```c
struct NkcEntry {
    u32 offset;        // absolute offset of the matching record in the payload container
    u32 size;          // payload length: 0 for a folder or a picture, set for a sample
    u16 kind;          // 1 = folder, 4 = data file
    u16 attribute;     // varies — see below
    u32 zero;
    u32 zero;
    u32 name_len;      // UTF-16 code units
    u16 name[name_len];
};
```

The two `u16`s are the pair to be careful with, because they look like one `u32` and are not: a
folder carries `kind = 1`, every file carries `kind = 4`, and the word after it is **not a
constant**. Measured: `0x0111` throughout one catalogue, one value per folder in another
(`0x0BCB` for three files of the same folder), `0xFC83`, `0x5A7`, `0x7D2D`, `0xE839` elsewhere. It
is not a hash of the name, since different names share it. Treat it as an opaque per-entry
attribute. [verified that it varies]

The list is flat and pre-ordered; there is no nesting even though the names look like paths. The
file **ends on a name** — the last entry has no trailing tail. [verified]

The order (tail first, then name) matters more than it looks. The other reading — name first, then
the next record's tail — also "works" in the sense that it parses to EOF with the right entry
count, but it pairs every name with the *following* record's offset. Checking the payload at each
offset against the name's extension settles it: tail-first matches every time, name-first matches
none. [verified]

`offset` is the field that matters: it is the absolute offset of the matching record in `.nkx` or
`.nkr`, and it is the only strictly necessary piece of `.nkc` semantics. `size` is the payload
length for a sample and zero for everything else, so it repeats what the container already says and
a reader that ignores it loses nothing. [verified for `offset`, strong for `size`, since every
catalogue on hand is a resource catalogue where it is zero]

The root's `child_count` counts the root's **immediate children**, which is not the length of the
flat list that follows: that list runs to the end of the file.

---

## 5. `.nkx` and `.nkr` — monolith containers

`.nkx` packs a library's audio; `.nkr` packs its non-audio resources (pictures, scripts, impulse
responses). They share one container format and differ only in the blob type they hold.

```
+0x0000  directory: typed records, names NUL-terminated, packed, unaligned
   ...   blob records
   ...   payload bytes
   ...   next blob record, next payload, …
  EOF
```

The directory is a flat list, not a tree, even though its names imply one. [verified]

There are **two directory layouts in circulation**, and they are not interchangeable:

* **recorded rows** (older `.nkx`): every file is a self-sizing record that carries its block's
  absolute offset, so a name can be paired with its payload with certainty. Folder records carry no
  block at all, which is why pairing by position is not enough here.
* **a bare name list** (newer `.nkr`): a flat prefix of zero-terminated UTF-16LE names, the blocks
  stored back to back in the same order. The word next to a name is *not* an offset, so the two
  lists can only be paired by order.

A reader should prefer the third source when it exists: the `.nkc` catalogue states where every
payload starts, and that join is authoritative. The heuristics are the fallback for an archive with
no catalogue.

### 5.1 Directory record

```c
struct DirectoryRecord {
    u16 size;          // total, including this header, the name and its NUL
    u32 id;            // 0x5E70AC54 = folder; otherwise an opaque key  [open]
    u16 version;       // 2 in the observed files
    u16 name[];        // UTF-16LE, NUL-terminated
};
```

`size == 8 + 2·len(name) + 2`. The name is a bare file name (`.ncw` for a sample); the directory
segment it lives in is encoded only in the preceding folder record, which is why folder records
exist at all. `id` is not a CRC or Adler of the name and not a file offset — treat it as an opaque
lookup key. [verified that it is none of those]

In the archive form that has no recorded rows, the same thing degenerates to: names, each
terminated by `00 00`, filling the prefix up to the first block.

### 5.2 Blob records

Every blob starts with a record type word:

```c
u32 type;      // 0x16CCF80A sample, 0x2AE905FA generic resource, 0x5E70AC54 folder
```

What follows the type is **not** a constant. Measured on two archives, the next two words are a
format generation and a flag set that both vary with the tool that wrote the archive:

```
fa 05 e9 2a | 11 01 00 00 | 00 00 ff 00 | .. 06 00 | c1 03 00 00 | 00 00 00 00
type          version       flags           gap       size = 961     zero
```

So do not match a 22- or 31-byte header as a magic string: agree on the type word, accept the
generations you have seen, and sanity-check the length against the remaining bytes. Believing the
older hypothesis that bytes `11 01 e3 a3` form a constant `0x1101E3A3` magic rejects every archive
written by a different tool version.

The two blob headers have different sizes `[verified]`:

```c
struct SampleBlobHeader {       // 31 bytes, then an .ncw payload
    u32 type;        // +0x00  0x16CCF80A
    u8  rest[15];    // +0x04  generation, flags and a format word (3 observed); not established
    u64 payloadSize; // +0x13  unaligned, 64-bit
    u32 zero;        // +0x1B
};                   // payload at +0x1F

struct ResourceBlobHeader {     // 22 bytes, then the resource bytes
    u32 type;        // +0x00  0x2AE905FA
    u8  rest[10];    // +0x04  generation (0x110 or 0x111) and a flag word; both vary
    u32 payloadSize; // +0x0E
    u32 zero;        // +0x12
};                   // payload at +0x16
```

The header sizes, the size field offsets and the type words are what a reader needs; the words in
between differ between archives and between authoring tools, and treating any of them as a constant
is how a reader ends up rejecting libraries that are perfectly valid.

The 64-bit size at `+0x13` is deliberately unaligned: read it byte-wise. Payloads are stored back to
back in the same order as the directory's blobs, so a reader can pair the two lists by position
after the directory prefix ends.

Some `.nkr` payloads are obfuscated, per library, and a reader must detect that rather than assume
it: a PNG that does not start with `89 50 4E 47` is not a PNG. `[strong]`

---

## 6. `.ncw` — the sample container

`.ncw` ("Native Compressed Wave") is a block-oriented, bit-packed PCM container. It has no entropy
coder and no encryption: decoding is a bit unpack plus a running sum. `[verified]`

```
+0x0000  file header (120 bytes)
+0x0078  block table: u32 × (blocks + 1), offsets relative to the data region
   ...   data
```

### 6.1 Header — 120 bytes

| offset | width | field | value |
| --- | --- | --- | --- |
| 0x00 | 4 | signature | `01 A8 9E D6` |
| 0x04 | 4 | version | `31 01 00 00` (`0x131`) in every observed file |
| 0x08 | `u16` | channels | 1 or 2 |
| 0x0A | `u16` | bits per sample | 16, 24 or 32 — the *source* depth |
| 0x0C | `u32` | sample rate | |
| 0x10 | `u32` | frames | per channel |
| 0x14 | `u32` | block-table offset | 120 in every observed file |
| 0x18 | `u32` | data offset | usually `0x14 + 4·(blocks + 1)`; use the field |
| 0x1C | `u32` | data size | equals the last table entry |
| 0x20 | `u32` | format word | 0 = PCM, 1 = float |
| 0x24 | 84 | opaque | not a file name; unused |

The block count is not stored: it follows from the gap between the table offset and the data
offset, divided by four, minus the sentinel. That count must equal `ceil(frames / 512)`, so a wrong
`frames` field is a hard error rather than a silent truncation — worth checking before decoding
anything, because the alternative is a click every 512 frames or a note that stops early.

### 6.2 The block table

`blocks + 1` little-endian `u32`s, offsets relative to the start of the data region. Entry `i` is
where block `i` begins, entry 0 is always 0, the entries never decrease, and the last one is a
sentinel equal to the data size. Those three properties make the table self-checking.

### 6.3 Sub-blocks

Each block holds 512 frames, and channels are interleaved **per block, not per file**: at each
table offset there is one 16-byte sub-block header per channel, in channel order. A stereo file
with 179 blocks has 358 sub-block headers.

| offset | width | field |
| --- | --- | --- |
| 0x00 | `u32` | signature `0x3E9A0C16` (bytes `16 0C 9A 3E`) |
| 0x04 | `u32` | base value, signed — sample 0 of the block |
| 0x08 | `i16` | `bits` |
| 0x0A | `u16` | flags |
| 0x0C | `u32` | unknown, zero in every observed file |

`bits` selects the kind of block:

* **`bits > 0` — delta coded.** 512 values of `bits` width follow. Sample 0 is the base value; each
  value is added to the previous sample. The body holds 512 values but the last one produces a
  513th sample that is never emitted, so 512 samples come out of 511 used deltas. In every observed
  sub-block that 512th value is zero — emitting it as a sample inserts a click every 512 frames.
* **`bits < 0` — raw.** 512 samples of `|bits|` width, sign-extended; the base value is ignored.

`bits` of 0 or 1 have no known meaning. Values are packed **LSB-first** — the first value occupies
the low bits of the first byte — and widths are not byte-aligned; the corpus uses every width from
2 to 24. The width is the narrowest that holds the block's deltas, with a floor of 2, and raw
blocks use the negative bit depth. That rule holds for 9 777 of 9 814 measured sub-blocks, and the
37 exceptions are always *wider* than needed, never narrower — so it is safe for validation, not
for decoding.

### 6.4 Flags

| bit | meaning |
| --- | --- |
| 0 | mid/side: sub-block 0 is mid, sub-block 1 is side, `l = m + s`, `r = m − s` |
| 1 | samples are IEEE-754 singles stored as raw bit patterns |

The flags of the **first channel's** sub-block apply to the whole block; the other channels' flag
words carry the same value. The last block is padded out to 512 frames, and the padding is dropped
using `frames` from the header. Mid/side combined with the float bit has never been observed and is
not specified.

Mid/side as written by Kontakt 4.2 is **not lossless**: it stores `(l+r)/2` and `(l−r)/2` with
truncating integer division, so an odd sum loses one LSB. The loss is already in the file; a
decoder reproduces what Kontakt plays by accepting it.

---

## 7. How a library fits together

```
Library folder
├── Instruments/
│   └── Piano.nki ─────── NISound item tree
│                          └── preset chunk (FastLZ)
│                               └── chunk stream
│                                    ├── Program: "Piano"
│                                    ├── GroupList: Close RR1, Close RR2, …
│                                    ├── ZoneList: key/vel ranges, root keys
│                                    └── FileNameTable: "Samples/C3_RR1.wav"
│
├── Samples/            ← loose .wav files both the table and the zones point at
│
│   ...or, for a packed library:
├── Samples.nkx ───────── directory + blobs, each blob an .ncw payload
├── Resources.nkr ─────── directory + blobs, each blob a picture or script
└── Samples.nkc ───────── name → offset into Samples.nkx / Resources.nkr
```

A **loose** library keeps its audio as `.wav` next to the instrument, and the file name table
resolves to those files directly. A **packed** library ships the audio as `.ncw` inside one
`.nkx`, with the `.nkc` catalogue bridging names to offsets. The instrument itself is the same in
both cases: zones hold indices into the name table either way, so the only difference a reader sees
is whether the resolved name ends in `.wav` or refers to a catalogue entry.

---

## 8. What is not established

* The meaning of the `.nkc` root's `sub` field (`0x4E69` / `0x4E6A` / `0`).
* The `.nkx` directory `id`: not a CRC, an Adler, or an offset. Needed to *write* an `.nkx`,
  not to read one.
* The `.ncw` `format` word's other values; only `3` has been observed on sample blobs.
* The per-entry attribute word in an `.nkc` entry: not a name hash, not constant, one value per
  folder in some catalogues and one value per file in others.
* `bits == 0` and `bits == 1` sub-blocks; none have been observed.
* The `password` digest in a script slot: what it hashes, and therefore how to change a locked
  script without breaking the preset.
* The private sections of Program, group and zone: structure is visible, meaning mostly is not.
* The per-version "suffix" of Program's public section beyond the versions that have been mapped.
* Whether the order of blobs in an `.nkx` matching the order of `.nkc` entries is guaranteed or
  incidental.
* The `.nkx` "folder record" layout is only partly decoded; its role is clear, its fields are not.

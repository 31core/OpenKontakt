# KSP panels: how a library draws its interface

A Kontakt instrument's interface is not artwork with knobs placed on it. It is a **script** that
declares controls, positions them, assigns pictures to them and then moves them around at runtime —
the artwork is just a label you can also load.

This document describes how a panel is laid out: the coordinate system, how a control gets its size,
how pictures and sprites work, how pages are built, and where the background comes from. The
language itself is `docs/ksp.md`; the file layout of a script slot is `docs/format.md` §3.5.

## 1. What the panel is

**1.1** The panel is whatever the *executed* script left on screen — not what the file declared. A
script moves, resizes, hides and repaints controls from its callbacks, and a page switch is nothing
but a batch of `set_control_par` calls. A player that renders the declarations instead of the
executed state shows the wrong page, the wrong labels and the wrong values. [verified]

**1.2** Every control is a variable (`$KICK`, `$BUT_MAIN`, …). Its value *is* the script's variable,
so moving a fader and running `on ui_control` are the same event from two sides.

**1.3** An instrument can have more than one panel — up to five scripts, each with its own
controls. Kontakt shows one script's page at a time.

## 2. The coordinate system

A control is positioned in one of two ways, and the mode is per control.

**2.1 Grid (default).** `move_control(x, y)` and `declare ui_* $c (x, y, w, h)` place a control on a
fixed lattice in native pixels:

```
x = 66 + (grid_x − 1) · 92
y =  2 + (grid_y − 1) · 21
w = grid_w · 92 − 5
h = grid_h · 21 − 3
```

So a row is 21 px tall, a column 92 px wide, the panel's origin is 5 px in from the left and 2 px
down, and a control never quite fills its cell — the 5 px and 3 px are the gaps between neighbours.
The `66` is the width reserved to the left of the grid. [verified]

**2.2 Native pixels.** `move_control_px(x, y)` places a control at absolute pixels, and
`set_ui_width_px` / `set_ui_height_px` set the panel's own size. Panels authored this way also give
every control an explicit `$CONTROL_PAR_WIDTH` / `$CONTROL_PAR_HEIGHT`. Mixing the two forms in one
script is legal; the mode is per control, not per panel. [verified]

**2.3 Panel height.** `set_ui_height(rows)` means `rows · 21 + 2`; `set_ui_height_px(px)` is exact.
When several scripts set a height, the tallest wins. A panel may be taller than the window it is
shown in, in which case it scrolls rather than being scaled down.

## 3. How big is a control?

The size is decided in this order, and the order is the part that surprises people:

1. a **non-resizable sprite** draws at its own native size, whatever `$CONTROL_PAR_WIDTH/HEIGHT`
   say. A library whose dial labels are 52×23 sprites declared as 80×30 controls means it: stretched
   labels overlap their neighbours. [strong]
2. otherwise explicit `$CONTROL_PAR_WIDTH` and `$CONTROL_PAR_HEIGHT`, when both are non-zero;
3. otherwise the sprite's frame size;
4. otherwise the grid cell.

## 4. Pictures and sprites

**4.1 Where they come from.** A picture is a name, resolved against the library's own
`Resources/pictures/<name>.png`, or against the resource archive when the library ships none
(`docs/format.md` §5). A name that resolves to nothing draws **nothing** — Kontakt ships built-in
skins with names that are not in the library, and inventing a default knob for those adds controls
the original panel does not have. [strong]

**4.2 The sidecar.** Every sprite is a PNG plus a `.txt` of the same name, and the sidecar is what
tells a renderer how to slice it:

```
Has Alpha Channel: yes
Number of Animations: 50
Horizontal Animation: yes
Vertical Resizable: no
Horizontal Resizable: no
Fixed Top: 0
Fixed Bottom: 0
Fixed Left: 0
Fixed Right: 0
```

`Number of Animations` is the frame count, and `Horizontal Animation` says whether the frames are
laid out along x or y. A 50-frame horizontal sheet is a fader: the control's value picks the frame.
Without the sidecar the sheet is a single image. [verified]

**4.3 Animation.** The frame shown follows the control's normalised value, so a knob or a fader is
one control and one sheet. A one-frame picture is the whole texture at every value — a button whose
value is 1 still shows its single frame, which is why page buttons swap *pictures* rather than
animating one.

## 5. Pages

**5.1 Hiding.** `$CONTROL_PAR_HIDE` (and `hide_part`) takes a bit mask:

| bit | value | hides |
| --- | --- | --- |
| background | 1 | the control's own background |
| title | 2 | the text label |
| value | 4 | the value readout |
| mod light | 8 | the modulation indicator |
| whole control | 16 | the control itself |

**5.2 The page pattern.** A paged panel hides every control, then un-hides the ones belonging to the
current page — usually by a `function show_<page>` that sets the background picture and the
thirteen-odd `$CONTROL_PAR_HIDE` values. A renderer that ignores the whole-control bit draws every
page on top of every other one. [verified]

**5.3 Page buttons.** A page button is a `ui_button` whose *picture* is swapped between an `…_On`
and an `…_Off` sprite: the highlight is a different image, not a colour or a size. The button also
holds 1 while its page is current, which is what makes a radio group out of five buttons. [verified]

## 6. Controls

| kind | behaves like |
| --- | --- |
| `ui_knob` | rotary, value picks the sprite frame |
| `ui_slider` | linear, value picks the sprite frame |
| `ui_button` | momentary in Kontakt's model — the script decides what stays on |
| `ui_switch` | toggles between its two ends |
| `ui_menu` | opens a list; the chosen row is stored as the value |
| `ui_label` | text only, but it can carry a picture and is how a whole panel background is drawn |
| `ui_value_edit` | a numeric field with a caret |
| `ui_table` | a row of bars, one per array element |
| `ui_waveform`, `ui_wavetable`, `ui_xy` | specialised display controls |

**6.1 Menus.** Rows are added with `add_menu_item($menu, text, value)`; the stored value is what the
script compares against. A menu declared with no range starts at 0…0, so a renderer must grow its
range as rows are added or every selection clamps to the first row.

**6.2 Value edits are drawn underneath.** Kontakt paints `ui_value_edit` boxes *below* picture
controls, so a panel that parks a numeric field under a button hides it. Drawing them last puts the
keyswitch fields on top of the articulation artwork. [strong]

**6.3 Mouse behaviour.** A negative `$CONTROL_PAR_MOUSE_BEHAVIOUR` makes a control drag relatively
(the value moves with the pointer's *motion* rather than its position), which is how a custom
dot-and-line handle is made to feel like a knob. The pixels-per-range law is [open]; a player that
ignores the parameter still works, but those handles drag like sliders.

## 7. The background

**7.1 Two sources.** The wallpaper is either a file the program's filename table names, or a picture
the script assigns with `set_control_par_str($INST_WALLPAPER_ID, $CONTROL_PAR_PICTURE, name)`. A
library can use either; a renderer that only looks at one of them shows a black panel for half the
libraries in existence. [verified]

**7.2 The panel's size.** The panel is the rectangle the artwork is measured against:

* width is `set_ui_width_px`, or **633** — Kontakt's fixed grid width — when the script never
  sets it;
* height is the **declared** `set_ui_height_px`, or `set_ui_height(rows)` as `rows · 21 + 2`;
* when nothing is declared, the picture's own aspect at that width is the honest answer, because
  the picture is the panel.

Two temptations have to be resisted here, and both were bugs that took several libraries to
notice:

* **do not clamp the panel to the visible area.** A 750 px panel in a 500 px view still ends at its
  own bottom edge; clamping it drags everything on it upward by the difference.
* **do not let a runtime height override the declared one.** A script asking for a taller panel at
  runtime does not move the artwork with it. One panel declares 750 px and ships an 818 px
  wallpaper; the 68 px difference is exactly the offset the bottom anchor produces, and taking the
  runtime height instead puts every label 68 px off.

**7.3 Scale.** Everything — wallpaper, controls, text — scales by **width**:
`scale = drawn_width / ui_width`. The height follows the panel, never the reverse: artwork two pixels
wider than the grid bleeds one pixel over each edge, and a panel is not shrunk to fit its own
height. [verified]

**7.4 Two kinds of wallpaper, two anchors.** This is the part that took the longest, because both
kinds are “an image bigger than the panel” and they are anchored differently.

*An ordinary wallpaper*, at most a little taller than the panel, is the panel's own artwork with
its edges bleeding over. It is pinned to the **bottom**:

```
image_top = panel.bottom − image_height
```

That is what keeps the labels baked into the image under the controls placed on them: one panel is
633 × 329 of artwork for a 260 px view, and top-aligning it puts every caption one row too high.

*A stacked page sheet* is one image holding every page of the panel. It is recognised by a vertical
period: the image repeats its first rows at the page boundary, and the period is the offset of the
first later row that matches the top rows. The match needs a tolerance — a re-encoded PNG never
comes back byte-identical — so compare a band of rows with a few levels of slack, not for equality. When that period is found — and the image is
more than 1.5 × the panel's height — the page is **centred** on the panel:

```
shift     = max(0, (page_height − panel_height) / 2)
image_top = panel.top − shift
```

One library's page is 668 px tall for a 536 px view, so the visible band starts 66 px into the page;
the reference agrees within a couple of pixels. Centring instead of bottom-aligning is what makes a
paged panel's controls land on their own page's labels rather than on the last page's. [verified]

**7.5 The skin offset.** `set_skin_offset(native_px)` scrolls the image up by that many image pixels,
which is how a script selects a page out of a sheet it did not stack. The image moves, the panel does
not, and the controls stay where the script put them. [strong]

**7.6 Taller than the view.** When the panel is taller than the area it is drawn in, it scrolls; the
artwork scrolls with the controls, because both live in the same native-pixel canvas. What does not
happen is scaling to fit — a 750 px panel in a 500 px window is a scrolled panel, not a shrunken one.

## 8. What is not established

* `$CONTROL_PAR_Z_LAYER`: scripts set it, and its effect on draw order has not been measured.
* `Fixed Top/Bottom/Left/Right` in a sidecar: the values are there, the nine-patch rule that uses
  them is not.
* The exact pixels-per-range law of relative dragging.
* How a control's value maps to a frame when the sheet's frame count does not divide evenly.
* Whether Kontakt clips a panel wider than the window or scales it.

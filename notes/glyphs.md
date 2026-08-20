# Glyphs, the VT engine, and input

Milestone 29's remaining increment: the piece that turns a framebuffer into a terminal a person can
read. Rung one put pixels on a screen ([framebuffer-contract.md](framebuffer-contract.md)) and rung
two multiplexed the screen among mutually distrusting clients ([compositor.md](compositor.md)).
Neither could show a letter.

The code halves are `crates/bitfont` (the font), `crates/video_terminal` (the grid engine, the keymap, and the
test script), `user/src/display_terminal.rs` (the terminal component), and `user/src/kbd.rs` (the keyboard
driver). This is the prose half.

## The shape

```text
  virtio-input ──virtio (PCIe, IOMMU)──► kbd ──the input ring──► compositor ──OP_BYTES──►┌───────┐
  (a keyboard)                                                                            │ display_terminal │
                                                         application ──OP_WRITE──────────►└───────┘
                                                                                              │ glyphs
                                                                    its surface ◄─────────────┘
                                                                         │
                     display ◄──gfx FLUSH(damage)── compositor ◄──COMMIT─┘
```

Everything above the surface is text; everything below it is rung one's contract, unchanged.

## The font: public domain, and that is why this one

`crates/bitfont` is an 8x8 monochrome bitmap font and a pure function from `(byte, x, y)` to a
colour.

**The source is `font8x8` by Daniel Hepper**, which is **public domain**, derived from Marcel
Sondaar's `font8x8.h`, also public domain, which traces back to IBM's public-domain VGA fonts.
Upstream is <https://github.com/dhepper/font8x8>; the U+0000..U+007F block is transcribed into
`crates/bitfont/src/glyphs.rs` by a script with no bits changed, and the provenance is in that file's
header.

Public domain drove the choice, and the reason is worth stating: **a bitmap font is compiled into the
image**, so its licence travels with the artefact rather than with a build-time tool. Terminus
(OFL-1.1) and Spleen (BSD-2-Clause) are both good fonts with compatible licences, and either would
have attached an attribution obligation to every binary that draws text. This one attaches none.

**Why a bitmap font at all**, rather than something scalable: a TrueType rasteriser (or `cosmic-text`
above it) wants an allocator, floating point, hinting, and a font file to load from a filesystem.
Every one of those is a dependency a `no_std`, allocation-free userspace component would have to
acquire before it could draw the letter A. A bitmap font needs none: the glyphs are a `static` in
`.rodata` and drawing one is a bit test.

It buys something else, and that is the real argument. Because rendering is a pure function, **the
expected picture is a value that more than one party can compute**. That is what makes the text on
the screen provable rather than plausible (below). Rung three will want the scalable path; this rung
would not have been checkable with it.

Two details the font's own tests pin, because both are invisible in review:

- **Bit 0 is the leftmost pixel**, which is the opposite of the convention most bitmap fonts use. A
  mirrored alphabet is nearly undetectable by eye (half the letters are symmetric enough), so the
  test spells out the shape of `F` as ASCII art. It earned its keep on the first run: it was written
  against an *invented* picture of `F` and the real font refused it.
- **No two printable glyphs are identical**, and every printable byte has one. A table transcribed
  with a dropped row would show up here as a collision or a hole and nowhere else until it was on a
  screen.

### The options, with pictures (2026-08-19, lane `bench/font-options`)

**First, a correction to the framing above.** The paragraph about Terminus and Spleen reads as
though the font were a compromise forced by licensing. It is not: `font8x8` is public domain, which
was verified for this survey by opening the upstream project rather than by recalling it. Its
`README` says, of the header files this table came from:

```text
Author: Daniel Hepper <daniel@hepper.net>
License: Public Domain
```

and the credits below that carry Marcel Sondaar's original header, which says the same thing and
names IBM's public-domain VGA fonts as the source. **Nothing in this tree owes an obligation to
anyone for the letters on the screen**, so a change of font would be a decision about how it looks.

**And a reversal on top of that correction, which is calef's** (2026-08-19). The old rule refused
any font with an attribution obligation, on the ground that a bitmap font is compiled into the image
and its licence therefore travels with the artefact. The first half of that reasoning stands; the
conclusion does not. He read OFL 1.1 and his verdict was that the obligation "actually doesn't look
onerous", so **obliging licences are in scope and priced rather than refused.** That matters,
because the public-domain corner of this field is small and the well-drawn fonts mostly live under
the OFL.

**The specimen sheet is how the aesthetic question gets answered.** The crate's design claim is that
the expected picture is a pure function, which is what lets the terminal, the kernel test and the
host-side scanout check agree about a letter. Spend the same property on the choice itself and a
font stops being an argument:

```text
cargo run -p bitfont --example specimen                          # what ships
cargo run -p bitfont --example specimen -- --dots                 # one character per pixel
cargo run -p bitfont --example specimen -- --font ter-u16n.bdf --name terminus-16
cargo run -p bitfont --example specimen -- --font bench/font-options/hand-drawn-8x8.art
```

It reads the three formats a bitmap font actually arrives in: `.hex` (GNU Unifont), `.bdf` (Adobe,
which is what Terminus, Spleen and every X11 bitmap font ship as), and the `.art` `#`/`.` picture
that is the only sane way to author one by hand. Every font gets the same sample text, chosen for
where small fonts fail: `Il1|` and `O0` (the confusions that ruin a hex dump), `rn` against `m` (the
one that makes prose *wrong* rather than ugly), the descenders `g q y p j`, and two lines of
ordinary prose and ordinary code, because a font that looks good on a pangram and bad in a sentence
is bad.

**The candidates, what they cost, and what they oblige.** "Grid" is the decisive practical column:
characters by rows on the display ladder's 128x64 scanout, which is 16x8 today.

| Font | Cell | Table | Grid | Licence | Reserved name |
|---|---|---|---|---|---|
| `font8x8` (ships) | 8x8 | 1024 B | 16x8 | Public domain | none |
| hand-drawn | 8x8 | 1024 B | 16x8 | ours | none |
| `unscii-8` (+`-alt`, `-thin`, `-mcr`) | 8x8 | 1024 B | 16x8 | Public domain / CC0 | none |
| `spleen-5x8` | 5x8 | 1024 B | **25x8** | BSD-2-Clause | none |
| `terminus-12` | 6x12 | 1536 B | 21x5 | OFL-1.1 | **"Terminus Font"** |
| `terminus-14` | 8x14 | 1792 B | 16x4 | OFL-1.1 | **"Terminus Font"** |
| `gohufont-14` | 8x14 | 1792 B | 16x4 | WTFPL v2 | none |
| `kaypro-ii` (`81-146a`) | 7x8 | 1024 B | **18x8** | **none stated** | **excluded, see below** |
| `terminus-16` | 8x16 | 2048 B | 16x4 | OFL-1.1 | **"Terminus Font"** |
| `spleen-8x16` | 8x16 | 2048 B | 16x4 | BSD-2-Clause | none |
| `unscii-16` | 8x16 | 2048 B | 16x4 | Public domain / CC0 | none |

Where each licence was read, since a claim from memory is a claim to mark as such: `font8x8`'s
`README` at `github.com/dhepper/font8x8`; unscii's `README.md`, whose line 18 says "You can consider
it Public Domain (or CC-0) except for the files derived from ... Unifont (unifont.hex, hex2bdf.pl,
unscii-16-full.*) which fall under GPL", an exception that does not touch `unscii-8` or `unscii-16`;
Terminus's own `OFL.TXT` inside `terminus-font-4.49.1.tar.gz`, which opens "Copyright (C) 2020
Dimitar Toshkov Zhekov, with Reserved Font Name "Terminus Font""; Spleen's `LICENSE` at
`github.com/fcambus/spleen`, two-clause BSD; and gohufont's `COPYING-LICENSE`, whose entire terms
are "0. You just DO WHAT THE FUCK YOU WANT TO."

**What an obligation would actually cost us**, in the order that matters:

- **The Reserved Font Name is the expensive clause, and only Terminus has one.** Being picky about
  fonts means eventually fixing a glyph, and under the OFL the moment a glyph changes the table is a
  Modified Version, which may not carry the reserved name without written permission. So adopting
  Terminus means either never touching it or renaming our copy. Spleen (BSD-2) and gohufont (WTFPL)
  reserve nothing, and a redrawn glyph costs nothing beyond the notice.
- **The OFL has no cure period.** Its own words are that the licence "becomes null and void" if a
  condition is not met, so shipping the notice has to be a mechanism rather than an intention.
- **Where the notice would live**, three places, because the obligation attaches to the image and
  not to the source tree. The font's source and its `LICENSE` in `vendor/`, registered in
  `vendor/README.md` the way the RedoxFS pin is. The identifier in `deny.toml`'s shared licence
  policy, with the honest caveat that `script/supply-chain` checks the **cargo graph**, so a font
  transcribed into `crates/bitfont/src/glyphs.rs` is on the register rather than on the gate. And a
  page in milestone 40's documentation store, so a machine running nife carries the text it owes.

**What is still excluded.** The Linux console's `lib/fonts/font_8x16.c` is the familiar IBM VGA
shape, and its first line is `// SPDX-License-Identifier: GPL-2.0`; copyleft on a table compiled
into every binary is a different question from attribution, and it is out. Fixedsys Excelsior is
called public domain by unscii's `README`, but that is a third party's summary and
`fixedsysexcelsior.com` does not resolve (checked 2026-08-19), so the claim cannot be read at its
source. **Ambiguous is treated as obliged.**

**What the shapes measure**, over the 52 letters, straight from the tables:

| Font | Ink per letter | Left edge sigma | Width sigma | Cap | x-height | Descender |
|---|---|---|---|---|---|---|
| `font8x8` | 24.6 | 0.27 | 0.79 | 7 | 5 | 1 |
| hand-drawn | 15.5 | 0.19 | 0.61 | 7 | 5 | 1 |
| `unscii-8` | 23.2 | 0.44 | 0.61 | 7 | 5 | 1 |
| `unscii-8-thin` | 14.4 | 0.27 | 0.77 | 7 | 5 | 1 |
| `spleen-5x8` | 11.8 | 0.23 | 0.30 | 6 | 5 | 1 |
| `terminus-12` | 15.6 | 0.34 | 0.52 | 8 | 6 | 2 |
| `terminus-14` | 20.0 | 0.41 | 0.80 | 10 | 7 | 2 |
| `gohufont-14` | 19.1 | 0.44 | 0.74 | 9 | 7 | 3 |
| `kaypro-ii` | 13.6 | 0.27 | 0.48 | 7 | 5 | 1 |
| `terminus-16` | 20.1 | 0.41 | 0.80 | 10 | 7 | 3 |
| `spleen-8x16` | 33.6 | 0.46 | 0.74 | 10 | 7 | 3 |
| `unscii-16` | 34.6 | 0.38 | 0.52 | 11 | 7 | 3 |

Ink per letter is weight, the two sigmas are consistency (which is what the eye reads as rhythm
rather than as any one glyph), and the descender column is how many rows a `g` gets below the
baseline an `x` sits on. **Descender depth is what separates the sizes**, and it is why every 8x8
font here, ours included, has a cramped `g`: one row against three.

**The cell size is a screen decision before it is a taste decision, and the screen turned out not
to be free to move.** The scanout is 128x64, so an
8x8 cell gives the 16x8 grid recorded under Honest limits, and any 8x14 or 8x16 font gives 16x4.
Four rows of text is not a terminal. Two candidates dodge that entirely by being narrower rather
than shorter: Terminus 6x12 gives 21x5, and Spleen 5x8 gives **25x8**, which is more columns than
today at the same number of rows. A narrower cell costs nothing but `GLYPH_W`, which is a constant
in `crates/bitfont` that three parties read.

**Growing the scanout is blocked, and it is blocked on the capability model rather than on memory**
(measured 2026-08-19, when a lane tried to build the chosen font onto a terminal-sized surface). A
`Frame` capability names exactly one page and each one occupies a cspace slot, the cspace has
sixteen slots, and the virtio-gpu driver's DMA region already uses nine of them. The hard ceiling is
`SURFACE_FRAMES <= 9`, which is 36,864 bytes: every non-square shape inside it (128x72, 144x64,
192x48) gives five text rows or fewer at 8x14, so **there is no scanout reachable today on which
gohufont-14 is a terminal.** 800x600, which is 100x42 characters and the size calef picked, needs
469 frames. The build fails rather than the boot, because `display_service::DRIVER_SLOT_DMA` carries
a `const` assertion for exactly this. The fork, its three priced options and the sizing arithmetic
that goes with it are in notes/frames.md's `BUGS`; until it is answered, gohufont-14 is the right
font for a screen this tree cannot yet make.

**Authoring our own is priced too**, because it was raised as an option and no existing sample can
answer it. `bench/font-options/hand-drawn-8x8.art` is 95 printable glyphs drawn for this survey, in
the `#`/`.` format the specimen tool reads, at exactly the cost of any other 8x8 font: 128 glyphs by
8 rows is 1024 bytes, and 8x16 would be 2048. The drawing cost was one lane for 8x8; 8x16 would be
more than twice that, because sixteen rows is where drawing skill stops being hidden by the grid.
The honest assessment of the result is in the `BUGS` note below, and the short version is that it is
consistent and plain rather than good.

### The Kaypro II character ROM: found, rendered, and excluded

calef owned a Kaypro II and asked what its font was. It was not a typeface with a designer; it was a
chip, and the chip has been dumped. The dump renders, it is genuinely the Kaypro II's, and **it is
excluded on licence**, which is the same answer this file already gives Fixedsys Excelsior and for
the same reason.

**Which machine, and how that is known.** MAME's `src/mame/kaypro/kaypro.cpp` gives the `kayproii`
machine one `"chargen"` region holding `81-146.u43`, 2048 bytes, `CRC(4cc7d206)`
`SHA1(5cb880083b94bd8220aac1f87d537db7cfeb9013)`. Don Maslin's archive at
`retroarchive.org/maslin/roms/kaypro/` lists `81-146A` as "Kaypro II/4/83 character generator", and
`github.com/ivanizag/kaypro-disassembly` carries `chars/81-146a.bin`, whose own `README` says
"81-146a: Kapyro II/83, downloaded from Retroarchive". That file is **bit-identical to MAME's**:
2048 bytes, the same SHA-1 and the same CRC-32, checked 2026-08-19. So the artefact is the Kaypro
II's own, not a later model's, and two archives and one emulator agree on it.

The distinction matters because the line ran on. The only Kaypro font in circulation under a clear
licence is VileR's `Kaypro2K` in the Ultimate Oldschool PC Font Pack (CC BY-SA 4.0), and its own
entry says it is the **Kaypro 2000**, a 1985 PC-compatible laptop. Different machine, different
decade, different font. A dump labelled "Kaypro" is not automatically the II.

**The geometry, which is a fact about the video board rather than about the file.** A character
generator has no header, so it is read by knowing the wiring. MAME's `kaypro_v.cpp`
`screen_update_kayproii` indexes it as `m_p_chargen[(chr<<3) | ra]` with `ra < 8`, shifts out
`0, BIT(gfx,4), BIT(gfx,3), BIT(gfx,2), BIT(gfx,1), BIT(gfx,0), 0`, and its comment says "The first
half of the character generator is blank, with the visible characters in the 2nd half. During the
'off' period of blanking, the first half is used. Only 5 pixels are connected from the rom to the
shift register, the remaining pixels are held high." Reading the bytes says the same thing: the
first 1024 are uniform and the font is the second 1024.

So the cell is **7 pixels wide by 10 scanlines**, of which the ROM supplies **5 ink columns by 8
rows** and the hardware holds the rest blank, on an 80x24 display. The specimen tool derives all of
that from the bytes and prints `7x8 cell, 1024 B table, 94/94 printable drawn, 18x8 on 128x64`.

**The licence, which is three questions and not one.** They are answered separately because they
have different answers, and collapsing them is how a tree ends up shipping something it cannot
account for.

1. **The 1982 bits.** A copyrighted work of Kaypro Corporation, which went through Chapter 11 and no
   longer exists. No release, dedication or grant was found. There is a real argument that the
   subject matter is not protected at all, since 37 CFR 202.1 lists as material not subject to
   copyright both "mere variations of typographic ornamentation, lettering or coloring" and, flatly,
   "(e) Typeface as typeface". That is a defence and not a licence, and it has never been tested on
   this artefact.
2. **The dump.** Somebody's labour, and nobody's stated terms. Retroarchive publishes it with no
   licence statement; `ivanizag/kaypro-disassembly` has no `LICENSE` file, which under GitHub's own
   terms leaves it all-rights-reserved. MAME records the hash and does not distribute the bytes.
3. **A recreation.** None exists for this ROM. The one clearly-licensed Kaypro font is the wrong
   machine, as above.

**Ambiguous is treated as obliged**, which is this section's existing rule, so **the ROM is not in
this repository and the font is not a candidate.** What is committed is the reader and the recipe:
the specimen tool grew a `.rom` format, and the two commands below reproduce everything above from a
file you fetch yourself. That is MAME's own posture, which is to ship the hash and not the bits.

```text
curl -O https://raw.githubusercontent.com/ivanizag/kaypro-disassembly/master/chars/81-146a.bin
shasum -a 1 81-146a.bin   # 5cb880083b94bd8220aac1f87d537db7cfeb9013
cargo run -p bitfont --example specimen -- --metrics \
    --font 81-146a.bin --name kaypro-ii --font gohufont-14.bdf --name gohufont-14
```

**How it actually looks, next to `gohufont-14`**, which is the current pick. The two are drawn
interleaved by giving `--font` twice, because a specimen in a section of its own is an inventory.

The Kaypro is **the most evenly-fitted font in this survey bar one**. Its left-edge sigma is 0.27
and its width sigma 0.48, against gohufont's 0.44 and 0.74; only Spleen 5x8 is tighter. That
evenness is real and it is the thing a person remembers about the machine. But it is the evenness of
a **constraint rather than of a design**: five columns leaves exactly one way to draw most letters,
so the regularity is enforced rather than chosen, and it is paid for twice. `M` and `W` come out as
exact vertical mirrors of each other, distinguished only by which end the middle spike sits at. And
the descender is **one row against gohufont's three**, so `g p q y j` all descend, but by a single
row each: a hook where gohufont has a tail.

It is also light, 13.6 ink per letter against 19.1, which is the weight that suited a green phosphor
tube whose bloom filled the strokes in. On a modern panel with no bloom it reads thin.

**The verdict, plainly: gohufont-14 is the better font, and it is not close on the letterforms.**
Nine rows of cap against seven, three rows of descender against one, a `g` with a real tail rather
than a hook, and an `M` and `W` that cannot be confused. (The two draw `a` the same way, so that is
not one of the differences, and an earlier draft of this paragraph said it was.) Its higher sigmas are the good kind of
variation, letters given their own width rather than padded to a grid, which reads as typographic
where the Kaypro reads as gridded.

**The one place the Kaypro wins is the screen, and it wins decisively**: 18x8 against 16x4 on the
128x64 scanout, more columns than the shipped `font8x8` and twice the rows of anything 14 tall. The
paragraph above about four rows not being a terminal applies to `gohufont-14` and not to this. That
is a genuine tension and it is not resolved by the licence answer, because the licence answer only
removes this particular font: **a 7x8 or 5x8 cell is what the scanout wants, and Spleen 5x8 is the
candidate that offers it under a licence we can take.**

### The rest of the era, and the one failure mode they share

calef asked whether any other early machine had a font worth wanting. Four were checked, and the
answer is the same for all but one, for a reason that is more useful than the individual verdicts:
**a permissive notice on a retro-font repository usually covers the packaging and not the glyphs**,
because the glyphs were somebody else's to begin with. Once that is the question you ask, the field
empties fast.

| Font | The original bits | The dump or recreation | Verdict |
|---|---|---|---|
| Kaypro II `81-146a` | Kaypro Corp, 1982, no grant | Retroarchive and `ivanizag`, no licence stated | **excluded** |
| DEC VT220, `htayj/DEC-Fonts` | DEC, ROM-derived via VT100.net | MIT, and the repo says it does not reach the glyphs | **excluded** |
| DEC VT220, GlassTTY | Slavinsky's own redrawing | Unlicense, public domain | **clean, but not a bitmap** |
| BBC Micro 8x8 | Acorn, in the MOS ROM | Linux's copy is GPL-2.0; others are ROM extractions | **excluded** |
| Atari ST 8x16, `ntwk/atarist-font` | Atari, the TOS high-res font | BSD-3 over what its own README calls a rebranding | **excluded** |
| VileR's Oldschool PC pack | various | CC BY-SA 4.0 | **excluded, share-alike** |

**`htayj/DEC-Fonts` is the one that handles this correctly, and it is worth reading for that alone.**
Its `README.org` says the fonts "incorporate historical DEC VT220 glyph data from a ROM-derived
image published by VT100.net" and then, in the same paragraph, that "The MIT grant covers only
rights held by the repository author and does not relicense pre-existing material." That is the
honest form of the sentence every other repository here leaves out. It also disqualifies the font,
and the author evidently knew that and wrote it anyway.

**`ntwk/atarist-font` is the same situation without the sentence.** Its `LICENSE` is a BSD-3-Clause
notice reading "Copyright 2015 ntwk", while its `README.md` describes the work as "a rebranding of
the high-resolution system font originally featured on the Atari ST home computer" and credits the
file it is based on to a third-party retro-fonts page. A notice cannot grant rights its author never
held, so this is Fixedsys Excelsior again: a third party's summary of someone else's licence, which
this file already refuses.

**The BBC Micro is excluded twice over**, which is worth stating because the obvious source is the
trap. Linux carries the font at `lib/fonts/font_acorn_8x8.c` under the same `GPL-2.0` that already
put `font_8x16.c` out, and copyleft on a table compiled into every binary is settled here. Every
other copy found is an extraction from Acorn's MOS ROM with no licence attached, which is the Kaypro
answer. There is no third source.

**GlassTTY VT220 is the one clean licence in the table and still cannot be rendered here.** Viacheslav
Slavinsky's `LICENSE` opens "This is free and unencumbered software released into the public domain",
and because it is his own redrawing rather than a ROM trace, that grant reaches the glyphs. But it
ships as a TrueType outline, and turning an outline into an 8-pixel bitmap needs a rasteriser and a
threshold, which is a **design decision rather than a conversion**: the specimen would then be a
picture of our hinting rather than of Slavinsky's font. Rendering it honestly means a FreeType or
FontForge dependency, and §46 says a dependency is a decision. It is left as a **proposed
milestone** rather than done badly.

**What the survey is actually evidence for.** The era's fonts are not available, and the two that
are already in this comparison are the exception rather than the sample: `unscii` is public domain
because its author redrew it, and `gohufont` is WTFPL because its author wrote it. **A font this
project can use is one somebody made and gave away, not one a machine once displayed.** That is the
whole reason the shortlist looks the way it does, and it will not change by looking harder.

**None of these needs a loader, a rasteriser, a filesystem or an allocator**, which is the property
that keeps them candidates at all. Anything that did would break the rendered picture as a pure
function, and with it the three-party agreement that proves the text on the screen.

## The VT engine: sans-IO, and checked against the real line discipline

`crates/video_terminal` keeps the grid: bytes in, a character grid out, plus the rectangle that changed. It holds
no endpoint, makes no syscall, and has never heard of a framebuffer, exactly as `line_editor` does for
the serial terminal ([line-discipline.md](line-discipline.md)).

**What it implements is not a guess.** The escape sequences a display terminal must understand are
the ones the line discipline already emits (DECISIONS §21), and rather than assert that from a
hand-written list that could drift, the crate's interoperability test **runs the real `line_editor`**
and feeds its echo stream to this parser: type a line, back up, insert, delete, kill, press Enter,
press ^L, and the grid must show the line the discipline says it assembled. Two separately-correct
components now fail together or not at all.

On top of that: printable bytes with deferred wrap, `CR`, `LF` with scrolling, `BS`, `TAB`, `BEL`
(ignored), `CSI A/B/C/D`, `CSI H`/`f`, `CSI J` and `CSI K` in all three modes, `CSI m` (reset, bold,
reverse, the eight ANSI colours and the bright foregrounds), and `ESC c`. Anything else is swallowed
whole.

Three decisions inside it worth reading:

- **Deferred wrap.** Writing into the last column leaves the cursor there and arms a pending wrap;
  the *next* printable does the wrap. Without it, a line that exactly fills the width scrolls the
  screen before anything asked it to, and a `CR` arriving right after the last character finds the
  cursor a row too low. That is the difference between a grid and a terminal.
- **Bold is bright.** A bold weight needs a second font and at 8x8 a bold face is a smudge. Every
  terminal since the DEC VT has answered SGR 1 by brightening, which is why the palette has eight
  bright entries.
- **The cursor is part of the picture**, drawn by inverting its cell rather than overlaid. That keeps
  the screen a pure function of the state: a test that predicts the screen predicts the cursor too,
  and a cursor left in the wrong place is a failure rather than a cosmetic difference nobody notices.

**A bug the tests caught before anything reached a screen**, recorded because it is a real terminal
bug and not a toy one: an OSC sequence (`ESC ]0;title BEL`, how every program sets a window title)
printed the title onto the grid, because the parser had no string state. It has one now, and the
test that found it feeds a title-setting sequence on purpose.

## The terminal: a client at both seams, and the same binary

`user/src/display_terminal.rs` serves the terminal contract's IPC half
([terminal-contract.md](terminal-contract.md)) against a grid and a font instead of a serial line.
One binary, two wirings, chosen by `arg0`:

| | `MODE_DISPLAY` | `MODE_WINDOW` |
|---|---|---|
| slot 0 | report endpoint | report endpoint |
| slot 1 | the **display** endpoint, WRITE (rung one) | the **doorbell**, WRITE (rung two) |
| slot 2 | the terminal endpoint, READ (it serves) | the terminal endpoint, READ (it serves) |
| mapped | the scanout, an application's output page | its control page, its surface, an output page |
| presents by | `gfx FLUSH(rect)` | `compose COMMIT` |
| knows | no device, no physical address | no device, no neighbour, not even its own position |

**That is `painter`'s authority in the first column and `window`'s in the second**, and it is the
answer to the question this increment was asked to check: *did the framebuffer contract need
changing to carry text?* No. Neither did the compositor's. Both carry pixels, and a terminal draws
pixels; `display` cannot tell `display_terminal` from the client that painted a test pattern, and `compositor` cannot
tell it from the client that painted a coordinate function. The answer is a spawn literal rather than
an argument.

### One endpoint, because one wait point

A terminal has two classes of sender: an application printing and an input source typing. DECISIONS
§33 recorded that a process here has exactly **one blocking wait point** (one `RECV`, no wait-any,
and two threads cannot share an address space), so telling them apart by endpoint is not available.
They arrive on one endpoint and are told apart by opcode, which is what `line_editor` already does.

The security consequence is stated rather than hidden: an application holding that endpoint could
send `OP_BYTES` and forge a keystroke into **its own** terminal. It gains nothing (the bytes come
back to the grid it is already printing on), and the boundary that matters, one client's input not
reaching another's, is the compositor's and is a capability there.

### The deadlock that shaped the input path

A terminal that answered a keystroke by ringing the compositor's doorbell **deadlocks**, and it takes
two keystrokes in one drain to do it: the compositor is blocked in its `CALL` to the terminal while
the terminal is blocked in its `CALL` to the compositor. That is DECISIONS §33's known cost of
input-as-a-blocking-`CALL`, arriving in practice.

It does not need to ring. The compositor rescans **every** client's control page on every `COMMIT`
from anyone, and the input source rings `COMMIT` itself after it fills the ring. So the frame that
delivers a keystroke is the frame that shows it: the terminal paints, records its damage, bumps its
sequence, and replies. Application output is different, because nobody else is going to ring for it,
so `OP_WRITE` does ring, and that is safe because the caller blocked in `CALL` is the application.

The result is better than the design the deadlock ruled out, which is worth saying plainly: a client
that does not have to ask for a frame after receiving input is one fewer round trip and one fewer way
to stall the compositor.

## Input: the ring is the authority, the doorbell is not

`user/src/kbd.rs` is a confined userspace virtio-input driver. It holds the device, its interrupt,
its own DMA page, the doorbell, and **the input ring's mapping**. It holds no client's endpoint and
cannot name a client.

That split is DECISIONS §33 seen from the producing side:

- **The power to type is the ring's mapping**, which no client has. It is not the doorbell: every
  client holds that, everything sent on it is content-free, and a client that rang it forever could
  not produce one character.
- **The power to decide who receives is the compositor's**, expressed as which of the per-client
  input endpoints *it* holds it uses. The driver cannot influence it. A client receives a keystroke
  because it **holds an input endpoint**, and a client granted none has an empty cspace slot and is
  refused with `NoSuchSlot`, "there is nothing there".

So focus never becomes ambient: there is no verb that grabs the keyboard, no message that names a
recipient, and no page a client can write that would inject input. The parts that could be forged do
not exist rather than being guarded.

The keyboard rides **PCIe**, and here that is a choice rather than a constraint: both `virt` machines
do offer a `virtio-keyboard-device` on the virtio-mmio bus. It rides PCIe so it lands in the same
IOMMU domain the GPU does. A keyboard is the device whose DMA you would least like unconfined,
because its buffers are where every keystroke lands.

The scancode-to-byte mapping is `video_terminal::keymap`: a US layout's main block, shifted and unshifted, as a
flat table plus one bit of state (shift is *held*, so it has to be remembered between events).
Host-tested, because a keyboard layout is data and a wrong row is exactly what a table test catches.
Two rules there earn their tests: a **release types nothing** (the first bug every evdev driver has
is every character arriving twice), and **Enter sends CR, not LF**.

## How text on a screen is proved

This is the part that took the most care, because text is where "it looked right" is most tempting
and least sufficient.

**The picture is a value three parties compute without talking to each other.** The script is
`video_terminal::script`, a constant in the contract crate, the same move `gfx_proto::pixel` and `compositor::SCENE`
make:

1. **The terminal** runs the engine over the bytes it was sent and paints what it says;
2. **the kernel** runs the same engine over the same script and compares the framebuffer pixel for
   pixel through the direct map. It never asks the terminal anything;
3. **the host** runs it a third time and compares QEMU's `screendump` against the same definition.

The third is not decoration. `-display none` means nothing in the guest can see the device's own
surface, so a wrong pixel format or a wrong scanout rectangle would satisfy the first two. And a
wrong format turns a *test pattern* into an odd-looking test pattern; it turns *text* into something
nobody can read.

**The host checker has its own negative control** (`cargo test -p xtask`), and its failure modes are
the terminal's rather than the driver's or the compositor's. It must reject:

- **the same screen with one letter changed** (`glyphs_ok` against `glyphs_0k`, an `o` for a zero,
  the closest pair of glyphs in the font and therefore the hardest case). This is the assertion that
  makes the whole thing mean something: a checker that could not tell those apart would report
  "readable text reached the scanout" for a terminal that drew the wrong text;
- **the typed input missing**, which is a screen that is correct as far as it goes;
- **every rendition ignored**, which is every glyph in the right cell in the wrong colour, the
  picture a terminal that swallowed SGR as an unknown sequence would draw;
- a blank terminal, and the other two pictures on the same scanout.

The script is chosen so a lucky pass is hard: four rows (a one-row picture hides a stride error),
three renditions, a `\r\n` pair (what `line_editor::expand_output` puts on the wire for a Unix `\n`),
and descenders plus an underscore (the glyph rows a font table truncated to seven would lose).

### Ordering, and what breaks it

Three pictures now reach one scanout over one boot, and `cargo xtask` looks for them **in order**:
the composed screen, then the terminal's text, then rung one's pattern, which stays up until QEMU
exits. Tests sort by name, so the order is arranged by naming:
`a_backing_outside_the_grant_is_refused_by_the_iommu` (which resets the device) sorts before
`a_bitmap_font_and_a_vt_engine_put_readable_text_on_the_scanout`, which sorts before
`a_confined_userspace_driver_puts_a_known_pattern_in_a_framebuffer`. A reordering does not corrupt
anything; no dump matches and the scanout check fails loudly.

### The one place the host presses a key

Nothing in the guest can press a key, so the **host** does: `cargo xtask` sends `sendkey` on the same
monitor connection the scanout check already holds open, every poll, from the start of the run. That
needs no synchronization, because QEMU drops key events until a driver sets `DRIVER_OK`.
`video_terminal::script::HOST_KEY` is the single definition of which key, so the side that presses and the side
that asserts cannot drift.

The keyboard test proves the path from a **physical key event to a terminal byte**; the compositor
test proves the path from the **ring to a focused terminal's pixels**. The seam between them is the
ring, which is exactly where §33 put the authority boundary. Naming the seam is better than one test
that hides it.

### And in the compositor, the routing is visible in the picture

`focus_routes_a_keystroke_to_one_terminals_grid_and_not_its_neighbours` puts two display terminals
side by side, types `A` at the focused one, presses TAB, and types `B` at the next. The kernel then
compares every pixel of the composed screen against the two engines it ran itself. A keystroke
delivered to the wrong client is a wrong picture, not a missed assertion, and the test also checks
that the two terminals' contents *differ*, so it cannot pass by the two scripts having become the
same text.

## Honest limits

Stated plainly, because a demonstrator's caveats are part of the deliverable.

- **No scrollback.** A live grid only. The roadmap named scrollback in this milestone and it is not
  here: it wants a ring of off-screen rows and a viewport, which is real work and changes the damage
  model. Recorded, not half-built.
- **No UTF-8.** The grid holds bytes and the font covers basic latin, so a decoder above it would
  have nothing to draw for most of what it decoded. When there is a font with the coverage to justify
  one, the decoder goes in the VT engine and `bitfont::glyph`'s signature becomes `char`.
- **No line editing in the display terminal.** It renders a stream and echoes keystrokes; it does not
  serve `OP_READLINE`. A client that wants edited lines puts `line_editor` in front of it and prints the
  discipline's echo through `OP_WRITE`, which needs no new protocol at all, because `line_editor`'s echo
  is exactly a byte stream this engine parses. That is not a hope: the `video_terminal` crate proves it on the
  host by running both.
- **A 16x8 grid.** The scanout is 128x64 and the font is 8x8. That is what the display ladder's
  current screen affords; the engine's maximum is 32x16 and both are constants.
- **No reflow on resize**, because nothing resizes. The roadmap named reflow; a fixed scene has
  nothing to reflow to.
- **The keymap is a US layout's main block.** No keypad, no function keys, no arrow keys, no compose,
  no dead keys, no other layout.
- **No bell**, visual or otherwise. `BEL` is consumed.
- **No mouse.** `virtio-tablet-pci` presents the same PCI device id as the keyboard, which is
  recorded in `crates/pci` so that a machine carrying both would be a known problem rather than a
  surprise. We attach only a keyboard.
- **No key repeat of our own.** The device's repeats are honoured; nothing here generates them.
- **The hand-drawn candidate is competent, not good.** `bench/font-options/hand-drawn-8x8.art` is
  consistent (the tightest left sidebearing in the survey) and light, and three glyphs are weak
  enough to name where a reader meets them: `$` is mushy where the stem crosses the S, `&` reads as
  a blob, and the shoulder of `r` sits a pixel clear of its stem so the arm looks detached. It is a
  drawn candidate for comparison, not a proposal, and nothing in the tree uses it.
- **The specimen tool clips wide glyphs rather than refusing them.** Both the `.hex` and `.bdf`
  readers keep the leftmost byte of a row, so a 16-pixel-wide glyph is shown as its left half, which
  looks like a clipped font instead of an error. The `.hex` height is taken as the commonest row
  count among the letters, because `unscii-8.hex` stores `U+0000` with sixteen rows in an eight-row
  font and the maximum is therefore a lie. The `.bdf` reader uses the bitmap and the bounding boxes
  only: `SWIDTH`, `DWIDTH` and the property block are ignored, which is right for a fixed-pitch cell
  and wrong for anything else. Half-block output is only faithful in a terminal that draws
  `U+2580`/`U+2584` at full cell height; `--dots` has no such dependency and is the tie-breaker.
- **A font narrower than the cell is drawn at its own advance, and that is a choice worth knowing.**
  Spleen 5x8 and Terminus 6x12 are narrower than eight pixels, and drawing them on an eight-pixel
  pitch makes them look loose in a way that is the tool's fault rather than the font's. The tool
  takes the advance from the font's own bounding box. What it does **not** do is prove that
  `crates/bitfont` would work at that width: `GLYPH_W` is a constant three parties read, and moving
  it is a change to the crate rather than to a table.
- **The 8x16 authoring option is priced but not drawn.** There is a hand-drawn 8x8 to look at and no
  hand-drawn 8x16, so the "author our own at twice the height" row in the survey is an estimate
  where the 8x8 row is a specimen.

## What adopting libghostty-vt would cost now

The roadmap names **libghostty-vt** (Ghostty's extracted VT core: zero-dependency, no libc, no
allocations, a C ABI, written in Zig) as the strongest form of milestone 23's claim, and milestone 36
built the C seam (DECISIONS §31, [c-seam.md](c-seam.md)) specifically to de-risk it. The Rust engine
above is built, so the comparison can be made on facts instead of estimates. **This is a
recommendation, not a decision.**

**What it would buy.** A vendor component in a language we do not use, capability-confined and
hot-swappable, is the thesis in its strongest available form: the more unverified the component, the
more the confinement has to prove. And a real VT engine is *much* more complete than ours: scrollback,
reflow on resize, UTF-8 and grapheme clustering, the DEC modes, mouse reporting, and a conformance
history against `vttest` that we would otherwise be writing from scratch for years.

**What it would cost, concretely, now that the seam and the Rust engine both exist.**

1. **A Zig toolchain in the build**, for one component, pinned. Milestone 36 already accepted a
   `clang` in the build for C and priced that; Zig is a second one, and it is the cost that does not
   go away.
2. **The seam is proved but the shape is not free.** §31's C seam holds *no capabilities and makes no
   syscalls*: the Rust shim holds everything and passes buffers. A VT engine fits that shape almost
   perfectly (bytes in, grid out, no IO), which is the good news, and it is not an accident: it is
   the same sans-IO property `crates/video_terminal` has. So the port is a shim that feeds bytes and reads cells,
   not a rewrite of `display_terminal`.
3. **The grid readback is the actual work.** Our engine gives `pixel(x, y)` as a pure function, which
   is what makes the three-witness proof possible. libghostty-vt's C ABI gives cells; the shim would
   have to walk them and the *expected-picture* definition would have to move to the Zig side or be
   reimplemented against its cell layout. **The proof structure, not the rendering, is what would
   have to be rebuilt.** That is the cost this increment discovered and could not have known before.
4. **Their API is in flux**, so any adoption pins a version and takes the divergence-management
   discipline the vendored RedoxFS already has (DECISIONS §18's vendoring policy).
5. **`crates/video_terminal` would not be deleted.** It is about 1,750 lines including its tests and its keymap (1,500 when this was written; it is a hedged magnitude, re-measured at each documentation sweep rather than gated, because a line count moves on every test anyone adds),
   and it is the thing that makes the host-side scanout check possible; keeping it as the reference
   implementation the foreign one is *checked against* is more valuable than either alone, and it is
   a better milestone-23 demonstration too (swap the engine, run the same suite, compare the grids).

**The recommendation.** Adopt it as a *second* engine behind the same seam, not as a replacement, and
do it when there is a reason to want scrollback and UTF-8 rather than to want a Zig dependency. The
milestone-23 claim is strongest when the two engines can be swapped under a suite that grades both,
and that is only possible because the Rust one exists. If the answer is "not yet", nothing is lost:
`display_terminal` is a component behind an endpoint, so swapping it later is a component change, which is the
property this increment was asked to keep and did.

## Where the pieces are

| piece | file |
|---|---|
| the font and its provenance | `crates/bitfont/src/lib.rs`, `crates/bitfont/src/glyphs.rs` |
| the VT engine | `crates/video_terminal/src/lib.rs` |
| the keymap | `crates/video_terminal/src/keymap.rs` |
| the test script, shared by three witnesses | `crates/video_terminal/src/script.rs` |
| the terminal component | `user/src/display_terminal.rs` |
| the keyboard driver | `user/src/kbd.rs` |
| enumeration | `kernel/src/pci.rs` (`find_input_device`) |
| the wiring | `kernel/src/user/display_service.rs` (`start_terminal`), `kernel/src/user/compositor_service.rs` (`spawn_terminal`), `kernel/src/user/keyboard_service.rs` |
| the tests | `kernel/src/user/display_tests.rs`, `kernel/src/user/compositor_tests.rs` |
| the host-side text check and its negative control | `xtask/src/main.rs` |
| the device lines | `scripts/qemu-runner-aarch64.sh`, `scripts/qemu-runner-riscv64.sh` (`NIFE_KBD`) |

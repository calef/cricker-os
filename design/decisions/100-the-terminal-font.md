# 100. The terminal font is gohufont-14

**Status: DECIDED.** calef, 2026-08-19, after looking at fifteen candidates rendered on the same
sample text: *"gohufont-14 it is."*

**What it replaces.** `font8x8`, which has shipped since milestone 29 and was chosen for its licence
rather than its looks.

## Why this one

**It was picked by looking, which is the only way to pick a font.** Milestone 29's crate is a pure
function from `(character, x, y)` to ink, so a specimen sheet is a thing the tree can print, and
`cargo run -p bitfont --example specimen` rendered every candidate on identical lines. The choice was
made from pictures rather than from a table of byte counts, and the same harness is what tested every
claim below.

**On the letterforms it beats what ships, and by a wide margin.** Nine rows of cap against
`font8x8`'s seven, seven of x-height against five, and a **three-row descender against one**, so
`g p q y j` get a tail rather than a hook. `font8x8` also carries display-ROM serifs on `I`, `l`, `t`
and `F` and a flared foot on `A`, which is noise at a size with no room for it, and its glyph widths
wander more than anything else measured (width sigma 0.79 against gohufont's 0.74, and its prose sets
clotted where gohufont's does not).

**And it carries no obligation at all.** WTFPL v2 is one operative term, read at `wtfpl.net`:

> 0. You just DO WHAT THE FUCK YOU WANT TO.

No notice to reproduce, no reserved font name, no share-alike, no cure period. That matters here
beyond convenience: **a bitmap font is compiled into the kernel image and into every binary that
draws text**, so its licence is a licence on the artefact rather than on a build-time tool.

## What was refused, and why each lost

- **`unscii-16`** (public domain) is the best-looking font in the survey and was calef's first pick.
  It is 8x16, so it gives **16x4** on the current scanout, exactly what gohufont-14 costs, and
  gohufont was preferred once both were seen at the same size.
- **`unscii-8`** (public domain) is the best 8x8 and keeps all eight rows. Refused on looks alone,
  which is the whole basis of this decision.
- **Terminus** (OFL-1.1) reserves the name **"Terminus Font"**, and the OFL forbids a Modified
  Version using a reserved name. Being picky about fonts means eventually fixing a glyph, so that
  clause is a live cost for us rather than a formality, and the OFL has no cure period: it
  *"becomes null and void"* if a condition is missed.
- **Spleen 5x8** (BSD-2-Clause, no reserved name) is the one candidate that gives **more** screen
  than today, 25x8 against 16x8. It remains the strongest argument against this decision and the
  block below says so rather than burying it.
- **The Kaypro II character ROM** was found, verified bit-identical to MAME's `kayproii` chargen
  region, rendered, and **excluded on provenance**: the dump states no licence at all. It also lost
  on looks. See notes/glyphs.md.
- **A hand-drawn font** was drawn, 95 glyphs, and judged *"not worse than what ships, and not as
  good as Terminus or unscii-8."* Kept in the tree as a specimen, not as a candidate.
- **Linux's `lib/fonts/font_8x16.c`** is `GPL-2.0` on line one. **VileR's Oldschool PC Font Pack** is
  CC BY-SA, which is share-alike. **Fixedsys Excelsior**'s public-domain claim could not be read at
  its source. All excluded, ambiguity included, which is this tree's standing rule.

## What this costs, stated plainly

**gohufont-14 is 8x14, and the scanout is 128x64, so the text grid becomes 16x4.** Four rows. That is
not a terminal, and notes/glyphs.md says so in its own words.

**So this decision is about which font, and it does not by itself change what a user sees.** Shipping
it at four rows would make the terminal worse in exchange for better letters. The scanout is what has
to move, and that is a display-ladder question rather than a font one: 128x64 was chosen so that a
stride bug, a transposition and an x/y swap are all size or content mismatches rather than invisible
(`gfx_proto::WIDTH`), and the only hard floor named is QEMU's 16 pixels per side. Nothing forces it
to stay small; the cost of growing it is in the pixel-for-pixel test harness rather than the driver.

## The obligations, and where they land

There are none to satisfy. Recorded anyway, because a reader will reasonably ask:

- **Nothing need travel with the image.** No notice, no licence text, no attribution.
- **`vendor/README.md`** gets an entry regardless, because the tree registers the provenance of
  everything it did not write, and the register is worth more than the obligation would have been.
- **`deny.toml`** is unaffected: `script/supply-chain` reads the cargo graph, and a font transcribed
  into a Rust table is not in it. That gap is real and belongs on the register rather than the gate.

**One thing worth deciding with open eyes**: the licence's name is unusual, and if licence text is
ever put in milestone 40's documentation store, `doc licenses/gohufont` would print it at the nife
prompt. That is a taste question rather than a legal one, and it is calef's.

## BUGS

- **The comparison is against fifteen candidates, not against every bitmap font.** The survey
  excluded proprietary faces without rendering them, and the best-drawn fonts of the 1980s are
  proprietary: the original Macintosh bitmaps, Monaco among them, were drawn by a designer Apple
  paid, which is exactly why they are not available.
- **`gohufont-14`'s WTFPL text carries no warranty disclaimer**, where BSD-2 and OFL both do. No
  candidate's disclaimer was ever going to matter for a font, and the difference is real and is
  recorded rather than smoothed over.
- **The measurements in notes/glyphs.md are this tree's own**, taken by its own harness rather than
  by a typographic tool. They are good enough to rank candidates and should not be quoted as
  properties of the fonts themselves.

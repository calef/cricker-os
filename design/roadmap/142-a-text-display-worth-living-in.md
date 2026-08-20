# 142. A text display good enough that people use it instead of a GUI

**Status: NOT-STARTED.** Minted 2026-08-20 by calef, on seeing the Kaypro-style font land:
*"I would like to explore a really awesome text display and not a grainy one like we get with
Kaypro II... the idea would be to deliver a text display so good that people would use it outside
of a GUI."*

**Gate: MILESTONE 141, DECISION.** Milestone 141 owns the palette property check, and a palette
cannot be chosen honestly before the check that says which palettes are admissible exists. The
decision is the font and the dependency that renders it, below. **Increments one and two need
neither** and a lane could start them today; they are the larger half of the deliverable and none
of it is aesthetic.

**In brief.** The terminal is 18 columns by 8 rows of a hand-drawn 7x8 bitmap on a 128x64 screen.
The ask is a display somebody would choose over a window manager. That is four independent axes,
of which the font is the one everybody names and the smallest one: **the surface, the terminal,
the type, and the colour.** This block sequences them, prices the type properly, and reports one
finding that makes the expensive axis cheap.

## Where this sits, and it is not a rung

[The display ladder](../display-ladder.md) climbs toward compositing and acceleration: rung three
is real applications on iced's software renderer with `cosmic-text`, rung four is a GPU. **This
milestone points the other way.** The ladder's destination is a system good enough to run a GUI;
calef's ask is a system good enough that you would not open one. Those are different claims and
the second one is the more interesting for a microkernel, because a terminal is a component the
whole confinement story already covers and a desktop is not.

So this is a **sibling of rung three that rung three then consumes**, not a step on the ladder.
Rung three would deliver good type as a side effect of adopting a UI toolkit, which is the
expensive way to buy it: `cosmic-text` exists to do shaping, bidirectional text, font fallback and
proportional layout, and **a monospace terminal needs none of those.** Take the type half on its
own terms first, and rung three's remaining work is the toolkit rather than the toolkit plus the
text.

## What "would use it outside a GUI" actually requires

calef named colour, type and rich text. The axis he did not name is the one that decides whether
anybody would actually live in it, and it is the axis that needs no rasteriser at all.

**Today's terminal is 18 columns by 8 rows, with no scrollback, no UTF-8, no reflow, no arrow
keys, no mouse and no bell** (notes/glyphs.md records all of these as honest limits). A person
choosing a text display over a window manager needs 80 columns by 24 rows as an absolute floor,
scrollback, UTF-8, and arrow keys. **None of that is a font problem**, all of it is buildable
today against the bitmap font that is landing, and it is more than half the work.

That reordering is the block's main recommendation: **make it a terminal first, then make it
beautiful.** A gorgeous 18x8 window is a demo. A plain 146x51 window with scrollback is a place
you can work, and the beauty then lands on something worth looking at.

## The scanout

**128x64 was chosen as a test instrument** and says so: `gfx_proto::WIDTH` records that a square
surface hides a stride bug, a transposition and an x/y swap, and that QEMU refuses a scanout
smaller than 16 a side. **That reasoning survives growth unchanged**, because it constrains the
*ratio* rather than the size, and the size it wants now falls out of the measured cell rather than
out of taste.

**The recommendation is 1280x720**, and it is arithmetic:

| Surface | Frames | Grid at the 2x cell (14x26) | Grid at the 1x cell (7x13) |
|---|---|---|---|
| 128x64 today | 8 | 9 x 2 | 18 x 4 |
| 1024x768 | 768 | 73 x 29 | 146 x 59 |
| **1280x720** | **900** | **91 x 27** | **182 x 55** |

1024x768 is the obvious number and it **misses 80 columns** at the 2x cell, which is the one
requirement a terminal has. 1280x720 clears it with room, is 16:9 and so is decidedly not square,
and satisfies both `const` assertions `gfx_proto` already carries: it is far above 16 a side, and
1280x720x4 is 3,686,400 bytes, exactly 900 frames with nothing left over.

**§102 is what makes this reachable, it is decided, and nobody is building it.** A `Frame` naming
a run turns 900 capabilities and 900 `MAP` calls into one of each, and without it the sixteen-slot
cspace refuses the surface outright. `Object::Frame` is still `Frame(u64)` in
`crates/capability/src/lib.rs`, one page and no count.

That is worth flagging rather than assuming, because **the milestone that motivated §102 no longer
needs it.** §102 was raised to unblock milestone 29's font increment at 800x608 for gohufont-14 at
8x14; §100 was then amended and the font that is landing is a 7x8 drawing that gives 18x8 on the
scanout we already have. So the decision was made, the pressure that produced it went away, and
**this milestone is now §102's first consumer.** Increment one is where it gets built.

Two costs that are nobody's decision and that a lane will meet:

- **The wire is fine.** `gfx_proto::rect` packs each of `x`, `y`, `w`, `h` into 14 bits, so the
  ceiling is 16,383 a side. 1280x720 does not come close, and no format change is owed.
- **The harness gets 112 times more expensive, and it is polled.** `cargo xtask`'s scanout referee
  takes a `screendump` **every 100 ms** for the whole suite and compares every pixel. At 128x64
  that PPM is about 24 KiB; at 1280x720 it is about 2.6 MiB, ten times a second, per ISA, and the
  comparison is 921,600 pixels against 8,192. This is the real price of a big screen in this tree,
  and it belongs in the increment that grows the surface rather than in a later surprise. It is
  also the one number here worth attacking: nothing forces the referee to poll at 100 ms once a
  dump costs a hundred times more.

## The type: what would actually ship, because it will not be Menlo

**Menlo is Apple's and cannot ship here.** It is bundled with macOS and is not redistributable;
this project has already excluded three fonts on exactly this ground (the Kaypro II ROM for
stating no licence at all, Linux's `font_8x16.c` for GPL, Fixedsys Excelsior for a public-domain
claim that could not be read at its source), and an Apple system font is a clearer exclusion than
any of them.

**And its ancestry is not folklore, it is in the file.** `/System/Library/Fonts/Menlo.ttc` on this
machine, read with `fontTools`, carries its own provenance in its `name` table:

| Field | Value |
|---|---|
| Copyright (id 0) | `Copyright (c) 2009 Apple Inc. Copyright (c) 2006 by Tavmjong Bah. Copyright (c) 2003 by Bitstream, Inc. All Rights Reserved.` |
| Trademark (id 7) | `Menlo is a Trademark of Apple Inc.` |
| Manufacturer (id 8) | `Bitstream` |
| Designer (id 9) | `Jim Lyles` |
| Vendor URL (id 11) | `http://www.gnome.org/contact/` |

Every part of the claim is confirmed by Apple's own metadata, and it is more specific than the
folklore. **Jim Lyles designed Bitstream Vera**; **Tavmjong Bah is the DejaVu project**; the vendor
URL still points at GNOME, which is where Vera was released. Apple's contribution is a 2009 layer on
top, and the trademark and the *All Rights Reserved* are Apple's claim over **their file**, not over
the shape of the letters.

**So the practical finding is a good one: DejaVu Sans Mono is not a substitute for Menlo, it is
Menlo's immediate ancestor.** Choosing it is not settling for a lookalike. It is taking the same
outlines from the last point in the chain where they were given away.

**The metrics are measured too**, from the same file, and they replace the estimate this block was
first drafted with. Menlo is 2048 units per em; every glyph advance is 1233 units (0.602 em, so it
is genuinely monospaced), `hhea` gives ascender 1901 and descender -483 with zero line gap, cap
height is 1493 and x-height 1120.

| "Menlo Regular 11" at | Cell | Cap height |
|---|---|---|
| 1x (11 px per em) | **6.62 x 12.80 px**, so a 7x13 cell | 8.0 px |
| 2x (22 px per em) | **13.25 x 25.61 px**, so a 14x26 cell | 16.0 px |

**"11 point" is not a number of pixels until a density is fixed**, and that choice is the one that
decides whether this looks like macOS or like a 1990s X terminal. At the 1x cell the cap is eight
pixels tall, which is where unhinted rendering goes soft and where hinting engines were invented.
At the 2x cell it is sixteen, which is where unhinted anti-aliasing has looked good for fifteen
years. **Build the atlas at the 2x cell.**

One more number from the same read: Menlo ships **four faces** (Regular, Bold, Italic, Bold Italic)
and 3,157 glyphs in the regular. We would ship four faces and a few hundred glyphs, which is the
size argument below.

**What would ship instead.** Licences read at their sources rather than recalled, and file sizes
taken by downloading the releases:

| Family | Licence | Reserved name | Faces | Regular `.ttf` |
|---|---|---|---|---|
| **DejaVu Sans Mono** 2.37 | Bitstream Vera for the base, DejaVu's own changes **public domain**, Arev glyphs on a Vera-shaped licence | "Bitstream", "Vera", "Tavmjong Bah" and "Arev" may not appear in a modified name. **"DejaVu" itself is not reserved.** | 4 | 340,712 B |
| **JetBrains Mono** 2.304 | OFL 1.1 | **none declared**, so OFL clause 3's renaming requirement does not bite | 4 and a variable font | 273,900 B |
| **Source Code Pro** 2.042R | OFL 1.1 | **"Source"**, which is broader than it looks | many | 210,312 B `.ttf`, 131,128 B `.otf` |
| Menlo | Apple, All Rights Reserved | not applicable | 4 | not available |

Three things in that table decide more than the letterforms will.

**DejaVu is the closest thing to Menlo that can be shipped, and it is not a lookalike.** It is the
generation Apple built on. Against that: **the project has been dormant since 2016** and has no
variable font, so nobody is fixing a glyph we find wrong. Its licence obliges the Bitstream and Bah
notices to travel with every copy, and forbids selling the font by itself.

**JetBrains Mono has the cleanest obligations of the three.** OFL 1.1 with **no** Reserved Font
Name, which is exactly the clause §100 called the expensive one when it refused Terminus: *"being
picky about fonts means eventually fixing a glyph"*, and a font with no reserved name lets us fix
one and keep the name. It is also the only actively maintained candidate. **Source Code Pro
reserves the word "Source"** and inherits §100's objection whole.

**None of the three is on the supply-chain allow-list, and that is a fact about crates rather than
about fonts.** `deny.toml` allows `MIT`, `Apache-2.0`, `Apache-2.0 WITH LLVM-exception`,
`BSD-3-Clause` and `0BSD`; neither `OFL-1.1` nor the Bitstream Vera licence is there. A font
arriving as a **crate** would therefore stop the build and need a scoped exception the way
`libfuzzer-sys` got one for NCSA. A checked-in coverage table does not, and inherits §100's
recorded gap instead: `script/supply-chain` reads the cargo graph, and a font transcribed into a
Rust table is not in it. **That gap grows with this milestone** and belongs in its own `BUGS` line
rather than being quietly enlarged.

**The recommendation, and it is a fork rather than an answer, because a font is chosen by looking.**
DejaVu Sans Mono if the goal is *"what calef asked for"*, since it is Menlo's own ancestor.
JetBrains Mono if the goal is a font this tree can live with for years. `bitfont`'s specimen
harness already renders any candidate on identical sample text (`cargo run -p bitfont --example
specimen`), which is how §100 was decided and is how this should be.

## Solarized Dark Higher Contrast, against milestone 141's three properties

calef named **Solarized Dark Higher Contrast**, and the first finding is about the name.

**It is not Ethan Schoonover's, and it is not an iTerm2 built-in.** iTerm2's `ColorPresets.plist`
holds eleven presets, "Solarized Dark" among them, and no Higher Contrast variant. The palette
under that name traces to a 2011 GitHub gist by `heisters`, titled *"Solarized High Contrast Dark
theme for iTerm2"*, which reaches most people through `mbadolato/iTerm2-Color-Schemes`; the two
files are byte-for-byte the same palette under two names.

**And it is a different palette, not a contrast adjustment.** All sixteen ANSI values differ from
canonical Solarized Dark, as do the background, foreground, bold and cursor colours. What it
discards is Solarized's structural idea, and that is the difference calef would actually feel:

| ANSI slot | Canonical Solarized Dark | Higher Contrast |
|---|---|---|
| 10 bright green | `#586e75` **base01, a grey** | `#51EF84` a green |
| 11 bright yellow | `#657b83` **base00, a grey** | `#B27E28` a yellow |
| 12 bright blue | `#839496` **base0, a grey** | `#178EC8` a blue |
| 14 bright cyan | `#93a1a1` **base1, a grey** | `#00B39E` a cyan |

Canonical Solarized spends half its ANSI table on a greyscale ramp on purpose, so only eight of
sixteen slots hold a hue. That is a deliberate and unusual choice, and it means every program that
uses a bright colour for emphasis (`ls --color`, a diff, a linter) gets a grey. **The Higher
Contrast variant is a conventional sixteen-colour terminal palette sitting on a Solarized-ish dark
ground**, which is very likely why it exists and why someone would prefer it. Neither is wrong;
they are different things and the record should not call the second one Solarized without saying
so.

**Now milestone 141's three properties, computed over both.** The check is over exactly the right
sixteen values in both cases, because Solarized's published ANSI mapping uses each palette entry
exactly once.

| Property | Canonical Solarized Dark | Higher Contrast | Today's palette |
|---|---|---|---|
| 1. every entry has three distinct channel values | **fails on one entry**: ANSI 14 `#93a1a1` (`a1` twice) | **fails on one entry**: ANSI 2 `#6CBE6C` (`6C` twice) | fails on **all sixteen** |
| 2. no two entries are channel permutations | **passes** | **passes** | fails |
| 3. no two entries saturate `0xff` in the same channel | **passes**, and no entry has any channel at `0xff` at all | **passes**, same | fails on all three channels |

**So the answer calef needs before he picks is: both pass, each after a one-unit nudge to a single
channel of a single colour.** `#93a1a1` becomes `#93a1a0`; `#6CBE6C` becomes `#6CBE6B`. Neither is
perceptible, both are inside the noise of the display, and each is a one-character edit.

Two things worth saying beyond the verdict. **Both candidates are strictly better test instruments
than what ships**, which fails all three properties, and both are better on property 3 in a way 141
did not anticipate: neither has a single channel at `0xff` anywhere, where the current palette has
twelve. And **milestone 141's gate does not choose between them.** It says both are admissible. The
choice is taste and the greyscale-ramp question above, and it is calef's.

## Rich text: what `Attr` would have to grow

`video_terminal::Attr` is **one byte**: four bits of foreground index, three bits of background
index, one bit of reverse. `Cell` is that byte plus a character byte, so a cell is two bytes and
the grid is a fixed array. That is the whole of "rich" today.

What a display somebody would live in carries, in rough order of how much people notice:

| Attribute | Cost | Note |
|---|---|---|
| 24-bit foreground and background | 48 bits a cell | SGR 38;2 and 48;2. This is what every syntax highlighter emits and the single largest gap |
| UTF-8, and a `char` rather than a byte | 32 bits a cell | notes/glyphs.md already names the decoder's home: the VT engine, with `bitfont::glyph`'s signature becoming `char` |
| Real bold and italic | a second and third face | see below, this reverses a recorded decision |
| Underline, and its styles | 3 bits | SGR 4:1..4:5, and a separate underline colour if we are being honest about "rich" |
| Strikethrough, dim, blink, invisible, overline | 5 bits | cheap, and expected |
| Hyperlinks (OSC 8) | a side table | the one modern terminals added that people actually use |
| Double-width cells | 1 bit and a layout rule | falls out of UTF-8 the moment CJK arrives |

A cell goes from 2 bytes to roughly 16. On a 91x27 grid that is 39 KB against 5 KB, which the heap
holds without argument; scrollback is what makes it interesting, since a thousand lines of history
at 91 columns is about 1.5 MB, and `MAX_COLS` and `MAX_ROWS` are 32 and 16 today with the grid a
fixed array in `.bss`. Somewhere between here and there the grid stops being a `static` and starts
being an allocation, and that is the change increment two actually is.

**One recorded decision reverses, and it should be reversed on purpose rather than by accident.**
`crates/video_terminal` says *"bold is bright"*, because a bold weight needs a second font and at
8x8 a bold face is a smudge. **At an anti-aliased 11-point cell both halves of that reason
expire**: a bold face is legible, and the atlas has room for it. So "rich" means shipping four
faces rather than one, which is a licence question four times over and a table four times larger.
That is the hidden cost in calef's word *rich*, and it is worth him seeing before he picks a
family, because a family with no italic is disqualified by this and by nothing else.

## How the verification survives, which it must

Three parties compute today's picture without talking to each other: the terminal draws it, the
kernel predicts the framebuffer pixel for pixel through the direct map, and the host grades QEMU's
`screendump`. The host checker's negative control is a screen with **one letter changed**, an `o`
for a zero. That structure is the reason text on this screen is proved rather than plausible, and
losing it would be a serious regression.

**The atlas keeps it exactly, and that is the strongest argument for the atlas.** A coverage table
is a `static` and a lookup, so `(character, face, x, y) -> coverage` is as pure as
`bitfont::glyph` is, compiles for host and kernel identically, and all three parties keep running
the same function. Nothing about the check's shape changes. What changes is its cost, which is the
screendump arithmetic above and not a question of principle.

**A runtime rasteriser would not keep it, and this is the honest half.** It is still a pure
function, so the check survives in principle; in practice the kernel-side witness would have to
link an outline parser and a rasteriser into the *test* kernel image, which is a font engine
inside the thing whose smallness is the thesis. Test-only or not, that is a bad trade for a
property we can have for free.

**The subpixel-drift worry is real and lands in a better place.** Two versions of a rasteriser can
disagree by a coverage value or two, and if the rasteriser ran at runtime that disagreement would
be a mysterious three-party mismatch. With a build-time atlas it is a **reproducibility question
about a checked-in artefact**: pin the generator, check in the table, and gate that regenerating
produces the same bytes. That is the shape `script/vendor-verify` already has for RedoxFS
("upstream plus our recorded patches") and the shape `crates/bitfont/src/glyphs.rs` already has
for its transcription, which was done by a script with no bits changed.

### The determinism measurement, which decides the crate

The worry above is not hypothetical, and it was measured rather than argued, twice, by two
investigations that did not see each other's work. **A peer session measured cross-architecture
output stability on 2026-08-20**, at raw `f32` coverage rather than at
the quantised byte, over 95 ASCII characters at nine sizes (fractional ones included) across four
fonts, on x86_64 and aarch64 with both `std` floats and `libm`:

- **`ab_glyph_rasterizer` is byte-identical** across every combination.
- **`fontdue` 0.9.4 is not**, at default features. Its `simd` feature is **on by default** and
  compiles an SSE path on x86 only, and the four-wide prefix sum reorders float additions against
  the scalar accumulation. One pixel in 151,414 differed on JetBrains Mono; three in 139,454 on
  Source Code Pro. The delta was always exactly 1/255, and dimensions and metrics always matched.

A second, independent investigation reproduced this at larger scale and reached the same numbers
by a different route: 58,708 glyph renderings across fifteen fonts, hashed on raw `f32` bits,
identical between aarch64 and x86_64 for `ab_glyph`; and for `fontdue`, 136 of 47,166 renderings
differing (0.288%), **every failure exactly one pixel by exactly one 255th**, which is the same
signature. It also pinned the mechanism: in the four-wide prefix sum, lane 3 computes
`(a3+a2) + (a1+a0)` where the scalar path computes `((a0+a1)+a2)+a3`, and float addition is
commutative but not associative.

**The reason the portability holds is worth knowing, because it is a property of the compiler
rather than of the crate.** Rust does not enable floating-point contraction, so `x + dxdy * dy`
cannot silently become a fused multiply-add on one architecture and not the other. That was checked
in generated assembly rather than assumed: zero `fmadd` or `fmla` on aarch64 at `-C opt-level=3`
(35 `fmul` and 32 `fadd` kept separate), and zero `vfmadd` on x86_64 even under
`-C target-feature=+avx2,+fma`.

### Cross-version, and the one instruction this section produces

The cross-version question is the one a checked-in atlas actually depends on, and the first answer
recorded here was too optimistic. **A patch bump did change raw output.**
`ab_glyph_rasterizer` 0.1.5's changelog says it, verbatim:

> Remove cap of `1.0` for coverage values returned by `for_each_pixel`, now `>= 1.0` means fully
> covered. This allows a minor reduction in operations / performance boost.

Measured across 0.1.1, 0.1.4, 0.1.5 and 0.1.10, the raw `f32` hash changes at 0.1.5 and the **8-bit
bitmap hash does not**, because Rust's float-to-integer `as` cast saturates and folds the
now-uncapped values back onto 255.

**So the instruction the atlas takes from this is one line: quantise to `u8` at the boundary and
never persist raw `f32`.** Doing that is what makes the version-to-version risk mostly disappear,
and it costs nothing, because 8-bit coverage is what the table holds anyway. It is the difference
between a table that survived a patch bump by luck and one that survives it by construction.

With that in place, the measured picture is: `ab_glyph` 0.2.15 through 0.2.32 and
`ab_glyph_rasterizer` 0.1.5 through 0.1.10 bit-identical, `fontdue`'s scalar path byte-identical
0.6.4 through 0.9.4. **Neither project promises any of it**, and the `BUGS` section says so.

**And pin 0.1.10 specifically.** `ab_glyph_rasterizer` 0.1.4 through 0.1.8 panic with an index out
of bounds on some in-bounds-adjacent geometry, which a randomised harness hit immediately. Fixed in
0.1.9.

**One more trap, in the crate this block does not recommend, recorded so nobody rediscovers it.**
`fontdue`'s `FontSettings::scale` (default 40.0) feeds the curve linearisation tolerance at font
*load* time, so the same glyph at the same pixel size renders differently depending on a number set
somewhere else entirely: `'a'` at 32 px gives byte sums of 32,613, 32,682 and 32,643 at scale 40,
100 and 12. A picture that depends on a load-time setting is precisely the shape this tree's
three-party check exists to refuse.

**One pixel in 150,000 is a catastrophe for this tree specifically, and that is worth stating
plainly** rather than filed as a curiosity. The kernel's witness and the host's checker compare
**every** pixel and a single mismatch is a failed test, so a rasteriser that disagrees between
aarch64 and riscv64 breaks the three-party agreement outright. **Pinning the version is not
enough** for `fontdue`; it would need `default-features = false` forced at every call site, which
is a rung-four mechanism (a note somebody has to remember) guarding the property the whole check
rests on.

So the choice of crate is decided by determinism rather than by dependency count, and the two
agree: **`ttf-parser` plus `ab_glyph_rasterizer`**, which is also the smallest graph on offer and
the one already proven to build on both ISAs. A
build-time atlas softens this further, since the divergence would then be between the machine that
regenerated the table and the machine that checked it rather than between two target
architectures, but softening it is not a reason to choose the crate that has it.

**Two rendering choices are verification choices as well as aesthetic ones**, and both should be
made deliberately:

- **Grayscale anti-aliasing, not subpixel (ClearType-style) anti-aliasing.** Subpixel rendering
  triples the horizontal resolution by lighting a panel's red, green and blue stripes
  independently, so it depends on that panel's subpixel order, which a framebuffer does not know
  and QEMU's `screendump` cannot represent. It also produces coloured fringes that a pixel-exact
  check would have to model. Grayscale keeps the picture a property of the pixels rather than of
  somebody's monitor, and it is where Apple ended up as well.
- **Blend in linear light, not in sRGB.** This is the difference between "anti-aliased" and
  "good", and it is the thing nobody mentions: lerping coverage in sRGB space makes light-on-dark
  text look bolder and dark-on-light text look thinner than the outline says. Solarized Dark is
  light text on a dark ground, which is exactly the case that goes wrong. The fix is a 256-entry
  sRGB-to-linear table and a reverse table, all integer, no float at runtime, and it is perhaps
  fifty lines.

**And one property that anti-aliasing quietly retires**, which milestone 141 could not have known
because it was written for a bitmap terminal. 141's palette check exists so that a corrupted pixel
is a detectably wrong colour rather than a different legal one. **With anti-aliasing there is no
such thing as an off-palette pixel**: every edge pixel is already a blend, so the legal set is not
sixteen values, it is every value between each pair. The properties do not become wrong, they
become decorative for this terminal, and the pixel-exact comparison (which was always the stronger
check) carries the whole load. 141 should still land, because the bitmap terminal is what ships in
between and because the palette it admits is the palette this one inherits.

## The dependency, which is §46's question and has a clean answer

§46's test runs in order, and the atlas makes the answer unusually clean because it splits the
work in two.

**1. Is it on the verification path?** *The runtime is; the generator is not, and that is the whole
point.* What gets verified is the **table**, by regenerating it and comparing bytes, the same shape
`script/vendor-verify` uses for RedoxFS. The code that produced the table is not in the kernel, not
in the terminal, and not in any proof harness, so §46's strongest reason to write a thing (you
cannot restructure someone else's crate to make a model checker tractable) does not apply to it at
all. A runtime rasteriser would land squarely on the verification path and this answer would
reverse.

**2 and 4. Is the spec the whole of correctness?** Here the two halves separate, and they separate
in opposite directions:

- **A TrueType and OpenType outline parser is a whole subsystem we would never write.** The format
  is decades deep in vendor quirks, and correctness is won by having been fed thousands of real
  fonts rather than by reading the specification. That is rule 4's crypto shape exactly: **take
  it.**
- **A scanline coverage rasteriser is not.** Filling a closed path of quadratic and cubic beziers
  and reporting per-pixel coverage is a few hundred lines of published, fully specified geometry,
  with an obvious oracle (compare against a supersampled reference) and no adversary. That is the
  in-between this tree writes.

**So the shape §46 recommends is: take `ttf-parser` for the outlines, write the coverage
rasteriser, and run both on the host.** That is "thin primitives or whole subsystems, nothing in
between" applied honestly rather than as a slogan, and the tree gets a rasteriser it can reason
about attached to a parser it never has to.

**Two mechanical facts a lane will hit, both worth knowing before the decision rather than after.**

- **A build-time dependency is not outside the supply-chain gate.** One `deny.toml` is applied to
  every workspace in the tree, and its own header gives the reason: *"a licence we would refuse in
  the kernel is not acceptable in the host tooling either."* So a generator in `xtask` or `tools/`
  is checked by the same advisory and licence policy as anything that boots. What it does buy is that it is not in the
  shipped artefact, which is a real reduction in exposure and not a reduction in scrutiny.
- **The licence allow-list is five entries** (`MIT`, `Apache-2.0`, `Apache-2.0 WITH
  LLVM-exception`, `BSD-3-Clause`, `0BSD`) plus one scoped exception for `libfuzzer-sys`, and it is
  an allow-list on purpose, *"because the failure we care about is a licence nobody looked at"*. If
  a chosen font ever arrives as a crate rather than as a checked-in file, and its licence is the
  OFL, it stops the build and needs an explicit scoped exception. A checked-in table sidesteps that
  and inherits §100's recorded gap instead: `script/supply-chain` reads the cargo graph, and a font
  transcribed into a Rust table is not in it.

**The dependency numbers, measured on this machine by a peer session on 2026-08-20** with
`cargo tree` and `cargo build` rather than read off crates.io. Transitive dependencies exclude the
crate itself, at default features:

| Crate | Deps | In `no_std` | Needs `alloc` | Licence |
|---|---|---|---|---|
| `ttf-parser` 0.25.1 | **0** | 2 (`core_maths`, `libm`) | **no** | MIT OR Apache-2.0 |
| `ab_glyph_rasterizer` 0.1.10 | **0** | 1 (`libm`) | yes | **Apache-2.0 only** |
| `ab_glyph` 0.2.32 | 3 | 5 | yes | **Apache-2.0 only** |
| `fontdue` 0.9.4 | 7 | 7 | yes | MIT / Apache-2.0 / Zlib |
| `swash` 0.2.10 | 12, one a proc-macro | 14 | yes | Apache-2.0 OR MIT |
| `cosmic-text` 0.19.0 | **39** | 30 | yes | MIT OR Apache-2.0 |

**The two-crate option is real and the APIs already fit.** `ttf-parser` gives
`Face::outline_glyph(GlyphId, &mut dyn OutlineBuilder)` against a trait of
`move_to`/`line_to`/`quad_to`/`curve_to`/`close`; `ab_glyph_rasterizer` accepts exactly
`draw_line`/`draw_quad`/`draw_cubic` and yields `for_each_pixel(|index, alpha: f32|)`. So
**`ttf-parser` plus `ab_glyph_rasterizer` is two crates, zero transitive dependencies on the host,
and no shaping stack at all.** That is a smaller graph than writing the rasteriser and taking only
the parser, and it is close enough to the §46 recommendation above that the honest thing is to say
so: the parser is the whole subsystem, and `ab_glyph_rasterizer` at 0 dependencies is a thin
primitive rather than the in-between. Take both.

**And it builds on both architectures already**, which is the finding that turns a recommendation
into a fact. The same peer session compiled the minimal combination, skipping `ab_glyph` and
`owned_ttf_parser` entirely: **four crates in total** (`ttf-parser`, `ab_glyph_rasterizer`, `libm`
shared between them, and `core_maths`), `no_std` plus `alloc`, clean for
**aarch64-unknown-none-softfloat** and **riscv64imac-unknown-none-elf**. §19 makes architectural
parity a tenet rather than an aspiration, and the cheapest option on the table is the one already
proven to build on both. That also matters for the option this block does *not* recommend: if a
runtime rasteriser is ever wanted after all, this is the one that could be.

**Two licence facts, since §46 makes a dependency a decision.** `ab_glyph_rasterizer` is
**Apache-2.0 only**, where nearly everything else in this tree is dual MIT or Apache-2.0. Apache-2.0
is on the allow-list so the gate is satisfied, but a dual-licensed project taking an Apache-only
dependency narrows what its downstream can do. Two things soften it: **the minimal combination
drops `ab_glyph` and `owned_ttf_parser`**, which were the other two Apache-only crates, leaving
`ttf-parser` dual and one crate not; and **a build-time atlas keeps even that one out of the
shipped graph.** And **`cosmic-text`'s tree contains `self_cell`, which is Apache-2.0 OR
GPL-2.0-only**, the only GPL-adjacent crate in any of these graphs; its tree also carries duplicate
majors of `read-fonts`, `font-types` and `skrifa`. That is rung three's problem and it should meet
it knowingly.

## Sequencing

Six increments. **The first two need no decision from calef and no dependency at all**, and they
are the larger half of "would use it outside a GUI".

1. **Grow the surface.** Build §102, take the scanout to 1280x720, and pay the harness cost
   deliberately: the referee's per-poll `screendump` and per-pixel comparison are 112 times bigger.
   Re-read `gfx_proto::WIDTH`'s non-square argument at the new size and record that it held.
   **Buildable today**, and it builds §102 rather than waiting on it.
2. **Make the grid a terminal.** 91x27 at the target cell, comfortably past the 80x24 floor,
   scrollback
   (a ring of off-screen rows and a viewport, which changes the damage model and is why milestone
   29 deferred it), UTF-8 in the VT engine with `bitfont::glyph` taking a `char`, and the arrow
   keys the keymap does not have. **Buildable today**, and this is the increment that earns the
   milestone's title.
3. **The atlas and the host-side generator.** A tool that turns an outline font into a coverage
   table, a checked-in table, and a gate that regenerating reproduces it byte for byte. Three
   things are decided already and should not be re-litigated in the lane: `ttf-parser` plus
   `ab_glyph_rasterizer` **pinned at 0.1.10**, **quantise to `u8` at the boundary and never persist
   raw `f32`**, and grayscale coverage rather than subpixel. Needs the font decision and the licence
   decision.
4. **Blending, in linear light**, with the sRGB tables. Small, and it is what makes the atlas look
   like type rather than like grey mush.
5. **Rich attributes.** Widen `Attr` and `Cell` for truecolour, real weights and underline styles;
   ship the bold, italic and bold-italic faces the widened `Attr` can now name. Retires
   *bold is bright*.
6. **The palette**, once milestone 141's gate exists to say which are admissible.

**A seventh, listed and not recommended: a glyph service.** A component holding the font files and
serving rendered glyphs, so a program that draws text needs no filesystem authority and no
rasteriser. It is the capability-shaped answer and it is genuinely attractive, but it buys nothing
this milestone needs (the atlas is smaller, faster and verifiable) and everything rung three needs
(arbitrary sizes, proportional faces, fallback). **Proposed as its own milestone when rung three is
live**, rather than folded in here.

## What is calef's, separated from what is blocking

**Blocking increment three, and only three:**

- **Which font family.** The candidates, their licences and their obligations are above; the
  choice is looking at them, which is how §100 was made and is the only way to make it.
- **Whether four faces or one.** Shipping regular alone is a smaller table and a poorer "rich".

**Blocking increment six:**

- **Which palette**, after milestone 141 lands its check. Solarized Dark Higher Contrast is his
  proposal and the finding above is what he needs before confirming it.

**Eventually his and blocking nothing:**

- Names. The generator tool, the atlas crate and any glyph service are all provisional.
- Whether the terminal ever gains a configurable palette, which milestone 141 already flags as a
  larger thing than it sounds because the palette is a `const` three parties agree on.

## Prior art

- **`cosmic-text`, and why it is rung three's rather than this one's.** It solves shaping,
  bidirectional text, fallback and proportional layout. A monospace grid needs none of them, and
  its dependency graph is priced above.
- **Every terminal emulator ships an atlas.** kitty, Alacritty and WezTerm all rasterise a glyph
  once and cache it in a texture, precisely because a terminal draws the same few hundred pictures
  forever. The move this block proposes is theirs with the cache moved from runtime to build time,
  which is available to us and not to them because our repertoire and size are fixed at build
  time and theirs are not.
- **macOS is the prior art for buying legibility with density** rather than with grid-fitting and
  subpixel tricks, which is the trade increment one makes when it picks a 2x cell.
- **`bitfont` is the prior art inside this tree**, and the structure it proved (a table, a pure
  function, a transcription script that changed no bits) is the structure increment three keeps.

## BUGS

- **The cell was measured from Menlo, and Menlo is not what will ship.** The 14x26 cell and the
  1280x720 that follows from it are read out of `/System/Library/Fonts/Menlo.ttc`, which is the
  right instrument for "what did calef ask for" and the wrong one for "what will we draw". DejaVu
  Sans Mono is Menlo's ancestor and its advance is close but not guaranteed identical; a lane
  should re-measure the chosen face before fixing the surface, and the surface should be chosen so
  a small difference does not cost a column.
- **Nothing here has been measured on the machine.** The harness cost, the table sizes and the
  cell arithmetic are all computed from constants in the tree. The soft-float claim in particular
  is read from two target JSONs and from what `compiler_builtins` provides; **no rasteriser has
  been run on either target**, which is exactly the measurement §46's rule 1 would want before a
  dependency were taken.
- **The atlas assumes the repertoire stays small.** Full Unicode coverage is not a bigger table,
  it is a different design, and a system that must render arbitrary text is back to a runtime
  rasteriser or a glyph service. The block says "a terminal does not need that" and a person
  running `cat` on a file of Devanagari would disagree.
- **Every determinism number here is measured behaviour, not a documented guarantee**, and the
  distinction is the whole caveat. Both investigations grepped both projects' READMEs, docs and
  changelogs for any statement of output stability and **found none**, so a future release may
  change the picture without calling it breaking. `ab_glyph_rasterizer` 0.1.5 already did exactly
  that, in a patch bump. The regenerate-and-compare gate is what turns the assumption into a check,
  and it is why increment three includes it rather than treating it as optional.
- **Neither project's CI tests more than one platform.** Both are `ubuntu-latest` only, so
  `ab_glyph`'s cross-architecture exactness is a property of its code that nothing upstream defends.
  There is also an **open, uncommented bug on precisely this arithmetic** (ab-glyph issue 121:
  accumulated float error producing tiny nonzero coverage outside the glyph, visible as shadows when
  text is drawn repeatedly). The reporter asks for better rounding. If that is fixed, output
  changes.
- **The x86 AVX2 path was verified statically, not run.** Rosetta does not expose AVX2, so the
  absence of fused multiply-add there was read out of generated assembly and the equivalence
  inferred. Rosetta is also not native x86 silicon: faithful for SSE float semantics, and still one
  machine rather than a fleet. Neither limit touches the two architectures this project actually
  targets, both of which were built and run.
- **This milestone enlarges §100's recorded supply-chain gap rather than inheriting it quietly.**
  `script/supply-chain` reads the cargo graph, so a font transcribed into a Rust table is invisible
  to it. That gap is a kilobyte of public-domain bitmap today and would become hundreds of
  kilobytes derived from a third party's obliging licence. The register in `vendor/README.md` is
  where it belongs, and it should be written at the same time as the table rather than after.
- **"Rich text" is read here as terminal attributes**, not as a document model. If calef meant
  proportional type, embedded images, or anything a terminal is not, this block answers the wrong
  question and the right answer is rung three.

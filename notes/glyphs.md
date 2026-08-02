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
5. **`crates/video_terminal` would not be deleted.** It is about 1,500 lines including its tests and its keymap,
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
| the wiring | `kernel/src/user.rs` (`display_service::start_terminal`, `compositor_service::spawn_terminal`, `keyboard_service`) |
| the tests | `kernel/src/user.rs` (`display_tests`, `compositor_tests`) |
| the host-side text check and its negative control | `xtask/src/main.rs` |
| the device lines | `scripts/qemu-runner.sh`, `scripts/qemu-runner-riscv.sh` (`CRICKER_KBD`) |

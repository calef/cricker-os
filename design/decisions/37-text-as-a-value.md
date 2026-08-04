# 37. Text is a value three witnesses compute, not a screenshot (milestone 29's remaining increment)

**Built 2026-07-30**, both ISAs, in QEMU. Font rendering, a VT state engine, a display terminal, and a
real keyboard: the piece that makes the display ladder's framebuffer readable. Concept note
notes/glyphs.md. (Section number chosen against `origin/main` at `92a0491`, where §35 is the scanner
policy. If a concurrent lane has claimed 36 by merge time, renumber; nothing here depends on it.)

**The decision, and it is the one to read: rendering is a pure function, so the expected picture is a
value.** A bitmap font and a sans-IO grid engine mean `pixel(x, y)` is computable by anyone holding
the script, and three parties do compute it, independently: the terminal runs the engine to draw, the
kernel runs it to predict the framebuffer pixel for pixel through the direct map, and `cargo xtask`
runs it on the host to grade what QEMU is actually scanning out. Text is where "it looked right" is
most tempting and least sufficient, and this is what replaces it. The host checker has its own
negative control and the assertion that carries the weight is **one letter changed** (`o` for a zero,
the closest pair of glyphs in the font): a checker that could not tell those apart would report
"readable text reached the scanout" for a terminal that drew the wrong text. It must also reject the
typed input missing and every rendition ignored, both of which are screens made of correct glyphs.

**The font is public domain, and the licence is the reason.** `font8x8` (Daniel Hepper, from Marcel
Sondaar's, from IBM's public-domain VGA fonts). A bitmap font is **compiled into the image**, so its
licence travels with the artefact rather than with a build-time tool; Terminus (OFL-1.1) and Spleen
(BSD-2-Clause) are fine fonts that would each have attached an attribution obligation to every binary
that draws text. Bitmap rather than scalable because a rasteriser wants an allocator, floating point,
and a font file, and because a pure function is what makes the paragraph above possible at all.

**Neither display contract needed a line changed, and that is now a spawn literal rather than a
claim.** The same `display_terminal` binary runs in two wirings: holding rung one's display endpoint and the
scanout with **exactly `painter`'s authority**, and holding rung two's doorbell and one window with
**exactly `window`'s authority**. `display` cannot tell it from the client that painted a test pattern;
`compositor` cannot tell it from the client that painted a coordinate function. Both contracts carry
pixels, and a terminal draws pixels. §29's note said a terminal would arrive as another client of that
contract; it did, twice.

**A found deadlock, and the better design it forced.** A terminal that answers a keystroke by ringing
the compositor's doorbell deadlocks as soon as two keystrokes arrive in one drain: the compositor is
blocked in its `CALL` to the terminal while the terminal is blocked in its `CALL` to the compositor.
That is §33's recorded cost of input-as-a-blocking-`CALL`, arriving in practice. It does not need to
ring: the compositor rescans every client's control page on every `COMMIT` from anyone, and the input
source rings `COMMIT` itself, so **the frame that delivers a keystroke is the frame that shows it**.
Application output still rings, because nobody else will, and that is safe because the caller blocked
in `CALL` there is the application. The design the deadlock ruled out was also the worse one.

**Input: the authority to type is a mapping, and the authority to route is a capability.** The
keyboard driver's power to inject a keystroke is the **input ring's mapping**, which no client has;
it is not the doorbell, which every client holds and which carries nothing. The driver holds no
client's endpoint and cannot name a client, so it cannot influence who receives what it types. That
is the compositor's, expressed as which of the per-client input endpoints *it* holds it uses, and a
client granted none has an empty cspace slot. So focus never becomes ambient: there is no verb that
grabs the keyboard, no message that names a recipient, and no page a client can write that would
inject input. The forgeable parts do not exist rather than being guarded (§33's idea, from the
producing side).

**The keyboard rides PCIe by choice, unlike the GPU.** Both `virt` machines *do* offer a
`virtio-keyboard-device` on the virtio-mmio bus, so this is not §29's "there is no mmio twin". It
rides PCIe so it lands in the same IOMMU domain the GPU does, because a keyboard is the device whose
DMA one would least like unconfined: its buffers are where every keystroke lands. Its event queue is
the **device-writes-into-driver-memory** direction, which the validator already proved for virtio-net
(§23, §30), so nothing in the confinement needed widening.

**The host is an actor for the first time.** Nothing in the guest can press a key, so `cargo xtask`
sends `sendkey` on the same QEMU monitor connection the scanout check already holds open, every poll,
from the start of the run. No synchronization is needed because QEMU **drops key events until a driver
sets `DRIVER_OK`**. The keyboard test then proves the path from a physical key event to a terminal
byte; the compositor test proves the ring to a focused terminal's pixels; the seam between them is the
ring, exactly where §33 put the authority boundary. Naming the seam is better than one test that hides
it. Verified headlessly on QEMU 11.0.2, both ISAs.

**No new syscall, no new method, no widened surface (§4).** The whole increment is endpoints, shared
frames, and `Spawn` grants that already existed. One constant moved: `MAX_DEVICES` 24 → 26, because a
third `display` programs the same physical GPU and no transport is ever released. Recorded with the
standing suggestion the number keeps earning, the same one §33 made about `KERNEL_EP_PAGES`: the
honest fix is releasing a transport when its driver dies.

**A bug worth recording because it is a real terminal bug.** The VT parser had no string state, so an
OSC sequence (`ESC ]0;title BEL`, how every program sets a window title) printed the title onto the
grid. Found on the host, in milliseconds, by the test that now feeds a title-setting sequence on
purpose. The interoperability test found its own footing the same way: the escape sequences a display
terminal must understand are the ones `line_editor` emits (§21), so rather than assert that from a list
that could drift, the test **runs the real line discipline** and feeds its echo stream to this parser.

**Deferred, and stated rather than implied:** no scrollback (the roadmap named it; it wants a ring of
off-screen rows and a viewport, which changes the damage model), no UTF-8 (the grid holds bytes and
the font covers basic latin), no line editing in the display terminal (`line_editor` composes in front of it
through `OP_WRITE` with no new protocol, which the `video_terminal` crate proves on the host by running both), no
reflow (nothing resizes), a US layout's main block only, and no mouse. notes/glyphs.md carries the
full list.

**The libghostty-vt question is left open on purpose, with the cost now measurable.** The roadmap
names it as milestone 23's strongest form and §31 built the C seam to de-risk it. Building the Rust
engine first was the right order, and it changed what the comparison is about: a VT engine fits the C
seam's shape almost perfectly (bytes in, cells out, no IO), so the port is a shim rather than a
rewrite, and **the work is the proof structure, not the rendering**. Our engine's `pixel(x, y)` is
what makes the three-witness check possible; libghostty-vt's C ABI gives cells, so the
expected-picture definition would have to be rebuilt against its layout. The recommendation in
notes/glyphs.md is to adopt it as a *second* engine behind the same seam rather than a replacement,
because a suite that grades two engines against each other is a better milestone-23 demonstration
than either alone. **Architect's call, not taken here.**

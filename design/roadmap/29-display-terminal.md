# 29. A display terminal: framebuffer, virtio-gpu, and a foreign component

**Status: BUILT.**

**In brief.** An on-device terminal: a userspace virtio-gpu driver (arriving over **PCIe**, which the §18 transport just made reachable), a framebuffer component, font rendering, and a VT state engine maintaining the grid; input from a virtio keyboard

**Why it matters.** the first pixels the demonstrator ever puts on a screen, and the strongest form of the milestone-23 claim if the VT engine is **libghostty-vt** (zero-dependency, no-libc, no-alloc, C ABI, Zig): a vendor component in a foreign language, capability-confined and hot-swappable. **Promoted from optional (2026-07-28): rung one of the display ladder (see [the display ladder](../display-ladder.md)), whose destination is a capability-routed compositor**

**Increment one built (2026-07-29, both ISAs): the first pixels, and the framebuffer seam.** A
confined userspace virtio-gpu driver (`display`) drives the control queue through the proved validator
over the §18 PCIe transport on both `virt` boards, behind the IOMMU; a *separate* client (`painter`)
holds only an endpoint and a shared surface and draws a coordinate-derived pattern into it. Two
witnesses in two address spaces digest the result against a value the kernel computes itself, so the
**framebuffer** is proven byte for byte; and the **scanout** is proven too, from the host, by driving
QEMU's monitor beside the suite and comparing a `screendump` PPM against the same pattern definition
pixel for pixel (both ISAs, with a negative control on the checker). The memory decision
generalized to a rule (a framebuffer is a bigger grant, never an exemption) and the GPU's own
confinement hazard (backing addresses ride in a command payload the transport validator cannot see, so
the IOMMU is the barrier) is proved by an attacker test. DECISIONS §29,
notes/framebuffer-contract.md.

**Increment two built (2026-07-30, both ISAs): glyphs, the grid, and a real keyboard.** DECISIONS
§37, notes/glyphs.md. A public-domain 8x8 bitmap font (`crates/bitfont`; the licence drove the choice,
because a font is compiled into the image), a **sans-IO VT engine** (`crates/video_terminal`) checked against the
*real* line discipline's echo stream rather than a written-down list of escape sequences, a display
terminal (`user/src/display_terminal.rs`) that is a client at **both** display seams with exactly `painter`'s and
exactly `window`'s authority, and a confined virtio-input keyboard driver (`user/src/kbd.rs`).

Three things are worth carrying forward from it. **The picture is a value three witnesses compute
independently** (the terminal to draw, the kernel to predict the framebuffer, the host to grade QEMU's
scanout), which is what replaces "it looked right" for text; the host checker's negative control is a
screen with **one letter changed**. **Neither display contract needed a line changed**, and that is
now a spawn literal rather than a claim. And **the authority to type is a mapping**: the keyboard's
power is the input ring no client maps, while the doorbell it rings carries nothing, so focus stays a
capability from the producing side as well as the receiving one.

**Still deferred, and stated rather than implied:** scrollback (it wants a ring of off-screen rows and
a viewport, which changes the damage model), UTF-8, reflow, and line editing in the display terminal
(`line_editor` composes in front of it with no new protocol, which `crates/video_terminal` proves on the host). The VT
engine's language remains an open question, and notes/glyphs.md now **prices** it: building the Rust
engine first changed what the comparison is about, because a VT engine fits the §31 C seam's shape
almost perfectly and the real cost of adopting libghostty-vt is rebuilding the *proof structure*, not
the rendering. The recommendation there is to adopt it as a second engine behind the same seam rather
than a replacement. **Architect's call.**

**Deliverable.** The demonstrator's first pixels: a userspace **virtio-gpu** driver (the device
arrives over PCIe on both `virt` boards, which the §18 transport just made reachable), a
framebuffer mapped into a terminal component, font rendering, and a VT state engine maintaining
the grid (escape parsing, scrollback, wrapping, reflow); keyboard input via virtio-input. The
serial console remains; this is a second head, not a replacement.

**Why, and the 23 connection.** The VT engine is the strongest candidate anywhere in the plan
for the full form of milestone 23's claim: **libghostty-vt** (Ghostty's extracted core:
zero-dependency, no libc, fixed buffers with no allocations, C ABI, implemented in Zig) running
as a capability-confined, hot-swappable vendor component would mean the kernel safely runs code
we did not write, in a language we do not use. Costs stated plainly: a Zig toolchain enters the
build for that one component, and their API is still in flux, so any adoption pins a version.
The single-toolchain fallback is `vte` (alacritty's parser): same shape, our language, much less
complete (no scrollback or reflow).

**Sequencing.** Needs the PCIe transport (done) and wants 28's contract first so the display
terminal implements a contract rather than inventing one. Optional and well off the thesis path;
a reach in the 24 spirit. **Effort: 2 lanes** (measured: first pixels, then glyphs/VT/input).

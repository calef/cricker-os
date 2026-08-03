# The display ladder (recorded 2026-07-28, Chris's direction)

The stated destination: eventually, something like COSMIC driving a GPU for display. That
decomposes into rungs, each independently a demo, and the decomposition is what makes the ambition
honest. COSMIC's shape is Rust clients rendering into shared buffers, a compositor compositing
them to scanout, everything message-passing; cricker-os already has shared frames and endpoints,
so the *architecture* is aligned even where the drivers are mountains.

**Status (2026-07-30): rungs one and two are built, and rung one's deferred VT engine now is too.**
Rung one shipped its contract and its pixels, rung two shipped whole, and milestone 29's remaining
increment closed the gap with a bitmap font, a sans-IO VT engine, a display terminal that is a client
at both seams, and a confined virtio keyboard (DECISIONS §37, notes/glyphs.md). All on both ISAs, all
with the pictures verified from the host as well as the guest: `cargo xtask` now proves **three**
pictures over one boot, in order, and the text check's negative control is a screen with one letter
changed. Rung three is the next step and is where the parked competitor question ([competitor-question.md](competitor-question.md)) has to be
answered on purpose.

1. **Rung one: milestone 29** (promoted from optional). **Built**: a confined userspace virtio-gpu
   driver (`display`), a client that draws (`painter`), and the framebuffer contract between them
   (`crates/gfx_proto`, notes/framebuffer-contract.md, DECISIONS §29). The framebuffer is a bigger
   grant and never an exemption; the pixels are proved in the guest by two witnesses in two address
   spaces and from the host by comparing QEMU's `screendump` against the pattern definition.

   **Its deferred half is built too** (2026-07-30, DECISIONS §37, notes/glyphs.md): a public-domain
   8x8 bitmap font, a sans-IO VT engine, a display terminal, and a virtio keyboard. The deferral's
   premise held exactly as written: the contract carries pixels, not text, so the terminal arrived as
   another client and **neither `gfx_proto` nor `display` changed a line**, which the same binary then
   demonstrated a second time by being a compositor client with `window`'s authority. The VT engine's
   language is still an open choice, and notes/glyphs.md now prices libghostty-vt against a built
   Rust engine rather than against an estimate.
2. **Rung two: a compositor component (milestone 33). Built**, both ISAs: `compositor` multiplexing one
   screen among three mutually distrusting clients, each holding a capability to its own surface;
   software composition honouring a damage rectangle; input routed by capability using the terminal
   contract's `OP_BYTES` driver half, so a terminal drops in unchanged. No ambient display: window
   enumeration and screenshots are **read-only mappings**, not verbs, so a client that holds neither
   has nothing to call and nowhere to look. See notes/compositor.md and DECISIONS §33. The design's
   load-bearing idea, which was not the obvious one: the shared doorbell endpoint carries **no
   authority at all** (a shared endpoint has no sender identity, so anything named in a message would
   be forgeable), every per-client fact lives in per-client memory, and the compositor therefore
   contains no authorization code. Wayland's model is the prior art and this is the difference in kind
   from it: Wayland attaches client identity at the transport and decides in code, so its security is a
   property of that code.

   **The rung also found the one primitive this kernel lacks**, and it is recorded as a fork rather
   than built: there is no wait-any, and two threads cannot share an address space, so a process has
   exactly one blocking wait point. A component that must distinguish more than one *class* of sender
   must therefore be more than one process, or carry authority somewhere other than its messages. The
   compositor took the second road, and it turned out stronger; but with the primitive, per-client
   endpoints would give unforgeable identity for free (letting a bad damage rectangle be refused to its
   author rather than clipped), a screenshot could be a consistent served snapshot, and input delivery
   would stop being a blocking `CALL` into a client. DECISIONS §33 has the two candidate forms and
   their costs. **Architect's call.**
3. **Rung three: real applications.** iced's software-rendering path and cosmic-text on the
   milestone 27 std PAL. Something COSMIC-like appears here, before any GPU.
4. **Rung four: GPU acceleration via virtio-gpu 3D (milestone 34).** The Venus path (Vulkan over
   the virtio device, over the §18 PCIe transport): how every VM gets a GPU without a hardware
   driver, and what would give wgpu something real. A mountain, but a climbable one, priced as
   such.
5. **Rung five: struck.** A bare-metal driver for the VisionFive 2's BXE-4-32 3D core is a
   Linux-scale multi-year effort (loaded firmware, thin documentation, Mesa still maturing on
   Linux itself) that proves nothing rung four does not. The board's standalone-display story is
   the DC8200 framebuffer path instead: U-Boot's `simple-framebuffer` handoff first (zero display
   code), a mode-setting driver only if ever needed, serial input until a USB HID milestone earns
   its own number. The JH7110 has no IOMMU, so display DMA on that board is confined by software
   discipline, and the record will say so.

Governance, stated now so it is not smuggled later: rungs one and two are demonstrator work.
Rungs three and four reopen the parked competitor question ([competitor-question.md](competitor-question.md)), which is the architect's call
to make consciously when rung two is real. **Rung two is now real, so that call is live**, and
milestone 33 deliberately stopped at its edge: no iced, no cosmic-text, no application work.

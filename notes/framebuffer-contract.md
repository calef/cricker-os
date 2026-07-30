# The framebuffer contract

Milestone 29, rung one of the display ladder. The contract a client speaks to get pixels on a
screen, written down so rung two (the compositor, milestone 33) implements against a contract
instead of inventing one. The code half is `crates/gfx_proto`; this is the prose half, the same
split `notes/fs-server.md` makes for the filesystem and `notes/terminal-contract.md` for the
terminal.

## The shape

```text
  virtio-gpu ──virtio (PCIe, behind the IOMMU)──► display driver ──display IPC──► client
       │                                              │  (gpud)                    (painter)
       └──── DMA: the whole region ──────────────────► │
                                   the surface ────── shared frames ──────────────┘
```

Three parties. The **device**, on the PCIe bus. The **display driver** (`user/src/gpud.rs`), which
owns the device and serves the contract. The **client** (`user/src/painter.rs`), which owns the
pixels and has never heard of virtio-gpu.

What each holds is the whole design:

| | display driver | client |
|---|---|---|
| slot 0 | report endpoint (status to its spawner) | report endpoint |
| slot 1 | `Irq`, the device's completion interrupt | the display endpoint, WRITE (it CALLs) |
| slot 2 | `Virtio`, the confined transport | |
| slot 3 | the display endpoint, READ (it serves) | |
| mapped | the whole DMA region (rings + surface) | the surface frames only |
| knows | its DMA region's physical base | no physical address at all |

The client cannot program a queue, cannot ring a doorbell, cannot see a descriptor ring (they are in
a page it is not mapped), and cannot name a physical address. **So the worst a hostile client can do
is draw nonsense**, which is exactly the authority a thing that draws should have.

That separation, not the picture, is why this increment exists. A compositor is the canonical
multiplexer of one device among mutually distrusting clients, and it can only be that if the thing
that draws is provably not the thing that talks to the hardware. Rung two takes the client's place
here unchanged.

## Control by message, pixels by shared frame

DECISIONS §10's division, third instance (after blk IPC and file IPC). A request is an endpoint
`CALL` carrying an opcode and a rectangle; the pixels never ride in a message. They live in frames
the kernel maps into both address spaces at spawn, which the client may write at any time without
asking anyone.

Two verbs, and that is the whole surface today:

- **`INFO`** -> `(0, width | height << 32)`. Ask the surface's geometry rather than assuming it. The
  crate's constants are the compile-time contract; this is the runtime one, and it is the question
  that still makes sense at rung two, where the compositor hands a client a surface whose size the
  client did not choose.
- **`FLUSH(rect)`** -> `(0, 0)` or a negative errno. "The pixels in this rectangle changed; get them
  onto the screen." The driver does the two device commands that means and does not reply until the
  device has completed both. The opcode and the rectangle both ride in the first word (`gfx::req`
  packs the rectangle into its low 56 bits), so a flush is one message with no second word to keep in
  step.

A rectangle outside the surface is **refused, not clamped**. A clamp hides a client's coordinate bug,
and the client is the only party that can tell the difference. `EINVAL` (-22), the same
negative-is-an-error convention `fs_proto` sets.

Why two device commands per flush, in this order: `TRANSFER_TO_HOST_2D` is what actually reads the
guest pixels (the device DMAs them out of the backing into its own host-side image), and
`RESOURCE_FLUSH` is what makes the host show that image. A driver that only flushed would put stale
pixels on the screen.

## The memory story: a framebuffer does not fit in a page

Every other driver here gets one 4 KiB DMA page. A 128x64 surface at 4 bytes a pixel is 32 KiB, so
the GPU needs eight more. The tempting shortcut is to let the framebuffer live outside the DMA
region: it is "just pixels", after all. That would put **the one device that reads bulk memory
outside the confinement everything else is inside**, which is exactly backwards.

So the region is **wider, not special**:

```text
  page 0        rings (queue 0 at 0x000) + control request (0x400) + response (0x600)   driver-private
  pages 1..8    the surface, 32 KiB of pixels                                           shared with the client
```

`1 + gfx_proto::SURFACE_FRAMES` **contiguous** frames, allocated with `alloc_contiguous` and
registered whole as the driver's DMA region. Three things follow, and they are the reason this is the
right shape:

1. The shadow-ring validator bounds every descriptor to `[dma_base, dma_base + dma_size)`, and the
   surface is inside that. **`crates/dma_validate` needed no change at all**, because it bounds
   addresses against a region whose size is a parameter; the region got nine times bigger and the
   proof still covers it. (This was the one place the increment could have tempted a change to a
   proved crate, and it did not.)
2. `iommu::confine` maps exactly that region plus the kernel's shadow page, frame-granular. The
   device can reach the pixels and nothing else.
3. The client maps only pages 1..8, so it cannot touch a descriptor ring even though it shares
   memory with a driver.

The block server already took a two-page region this way for whole-block reads (milestone 32). This
is the same move, wider, and it is worth stating as a general rule: **a device that needs more memory
gets a bigger grant, never an exemption.**

The backing is one virtio-gpu memory entry rather than eight, because the frames are contiguous. That
is not required (the command takes a list) but it keeps the domain a single range and the entry list
trivial.

## The confinement hazard a GPU adds, and what actually stops it

This is the part worth reading twice, because it is the first device here whose DMA addresses do not
all arrive in descriptors.

Everywhere else, every address a device will touch arrives in a **virtqueue descriptor**. The kernel
validates each one and copies it into a shadow ring the driver physically cannot write, so the bytes
the device acts on are the bytes the kernel checked (notes/dma.md). A virtio-gpu's *backing*
addresses arrive somewhere else: inside a `RESOURCE_ATTACH_BACKING` **command payload**. The kernel
bounds the descriptor that carries that command, but the addresses inside it are bytes it does not
parse.

And it should not start parsing them. Teaching the transport to read virtio-gpu commands would put
device knowledge in the one place DECISIONS §18 keeps device-neutral, and it would be a per-device
arms race: the next device class with addresses in a payload would need its own parser in the kernel.

**So the IOMMU is the barrier for this class of address, and that is proven, not assumed.**
`the_iommu_refuses_the_gpu_a_framebuffer_outside_the_drivers_grant` gives a driver exactly the honest
driver's authority, points the resource's backing at a frame the kernel deliberately left out of its
domain, asks the device to transfer from it, and asserts the IOMMU recorded a fault at that frame.
Both ISAs.

Two consequences follow, both stated plainly because they matter:

- **`iommu_platform=on` carries more weight for the GPU than for the disk.** Drop it from the runner
  and the disk still has the shadow ring; the GPU has nothing bounding its backing. The runners say
  so at the device line.
- **On a board with no IOMMU, this hazard is open.** The VisionFive 2 has no IOMMU (notes/dma.md, the
  standing default), so a display driver on first silicon would be able to point the GPU at any
  physical address. On that board the display driver has to be trusted, or the transport has to grow
  a virtio-gpu-aware check, and the record should say which was chosen rather than discovering it
  later. Under HVF, likewise: PCIe DMA runs unconfined there by standing default.

### A surprise worth recording: the device's "OK" is not evidence

The first version of the escape test asserted that the device *refused* the command. It did not.
QEMU's DMA layer answers a translation failure by handing the device a **bounce buffer** rather than
failing the mapping, so `RESOURCE_ATTACH_BACKING` returns `OK_NODATA` while the bytes the device
actually gets are not the victim frame's. The confinement held; only the error reporting did not
survive the trip.

So the test asserts on the **IOMMU's fault queue**, which is the hardware's own account, and the
response code is only printed for the record. A test that had trusted the response code would have
reported a hole that was not there, and (worse, in the other direction) a test that trusted an `OK`
as "the device read it" would have reported a read that did not happen.

An earlier iteration also aimed the escape at "the frame just past my region", which was a bad guess:
the kernel's shadow page is allocated immediately after the DMA region, and that frame **is** in the
domain. The kernel now picks the victim frame and hands it to the attacker in `arg2`, the same way
milestone 16b's confinement test picks its own, because the test has to know the exact address to
look for in the fault queue.

### A found limit: the RISC-V IOMMU's fault queue overflows silently

Worth its own heading because it will matter more to whoever routes faults to a production handler
than it did here. The RISC-V IOMMU driver gives the fault queue 128 records
(`FQ_LOG2 = 7` in `kernel/src/arch/riscv64/iommu.rs`) and **never clears the queue's overflow bit**.
So a flood of faults latches the overflow and, from then on, *no further faults are recorded at all*.

This was found the way such things should be: the escape test first attached a 4096-byte backing,
which produced a flood, and the **next** test in the suite (milestone 16b's
`the_iommu_faults_a_dma_that_escapes_the_domain`) then observed no fault and correctly reported the
IOMMU as not confining the device. A real regression signal, from a cause two tests away.

Two mitigations, both local to milestone 29 because the driver's overflow handling is not this
milestone's lane:

- the escape attaches **four bytes**, so it provokes exactly one translation and one fault, which asks
  the same security question ("can this device reach an address outside its grant?") without flooding;
- the test **drains the queue** when it is done, leaving it as it found it.

What is left for a fault-handling milestone: clear the overflow bit when draining, and decide what a
production kernel does when a confined device faults at all (today the queue is drained only by tests;
DECISIONS §20 already records fault routing as future work). aarch64's SMMUv3 event queue has the same
shape of limit and was not exercised into it here.

## What the test proves, and the one thing it does not

`a_confined_userspace_driver_puts_a_known_pattern_in_a_framebuffer`, one arch-neutral test that runs
on both ISAs (everything under it is portable: same two binaries in both archives, same PCIe seam on
both boards, one host-tested contract crate).

**Proven:**

- The pattern is a **per-coordinate function**, not a fill: red rises with `x` five times faster than
  with `y`, green rises with `y`, blue is `x xor 2y` times an odd number. A blank buffer, a solid
  fill, a loop that never advanced, a transposed surface, a one-row stride error, and a one-pixel
  shift all fail it. `crates/gfx_proto`'s host tests assert those properties of the pattern itself,
  so the pattern's fitness is checked in milliseconds rather than trusted.
- The digest is **position sensitive** (FNV-1a over the pixel words in row-major order), so the same
  pixels in the wrong order is a different answer.
- **Two independent witnesses in two address spaces.** The client digests the surface after the
  flush through its own mapping; the driver digests it through a different mapping after the device
  reported the transfer complete. The kernel compares both against a value it computed itself from
  the contract, so neither process grades its own homework, and the two are also compared against
  each other.
- **The device could reach exactly those frames and no others**: the successful
  `RESOURCE_ATTACH_BACKING` happened while translation was in force, and the escape test shows an
  address outside the grant faults.
- The one-shot client is **reaped**, not leaked, before the test returns.

**Not proven by the in-guest test: the scanout.** The suite runs `-display none`, and nothing inside
the guest can read QEMU's host-side surface back, so "the bytes we handed the device are the bytes it
read out of our frames" is as far as an in-guest test reaches. A wrong pixel **format** or a wrong
scanout rectangle would pass it and show garbage on a real screen.

## Proving the scanout, from the host

That gap is closed, and by the host rather than the guest, because only the host can see the pixels.

QEMU's monitor works headlessly: `screendump FILE` writes a PPM of the scanout even with
`-display none` (verified against QEMU 11.0.2). So the runners take a monitor socket
(`CRICKER_GPU_MON`), and `cargo xtask`'s `cargo_test_with_scanout_check` drives it **while the
ordinary test run is happening**: it spawns the suite, and beside it polls the monitor every 100 ms,
dumps the scanout, and compares the PPM against `gfx_proto::pixel`, the same definition the client
painted from. The first match ends the polling. Both ISAs. On success it prints:

```text
scanout check (aarch64): the 128x64 pattern reached the DEVICE's scanout, verified pixel for pixel
against gfx_proto::pixel (target/gpu-scanout-aarch64.ppm)
```

**So the pixels are proven all the way to the device**, not just to our own frames: the guest's two
witnesses agree about the framebuffer, and the host independently confirms that what QEMU is
scanning out is the pattern, pixel for pixel, in the right format, at the right geometry.

Three details that make this a real check rather than a comforting one:

- **It runs inside the existing test run, not a second boot.** The suite is minutes long per ISA, and
  the pattern stays on the scanout from the display test until QEMU exits, so there is nothing to
  synchronize with the guest. No extra QEMU, no marker protocol.
- **The geometry is part of the assertion.** QEMU's console defaults to 640x480 and `SET_SCANOUT`
  resizes it, so a dump that is 128x64 is itself evidence our scanout rectangle reached the device. A
  scanout never set fails on size before a single pixel is compared.
- **The checker has a negative control** (`cargo test -p xtask`): it must reject a black scanout, a
  red/blue-swapped one (what a wrong virtio-gpu format code produces, and exactly what the in-guest
  test cannot see), a one-row shift, a single wrong pixel, and the default console size. A checker
  that accepted anything would report success on every run, which is worse than no check.

Ordering is load-bearing and deliberately fail-loud: the confinement test resets the device (which
destroys the scanout), so it must run **before** the pixel test. That is why it is named
`a_backing_outside_the_grant_is_refused_by_the_iommu` rather than `the_iommu_...`, and the reason is
in its doc comment. If the order ever changes, no dump matches and the scanout check fails; nothing
is silently waved through.

Two practical constraints found while building it, recorded so they are not rediscovered: the unix
socket path must stay under 104 bytes (a worktree checkout plus `target/` gets close, so the socket
lives in `/tmp` while the PPM goes under `target/`), and `socat` is not installed here, so the monitor
client is xtask itself over `std::os::unix::net::UnixStream`.

What is still **not** proven, to be exact about it: that a physical panel would show this. QEMU's
scanout is the last thing we can observe, and on real hardware there is a display controller past it.
That is a silicon question (notes/target-hardware.md), not a QEMU one.

## What rung two did with this contract (milestone 33)

Written back here on 2026-07-29, because a contract's real test is what happened when the next thing
implemented against it, and the answer is worth recording: **nothing in this rung changed.**

`crates/gfx_proto` and `user/src/gpud.rs` are byte-for-byte the same. The compositor
(`user/src/compd.rs`) took `painter`'s place at this seam, holding the display endpoint and the scanout
frames with exactly `painter`'s authority, and the driver cannot tell the difference. The only addition
on this side of the seam is a kernel wiring entry point that starts the driver **with no client**
(`display_service::start_driver`), because rung two's client is spawned separately with a scene behind
it.

Three of the four rung-two tests go further and replace `gpud` with a **kernel stand-in** that serves
`INFO` and `FLUSH` over frames the kernel allocated. The compositor does not notice that either, which
is milestone 23's swappable-component claim arriving as a side effect of a contract rather than as a
demonstration built on purpose. It also made the damage rectangle *observable*: a real driver honours a
rectangle and says nothing about it, so the stand-in is how "a one-window redraw does not cost a whole
screen" became an assertion.

Two predictions in the list below came out exactly as written, and one was answered differently:

- **Damage tracking**: the compositor flushes one rectangle per frame, and the test poisons the rest of
  the scanout and finds the poison intact. As predicted, no driver change.
- **Input**: the keyboard's routing question turned out to be the compositor's, as predicted, and the
  answer reuses `linedisc::proto::OP_BYTES` verbatim.
- **Several surfaces**, differently: the prediction was "a compositor holding one endpoint per client
  surface needs a driver change and not a contract change". Rung two holds **one** endpoint for all its
  clients instead, because a shared endpoint carries no sender identity and per-client surfaces are
  therefore identified by *memory* rather than by endpoint. Still no contract change, and still no
  driver change, but by a different route than this note guessed. See notes/compositor.md.

The scanout check grew accordingly: `cargo xtask` now proves **two** pictures over one boot, the
composed screen first and this rung's pattern second, both on both ISAs.

## What the display terminal did with this contract (milestone 29's text increment)

Written back here on 2026-07-30, for the same reason rung two's section exists: a contract's real test
is what happened when the next thing implemented against it. **Nothing in this rung changed.**

`crates/gfx_proto` and `user/src/gpud.rs` are byte-for-byte the same again. The display terminal
(`user/src/vterm.rs`) takes `painter`'s place at this seam with **exactly `painter`'s authority**: a
report endpoint, the display endpoint, and the surface frames. It draws glyphs instead of a
coordinate pattern and calls `FLUSH` with the rectangle of cells that changed, which is what this
note said a client does. The only addition on this side is a kernel wiring entry point
(`display_service::start_terminal`), and the only thing the terminal adds over `painter` belongs to
the *terminal* contract rather than this one: an endpoint it serves, and a page an application writes
text into.

So the prediction in the list below came out exactly as written, and the seam claim is now made twice
in one milestone: the same binary is also a compositor client, with exactly `window`'s authority, and
`compd` cannot tell it from the client that painted a pattern either. See notes/glyphs.md.

The scanout check grew accordingly: `cargo xtask` now proves **three** pictures over one boot, in
order (the composed screen, the terminal's text, then this rung's pattern), on both ISAs.

## The seams left open

Deliberately not in rung one, each with the seam it will use:

- **Font rendering and a VT state engine.** ~~They arrive as a *client* of this contract~~ **Done,
  2026-07-30**, and as predicted: a terminal draws glyphs into a surface and calls `FLUSH`, with no
  change here. The VT engine is Rust (`crates/vt`); libghostty-vt is still an open choice and
  notes/glyphs.md prices it now that there is a built engine to compare against.
- **Input.** **Done, 2026-07-30.** A keyboard is a second device with its own driver and its own
  capability (`user/src/kbd.rs`, virtio-input over this same PCIe transport and behind the same IOMMU
  domain), and the routing question turned out to be the compositor's exactly as predicted. What this
  note did not predict is where the *typing* authority lives: it is the input ring's mapping, not the
  driver's device, and the doorbell it rings carries nothing at all. See notes/glyphs.md.
- **Several surfaces.** One resource id today, hardwired. The contract routes by *endpoint*, so a
  compositor holding one endpoint per client surface needs a driver change and not a contract change.
- **Damage tracking.** `FLUSH` already takes a rectangle and the driver honours it (the transfer's
  offset is computed from it), so a compositor that redraws one window does not pay for the screen.
  The client today flushes the whole surface because it changed the whole surface.
- **The cursor queue.** virtio-gpu has a second virtqueue for a hardware cursor. The driver never
  sets it up, which is why the multi-queue confinement's two-queue ceiling (DECISIONS §23) is
  untouched by this milestone.

## Where the pieces are

| piece | file |
|---|---|
| the contract, host-tested | `crates/gfx_proto/src/lib.rs` |
| the display driver | `user/src/gpud.rs` |
| the client that draws | `user/src/painter.rs` |
| enumeration | `kernel/src/pci.rs` (`find_gpu_device`) |
| the spawn wiring | `kernel/src/user.rs` (`display_service`) |
| the tests | `kernel/src/user.rs` (`display_tests`) |
| the device lines | `scripts/qemu-runner.sh`, `scripts/qemu-runner-riscv.sh` (`CRICKER_GPU`) |

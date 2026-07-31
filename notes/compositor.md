# The compositor

Milestone 33, rung two of the display ladder. One screen, several mutually distrusting clients, each
holding a capability to its own surface. The code halves are `crates/compose` (the contract and the
pixel arithmetic), `user/src/compositor.rs` (the compositor), and `user/src/window.rs` (a client); this is
the prose half, the same split [framebuffer-contract.md](framebuffer-contract.md) makes for rung one
and [terminal-contract.md](terminal-contract.md) for the terminal.

Rung one asked whether an unprivileged process could put pixels on a screen without holding a device.
Rung two asks the question that makes a compositor interesting: **can several processes share one
screen without being able to reach each other?**

## The shape

```text
  virtio-gpu ──virtio (PCIe, IOMMU)──► display ──gfx FLUSH(damage)──► compositor ◄──doorbell CALL── clients
                                        │      (rung one's contract)    │  │                        (a surface each)
                                        └───────── the scanout, shared ─┘  ├── input ring (shared with the
                                                                           │   input source only)
                                                                           └── one input endpoint per
                                                                               focusable client
```

**The compositor is rung one's client, unchanged at that seam.** It holds the display endpoint and the
scanout frames exactly as `painter` did, and `display` cannot tell the difference; three of the four
kernel tests replace `display` with the kernel itself and the compositor does not notice that either.
That was the promise the framebuffer contract made when it said routing was by endpoint, and it cost
nothing to keep: `display` and `gfx_proto` needed no change for this milestone beyond one new
kernel-side wiring entry point (`display_service::start_driver`, the driver with no client).

What each party holds:

| | compositor | an honest client | a capture client |
|---|---|---|---|
| slot 0 | report endpoint | report endpoint | report endpoint |
| slot 1 | display endpoint, WRITE | the doorbell, WRITE | the doorbell, WRITE |
| slot 2 | the doorbell, READ | its input endpoint, READ (focusable only) | *(empty)* |
| slots 3.. | one input endpoint per focusable client, WRITE | | |
| mapped RW | the scanout, the window list, the input ring, every client's control page and surface | its own control page and surface | its own control page and surface |
| mapped RO | | | **the screen and the window list** |
| knows | nothing about any device | nothing about the screen, its own position, or its neighbours | |

## The idea the whole design rests on: the doorbell carries no authority

Every client rings **one shared endpoint**, and every request on it is content-free. There are two
verbs, `HELLO` ("I have started") and `COMMIT` ("look at the surfaces"), and neither takes an
argument. That is not minimalism for its own sake.

A shared endpoint carries **no sender identity**. There are no badged capabilities here (DECISIONS
§26.5 records that decision and what would bring it back), so a server receiving on an endpoint that
several clients hold cannot tell which one sent a message. A protocol that named a surface, a window,
or a rectangle in its *message* would therefore be forgeable by any client: `flush(window 2)` from the
holder of window 0 would be indistinguishable from the real thing. That is the vulnerability this rung
exists not to have.

So the design inverts it. **The message says nothing; the memory says everything.**

- **Every per-client fact lives in per-client memory.** A client's geometry, its window id, its damage
  rectangle and its sequence counter are fields in a control page that only it and the compositor map
  (`compose::proto::ctl`). The only surface a client can describe is its own, because the only control
  page it can write is its own.
- **Every privileged answer travels through privileged memory**, never through a reply. A screenshot is
  a read-only mapping of the screen. The window list is a read-only page the compositor publishes.
  There is **no enumerate verb and no screenshot verb**, so there is nothing for a hostile client to
  call and nothing for the compositor to guard.
- **Keystrokes arrive in memory too**, an input ring the input source shares with the compositor and
  nobody else. A keystroke carried in a message word would let any client inject input into the
  focused client; a keystroke in a page no client maps cannot be forged at all.
- The reply words carry **status only**, and the kernel's one-shot Reply capability routes them to
  whoever called (DECISIONS §12). So a request is answered correctly without the compositor ever
  learning who asked.

The result is a compositor with **no authorization code in it**. It never asks "may you?" about
anything, because there is no request it could receive that would need the question. It cannot leak
the screen to a client that asks nicely, since handing over the screen is not an operation it has.

That is the difference in kind from Wayland, and worth stating precisely because Wayland is the prior
art and is often described as if it had already solved this. A Wayland compositor holds a socket per
client and every request arrives on it *with the client's identity attached by the transport*; the
compositor then decides, in code, what that client may do. Its security properties are properties of
that code. Here the compositor holds a capability per client too (an input endpoint), but the
authority to *read* anything is a mapping the kernel made at spawn, and no code path in the compositor
can widen it. Wayland's model approximates capability routing; this is capability routing.

## What a frame is

1. A client paints its own surface, writes its damage rectangle into its own control page, bumps its
   sequence, and `CALL`s `COMMIT`.
2. The compositor drains the input ring (below), then walks every client's control page. A sequence
   that changed means new pixels: clip that client's rectangle to that client's surface, place it on
   the screen, and union it into this frame's damage.
3. Composite the damage: background first, then every window in stacking order, each clipped to the
   damage. Opaque windows, so the last writer wins and the stacking order is visible in the result.
4. `gfx FLUSH(damage)` to the display, and reply to whoever rang.

Two properties of that loop are worth naming.

**The caller is blocked in `CALL` for the whole of composition**, which is what makes reading a
client's pixels safe with no lock and no double buffering: the client that rang cannot be writing
while the compositor reads. That is flow control by rendezvous rather than by trust, and it is the
same argument [line-discipline.md](line-discipline.md) makes for the terminal. A client that rings on
*another* client's behalf gains nothing, since it is still only its own control page it can write.

**A client's rectangle is untrusted input**, so it is clipped, not believed. Rung one *refuses* an
out-of-surface rectangle rather than clamping it, on the grounds that the caller is the only party who
can tell a coordinate bug from intent. Rung two clips, and the reason is the same identity-free
doorbell: the compositor scans every surface, so it cannot attribute a bad rectangle to a caller in
order to refuse *that caller's* request. And the worst a lie can do is make the compositor re-copy the
liar's own pixels. So it clips and records `STATUS_CLIPPED` **in the liar's own control page**, which
is per-client feedback through the only channel that can carry it.

## Focus is a capability, not a variable

Three questions Unix conflates, separated here:

- **Who may deliver input?** Whoever maps the input ring. That is the input driver, and in the tests
  it is the kernel playing the driver's part (`Wiring::type_bytes`). No client maps it, so no client
  can inject a keystroke into another. There is no "grab the keyboard" verb to guard because there is
  nothing a message could say that would do it.
- **Who may receive it?** The focused client, because it *holds an input endpoint*. A client without
  one cannot be sent a keystroke by anyone, and its attempt to receive on the slot where one would be
  is refused by the kernel with `NoSuchSlot`. Holding the endpoint is what makes a client eligible for
  focus at all.
- **Who decides?** The compositor, in userspace, on policy of its own (TAB moves focus to the next
  window). The kernel routes the message and knows nothing about focus. The decision is *published* in
  the window-list page, so a holder of that page, and the kernel test, can witness a focus change
  rather than ask about it.

Input reaches the focused client as `lineedit::proto::OP_BYTES` over a `CALL`: the terminal contract's
driver half, verbatim (notes/terminal-contract.md). So a terminal is a client of this compositor
without either contract changing, which is what rung three needs and the reason the framing was reused
rather than reinvented.

**That claim was cashed on 2026-07-30** (milestone 29's text increment, notes/glyphs.md), and the
routing is now visible in the picture rather than only at the endpoint:
`focus_routes_a_keystroke_to_one_terminals_grid_and_not_its_neighbours` puts two display terminals
side by side, types `A` at the focused one, presses TAB, types `B` at the next, and the kernel
compares every pixel of the composed screen against the two VT engines it ran itself. A keystroke
delivered to the wrong client is a wrong picture. Two things came out of it that this note had not
foreseen:

- **The producing side of "who may deliver input" got a real driver.** `user/src/kbd.rs` is a confined
  virtio-input driver holding the ring's mapping and the doorbell, and nothing else. It holds no
  client endpoint and cannot name a client, so it cannot influence focus; and the doorbell it rings
  carries nothing, so the ring's mapping really is the whole of its power to type.
- **A client must not ring the doorbell in response to input.** The compositor is blocked in its
  `CALL` to that client, so a client that answered a keystroke by ringing deadlocks the pair as soon
  as two keystrokes arrive in one drain. It does not need to: this compositor rescans every control
  page on every `COMMIT` from anyone, and the input source rings `COMMIT` itself, so the frame that
  delivers a keystroke is the frame that shows it. That is the blocking-`CALL` cost this note records
  below, met in practice, and the workaround turned out better than the design it ruled out.

## No ambient display, in two dialects

The roadmap's requirement is that window enumeration, screenshots, and screen sharing be **grants, not
defaults**, and that a refusal read as "you hold no such capability" rather than as a permission error.
Both halves are proved, and the refusal turns out to have two forms, which is a pleasing thing to be
able to say:

- **An empty cspace slot.** A client that was not granted an input endpoint has *nothing* in slot 2.
  Its `RECV` there returns `abi::Error::NoSuchSlot` (-1), whose doc comment has said the right thing
  since milestone 7: "The slot is empty. Not permission denied: there is nothing there." The test
  asserts on exactly that value, because `NotPermitted` would mean the authority existed and was
  withheld, which is a different and weaker world.
- **An unmapped address.** A client that was not granted the screen has no mapping where the screen
  would be. Its read faults, the kernel kills it, and the test observes the fault (and, on aarch64,
  the exact faulting address). "There is nothing there" again, in the address space's dialect instead
  of the cspace's.

A capture client shows the other side of the same coin: it holds a **read-only** mapping of the screen
and of the window list, so it can screenshot and enumerate with no server involved and no verb to
call, and its attempt to *write* the screen faults. A thing that may look at the screen may not draw on
it.

**That client is also the screen-sharing case**, not a separate mechanism waiting to be built. It is an
ordinary window client, with its own surface and no special relationship to the compositor, which was
*additionally* granted the screen read-only in its spawn literal: exactly the shape of a screen-sharing
app or a recorder, and exactly the authority such a thing needs and no more. The three items the
roadmap asks for (enumeration, screenshots, screen sharing) are therefore one grant with three uses
rather than three features, which is the point of building on mappings instead of verbs. And because the
grant is a frame mapping, it is revocable through the machinery milestone 13 already built
(`Frame::REVOKE`) rather than by asking a server to stop honouring a request.

## How the isolation is proved rather than asserted

This is the thesis content of the rung, so it is proved from four directions at once, in
`kernel::user::compositor_tests::a_client_holds_no_capability_for_its_neighbours_pixels_or_the_screen`.

The attacker is given every advantage short of a capability:

- it is the **same binary** as the honest client, with the same grants, and it paints its own window
  and reports correctly first (an attack that failed for its own reasons would prove nothing);
- the kernel hands it the **exact virtual address** at which its neighbour's pixels sit, the way
  milestone 29's escape test is handed its victim frame;
- that address is real. Every client maps its surface at the same virtual address, so this is the
  number the neighbour itself uses; and the kernel allocates every client's frames from **one
  contiguous run**, deliberately, so the page just past the attacker's grant genuinely is its
  neighbour's memory. The test asserts that adjacency before believing anything else, because an
  attack on an empty hole would "pass" while proving nothing.

Then:

1. the write **faults** (both ISAs), and on aarch64 the faulting address is exactly the one it was
   handed;
2. the attacker's own report endpoint is **silent** afterwards. It would have sent `WIN_ESCAPED` with
   the value it read back had the access succeeded, so this is the negative half stated as an
   observation (`endpoint_waiting_senders`, the same trick milestone 22 uses to say "and then nothing
   happened" without hanging);
3. the victim's **witness pattern is unchanged**, digested by the kernel through the direct map, from a
   value the kernel computed itself out of the contract;
4. and the victim **re-reads its own surface** and reports the digest again. It is held in a `CALL`
   across the whole attack, so "after" really is after the attacker is dead, and the second witness
   lives in the victim's own address space rather than in the kernel's account of it.

A read fault proves the page is not mapped *at all*, which is why the two probes in this test are a
write (integrity, at a neighbour's pixels) and a read (confidentiality, at the screen): both
directions are exercised against real hardware behaviour rather than one being argued from the other.

## The damage rectangle, proved end to end

Rung one put a damage rectangle in the contract on the argument that a compositor redrawing one window
should not pay for the whole screen. Rung two is where that becomes checkable, and it is checked twice.

On the host, in microseconds: `crates/compose`'s tests poison a screen buffer, composite one window's
damage, and assert that every pixel outside the rectangle still holds the poison while every pixel
inside holds the composed picture.

In the guest, end to end: the kernel plays the display server, so **the flush rectangle is a value it
can compare**. The test poisons the real scanout frames between two frames of one client, and after
the second commit it asserts that the flush was exactly `damage_to_screen(window, SMALL_DAMAGE)`, that
one commit produced exactly one flush, and that the poison **outside** that rectangle survived. A
compositor that quietly repainted the screen every frame would erase the poison and fail, rather than
merely being slow in a way no test could see.

That is also the reason three of the four tests use a kernel stand-in for the display rather than the
GPU: not to save a bring-up (though it does), but because a real driver honours the rectangle and says
nothing about it, so the flush has to be observed somewhere it can be read.

## Proving the picture, and why the host has to be involved

Four witnesses, because a compositor's output is exactly the thing a guest-side digest cannot confirm:

1. **the display driver**, digesting the frames it handed the device after the device reported the
   transfer complete. Its one status report covers the compositor's *startup* frame, which is the
   background alone (no client has committed yet), so it doubles as the check that an empty screen is a
   defined picture rather than whatever was in RAM;
2. **the kernel**, reading the scanout frames through the direct map and comparing every pixel against
   `compose::expected_screen_pixel`, which it computed from the contract;
3. **a capture client in its own address space**, reading the screen through the read-only mapping that
   is its screenshot capability, and digesting it;
4. **the host**, through QEMU's monitor: `cargo xtask` dumps the scanout with `screendump` beside the
   running suite and compares the PPM against the same per-pixel definition.

The fourth is not decoration. `-display none` means nothing in the guest can see the device's own
surface, so a wrong pixel format, a wrong scanout rectangle, or a compositor writing its picture
somewhere other than the scanout would satisfy all three in-guest witnesses and show garbage on a
screen. Milestone 29 built that check; this milestone made it prove **two** pictures over one boot, in
order: the composed screen first (the compositor test holds it up for three seconds so a 250 ms poll
cannot miss it), then rung one's pattern, which stays up until QEMU exits. Both must be seen or the run
fails, and the ordering is part of the check, so a reordering of the suite fails loudly instead of
being waved through.

The composed check has **its own negative control** (`cargo test -p xtask`), because rung two's failure
modes are not rung one's. It must reject a z-order inversion and a missing window, both of which are
pictures made entirely of correct pixels in almost the right places, which is what a compositor
actually gets wrong and what a "is it not black?" checker would happily accept. It must also reject
rung one's pattern, and rung one's checker must reject the composed screen, or the poll loop's ordering
would mean nothing.

## The primitive this rung wants and does not have

The most useful finding of this milestone is a limit, and it shaped everything above.

**A process here has exactly one blocking wait point.** A thread can be parked in one `RECV`; there is
no wait-any and no non-blocking receive (DECISIONS §24 records the same gap from the shell's side), and
two threads cannot share an address space (`Tcb::CONFIGURE` *consumes* the aspace capability, and the
address space dies with the thread). A compositor has three classes of sender: its clients, an input
source, and whoever may read the screen. Distinguishing classes of sender is what endpoints are for,
and one endpoint per class needs one wait point per class.

So the constraint is structural, not stylistic: **a component that must distinguish more than one class
of sender must either be more than one process, or route everything through one endpoint and carry
authority somewhere else.** This design took the second road, and it turned out well (the memory-carried
authority is stronger than a per-class endpoint would have been, and it removed authorization code
rather than adding it). But the road was not chosen freely, and the honest record says so.

What would change if the primitive existed, in either of its two forms (a wait-any / poll on several
endpoints, or threads sharing an address space):

- the compositor could hold **one endpoint per client** and get unforgeable sender identity for free,
  which would let a reply carry per-client data and let a bad damage rectangle be *refused* to its
  author rather than clipped;
- a screenshot could be a served request that copies a **consistent** snapshot into the requester's
  buffer, instead of a live read-only mapping that can be read mid-composite (see the limits below);
- input delivery could stop being a blocking `CALL` into a client, which is today the one place a
  misbehaving client can stall the compositor.

Both forms are real design work with real consequences (a shared address space raises lifetime and
revocation questions; a wait-any primitive widens the §4 syscall surface), so neither is something this
milestone gets to decide. It is recorded in DECISIONS §33 as the fork it is.

## Who is trusted with what, stated exactly

The claim this rung proves is **client-to-client** isolation, and the boundary deserves to be drawn
rather than implied.

**The compositor sees every client's pixels.** It has to: compositing is reading them. So a client's
confidentiality is against *other clients*, not against the compositor, and `compositor` is in every
client's trusted computing base for the contents of its window. That is true of every compositor,
Wayland included, and it is the reason the interesting question was never "can the compositor be
prevented from reading a surface" but "can a client be". What the kernel does buy here is that the
compositor's authority is **enumerated in one spawn literal** and cannot grow: it holds no device, no
interrupt, no DMA authority, no physical address, and no way to name a frame it was not handed. A
compromised compositor can lie about the screen and read the windows it composites; it cannot reach the
disk, the network, another process's memory, or the GPU's command stream (that last one is rung one's
confinement, and it is why the driver is a separate process).

**The display driver sees the composed screen** and nothing else of the clients: it never maps a client
surface. **The kernel is trusted absolutely**, as always here, and in the tests it also plays the roles
a full system would give to separate components (the input driver, and the display server in three of
the four tests), which is worth saying so that "the kernel checked it" is not mistaken for "a
distrusted component was checked".

## What this does not do that a real compositor would

Stated plainly, because a demonstrator's honest limits are part of the deliverable:

- **No window management.** The scene is a compile-time constant (`compose::SCENE`): three windows,
  fixed sizes, fixed positions, fixed stacking order. A real compositor learns its windows from clients
  that ask for surfaces and from a user who moves, resizes, raises, and closes them. Nothing here
  negotiates a surface; the kernel grants three at spawn. That is also what makes the composed screen a
  value a test can predict, which is why rung two is built this way and rung three would have to change
  it.
- **No alpha, no transforms, no scaling.** Windows are opaque and composition is a copy. Blending is
  arithmetic the crate could grow; it would not change any authority question.
- **One damage rectangle per frame, as a bounding box.** Two small changes far apart cost the rectangle
  that contains both. A real compositor keeps a region (a list of rectangles) and pays only for the
  parts. The union is a few extra pixels of copying here and the wrong trade at desktop resolution.
- **Software composition only**, which at 128x64 is nothing and at 4K would be the whole cost. Rung
  four (milestone 34) is where a GPU does this, and this milestone deliberately does not start it.
- **A screenshot can tear.** The capture grant is a live read-only mapping, so a reader that looked
  during a composite would see a half-composed screen. The tests read it at a quiet moment. The fix is
  the served-copy path in the section above, which wants the missing primitive.
- **No defence against denial of service.** A client can spam the doorbell, never answer an input
  `CALL`, or never reply, and the compositor's single thread will slow or stall. Confidentiality and
  integrity are what this rung proves; availability against a hostile client needs the same missing
  primitive plus a policy, and Wayland does not solve it either.
- **No vsync, no frame pacing, no cursor.** There is no display interrupt to wake on (virtio-gpu's
  cursor queue is untouched, as rung one left it), so every frame is driven by a client's commit.
- **The window list is the scene, not live state.** Enumeration returns the fixed geometry plus the
  live focus. With window management would come a list that changes, and the page layout already has
  room for it.

## Where the pieces are

| piece | file |
|---|---|
| the contract and the pixel arithmetic, host-tested | `crates/compose/src/lib.rs` |
| the compositor | `user/src/compositor.rs` |
| a client, with its roles and its attacks | `user/src/window.rs` |
| the wiring (frames, endpoints, grants) | `kernel/src/user.rs` (`compositor_service`) |
| the tests | `kernel/src/user.rs` (`compositor_tests`) |
| the host-side scanout check and its negative control | `xtask/src/main.rs` |
| the display driver it flushes to, unchanged | `user/src/display.rs` |

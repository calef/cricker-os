# 33. The compositor's authority is memory, not messages (milestone 33, the display ladder's rung two)

**Status: DECIDED.**

**Built 2026-07-29**, both ISAs, in QEMU. One screen multiplexed among mutually distrusting clients,
each holding a capability to its own surface: software composition honouring a damage rectangle, input
routed by capability, and no ambient display. Concept note: notes/compositor.md. (Section number chosen
against main at `ab2c2bb`, where §30 is the DMA proof, §31 the C seam, and §32 the reap right. If a
concurrent lane has claimed 33 by merge time, renumber; the content does not depend on it.)

**Rung one's seam held exactly as promised.** The compositor takes `painter`'s place at the display
contract and `display` cannot tell the difference: `gfx_proto` and the driver needed **no change**,
and the only kernel-side addition is a wiring entry point that starts the driver with no client
(`display_service::start_driver`). Three of the four tests replace `display` with the kernel itself
and the compositor does not notice that either, which is milestone 23's swappable-component claim
falling out of a contract rather than being demonstrated on purpose.

**The decision, and it is the one thing to read here: authority is a mapping, not a message.** Every
client rings **one shared doorbell endpoint**, and both verbs on it (`HELLO`, `COMMIT`) are
content-free. A shared endpoint carries no sender identity (§26.5: no badged capabilities), so any
request that *named* a surface, a window, or a rectangle would be forgeable by any client. So nothing in
a message is trusted:

- every per-client fact lives in that client's own **control page** (geometry, id, damage rectangle,
  sequence), which only it and the compositor map. The only surface a client can describe is its own;
- every privileged answer travels through **privileged memory**, never a reply. A screenshot is a
  read-only mapping of the screen; the window list is a read-only page the compositor publishes. There
  is **no enumerate verb and no screenshot verb** to guard;
- keystrokes arrive in an **input ring** shared with the input source alone, so input cannot be
  injected by a client that can only ring the doorbell;
- the reply words carry status only, routed to the caller by the kernel's one-shot Reply (§12), so a
  request is answered without the compositor learning who asked.

The consequence is the point: **the compositor contains no authorization code at all.** It never asks
"may you?", because there is no request that would need the question, and it cannot leak the screen to a
client that asks because handing over the screen is not an operation it has. That is the difference in
kind from Wayland, which attaches client identity at the transport and then decides in code; its
security properties are properties of that code. Wayland's model approximates capability routing; this
is capability routing.

**No new syscall, no new method, no widened surface (§4).** The whole rung is endpoints, shared frames,
and `Spawn` grants that already existed. The one kernel-resource change is a constant: `KERNEL_EP_PAGES`
128 → 160, the third bump of a number whose comment has always said it grows with the suite. Recorded
with the standing suggestion it repeats: next time, reap the harness's boot services instead, which is
its own piece of work because endpoint teardown does not exist (§13 pins a region hosting an endpoint).

**The isolation is proved, not asserted, and the attacker is given every advantage short of a
capability.** It is the same binary as an honest client with the same grants, it paints its own window
correctly first, and the kernel hands it the **exact virtual address** of its neighbour's pixels. That
address is real twice over: every client maps its surface at the same virtual address (so it is the
number the neighbour itself uses), and the kernel allocates all the clients' frames as **one contiguous
run** so the page past a client's grant genuinely is its neighbour's memory, which the test asserts
before believing anything else. Then: the write faults (both ISAs, exact address checked on aarch64,
which is the ISA that records one); the attacker's report endpoint stays silent, so the "I read it back"
message it would otherwise send did not happen; the victim's witness pattern digests identically before
and after through the kernel's direct map; and the victim, held in a `CALL` across the whole attack,
re-reads its own surface afterwards and reports the same digest from its own address space.

**No ambient display, and the refusal has two dialects.** A client not granted an input endpoint has an
*empty cspace slot*: `NoSuchSlot` (-1), "there is nothing there", asserted by value because
`NotPermitted` would describe a weaker world. A client not granted the screen has *no mapping* where the
screen would be: its read faults. Same sentence, one in the cspace and one in the address space. A
capture client holds the screen and the window list **read-only**, so it can screenshot and enumerate
with no server involved, and its attempt to write the screen faults: a thing that may look at the screen
may not draw on it. Screen sharing is that grant aimed at a third party, and being a frame mapping it is
revocable through §13.

**The boundary this rung proves is client-to-client, and that is stated rather than implied.** The
compositor sees every client's pixels because compositing is reading them, so `compositor` is in every
client's TCB for the contents of its own window, exactly as a Wayland compositor is. The question was
never whether a compositor could be prevented from reading a surface; it was whether a *client* could
be. What the capability model buys is that the compositor's authority is enumerated in one spawn literal
and cannot grow: no device, no interrupt, no DMA authority, no physical address, no way to name a frame
it was not handed. A compromised compositor can lie about the screen and read the windows it
composites, and cannot reach the disk, the network, another process, or the GPU's command stream (that
last one being rung one's confinement, and the reason the driver is a separate process).

**Damage is honoured, and that is observed rather than claimed.** The kernel plays the display server in
three tests precisely so the flush rectangle is a value it can compare: one commit produces one flush,
the flush is exactly the client's rectangle placed on the screen, and the poison the kernel wrote over
the rest of the scanout between two frames is **still there** afterwards. The same property is checked on
the host in microseconds by `crates/compositor`.

**The picture is proved by four witnesses, one of which has to be the host.** The driver's digest of the
frames the device read (the compositor's startup frame, which is the background alone, so an empty screen
is a defined picture); the kernel's own pixel-for-pixel comparison through the direct map; a capture
client's digest taken in a third address space; and QEMU's `screendump` compared against the same
per-pixel definition. The fourth is not decoration: `-display none` means no in-guest witness can see the
device's own surface, so a wrong format or scanout rectangle would satisfy all three and show garbage.
Milestone 29's checker now proves **two** pictures over one boot in order (composed screen, then rung
one's pattern), both must be seen, and the composed check has its own negative control because rung two's
failure modes are not rung one's: it must reject a z-order inversion and a missing window, pictures made
entirely of correct pixels in almost the right places.

**The open fork, and it is the most useful finding of the milestone: this kernel has no wait-any.** A
process has exactly one blocking wait point (a thread parks in one `RECV`; there is no non-blocking
receive, and two threads cannot share an address space because `Tcb::CONFIGURE` consumes the aspace
capability and the space dies with the thread). A compositor has three classes of sender (clients, an
input source, a screen reader), and distinguishing classes of sender is what endpoints are for, so one
endpoint per class needs one wait point per class. **The constraint is structural: a component that must
distinguish more than one class of sender must be more than one process, or carry authority somewhere
other than its messages.** This rung took the second road and it turned out stronger than the first
would have been. But if the primitive existed, a compositor could hold one endpoint per client and get
unforgeable identity for free (letting a bad damage rectangle be *refused to its author* rather than
clipped), a screenshot could be a served consistent snapshot rather than a live read-only mapping that
can tear, and input delivery would stop being a blocking `CALL` into a client. Both candidate forms are
real work with real consequences (a shared address space raises lifetime and revocation questions; a
wait-any widens §4), so **this is calef's call, not a thing to build quietly.** notes/compositor.md
carries the full argument.

**Honest limits, recorded because a demonstrator's caveats are part of the deliverable.** The scene is a
compile-time constant: three windows, fixed sizes, positions and stacking order, no surface negotiation,
no move, resize, raise or close. That is what makes the composed screen a value a test can predict, and
it is also the thing rung three would have to change. No alpha, no scaling. One damage rectangle per
frame as a bounding box rather than a region list. Software composition only, which at 128x64 is nothing
and at 4K would be the whole cost (rung four, milestone 34, and deliberately not started). A screenshot
can tear. And **no defence against denial of service**: a client can spam the doorbell or refuse to
answer an input `CALL` and slow or stall the compositor's single thread. Confidentiality and integrity
are what this rung proves; availability wants the missing primitive and a policy, and Wayland does not
solve it either.

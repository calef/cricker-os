# Frame capabilities: shared memory a process owns

DECISIONS §10 has a one-line rule for the data path: **IPC carries control, shared memory carries
data.** The endpoint moves the small stuff (a length, a request code) and the bulk bytes live in a
page both parties can see, so the kernel never copies them. For a long time cricker-os honored that
rule only by accident of setup: the kernel allocated the shared page and mapped it into both the
console client and server at spawn, and both sides just found it at a fixed virtual address they had
agreed on in advance. The sharing was real but frozen. Two processes could share memory only if the
kernel decided, at the moment it created them, that they should.

A `Frame` capability makes shared memory a thing processes *do* instead of a thing the kernel
*pre-arranges*. This note is that object.

## What a Frame is

A capability whose object is a single physical page. Its address is its identity: `Object::Frame(pa)`
names the page at `pa`, and a process can never forge one, because the only ways to hold a `Frame`
are to retype it out of your own untyped or be handed it by someone who has it, and both keep the
object intact. Its rights say what you may do with the page: `READ` to map it read-only, `WRITE` to
map it read/write, `GRANT` to pass it on.

## Retype, then map: two operations, not one

seL4 splits "get a page" from "put it in your address space," and so do we, because the split is what
makes a page a first-class, delegatable object rather than something that only exists mapped.

- `Untyped::RETYPE` carves one page out of the caller's untyped and mints a `Frame` capability for
  it, full rights, into the caller's cspace. Nothing is mapped. The caller now *holds a page* and
  can map it, or delegate it, or delegate it and never map it.
- `Frame::MAP(va, writable, untyped_slot)` maps the page at `va`. A read/write mapping needs `WRITE`
  on the frame; a read-only one needs `READ`. The page tables to reach `va` are drawn from the
  untyped named by `untyped_slot`, so like everything a process spends, mapping a frame comes out of
  its own budget and the **kernel allocates nothing**.

Contrast `Untyped::MAP`, which does both at once (retype a page and map it writable). That is the
convenient path for a process's private memory. `RETYPE` + `MAP` is the path when the page is going
to be shared, because between the two steps is where the delegation happens.

## Sharing is delegation applied to memory

Because a `Frame` is an ordinary capability, it travels over an endpoint with `SEND_CAP`, and the
rights narrow on the way exactly as they do for any delegation (see [delegation.md](delegation.md)).
So the whole sharing protocol is:

1. Producer `RETYPE`s a frame, `MAP`s it read/write, writes into it.
2. Producer `SEND_CAP`s the frame to the consumer, narrowed to `READ` (dropping `WRITE` and `GRANT`).
3. Consumer `RECV_CAP`s it, `MAP`s the *same physical page* read-only, and reads what the producer
   wrote.

The kernel copied nothing and was never told these two processes would share memory. They built the
sharing themselves out of a capability, and the read-only narrowing means the consumer can look and
not touch. A peer handed `READ` alone gets `NotPermitted` if it asks to map the page writable, which
the test checks by trying.

## The lifetime question, and why there is no double-free

A page shared into two address spaces cannot be owned by either, or the first one to die frees memory
the other is still using. cricker-os sidesteps this cleanly because of how teardown already works: an
`AddressSpace` frees only the frames it recorded at spawn (`self.frames`) plus its page tables and
root. A page mapped at *runtime*, by `Untyped::MAP` or `Frame::MAP`, is never in that list, so
teardown does not free it. A frame's page (and the page tables that map it) belong to the untyped
region they came from, and are reclaimed only when that region is destroyed, wholesale, the way
untyped memory always is. So when the producer exits, its mapping of the shared page simply goes away
with its address space; the physical page persists, and the consumer's mapping is still good. No
refcount, no double-free, because address spaces borrow frames and never own them.

The honest limit: individual frames are not reclaimed on their own, only with their whole untyped
region. That is the same bounded, deliberate gap untyped memory already has, and closing it is the
same parked problem: capability revocation.

## The synchronization edge is the IPC rendezvous

On ARM's weak memory model, the producer's write is not automatically visible to the consumer just
because it happened first in time. What makes it visible is that the delegation is a *rendezvous*:
the producer's `SEND_CAP` releases the scheduler lock and the consumer's `RECV_CAP` acquires it, and
that release/acquire pair is the happens-before edge. The write lands before the send, the send
synchronizes with the receive, the read comes after. So the same IPC that carries the capability also
orders the memory, which is a tidy demonstration of why "control travels by IPC" and "data travels by
shared memory" fit together rather than being two unrelated rules.

## What the test proves

`a_frame_capability_shares_a_page_and_a_read_only_view_cannot_write_it` runs the protocol above with
two user processes and checks two things: the consumer reads the producer's sentinel through its own
mapping (the page is genuinely shared), and a writable mapping of the read-only view is refused (the
rights confine it). The sharing half is self-verifying in a nice way: `RETYPE` hands back a *zeroed*
page, so if the consumer had somehow mapped a different page instead of the shared one, it would read
zero, not the sentinel. Reading the sentinel can only mean it mapped the producer's page. And verified
it can fail: stub the `WRITE` check in `Frame::MAP` and the read-only view becomes writable, so the
confinement assertion trips.

## What this unlocks, and what is left

The static shared-buffer wiring in the console and virtio paths (a page the kernel maps into both
sides at spawn) is now a special case of something general: a `Frame` one side holds and delegates.
Rewiring those drivers to use frame capabilities instead of spawn-time mappings is the follow-on
refactor, not done here. This note builds the object and proves it; migrating the existing users to
it is separate work.

# 91. A region's endpoints are swept before its refusal, not after

**Status: DECIDED.** Minted by the integrator at merge (2026-08-16), which is where a section
global to the tree is assigned. The change is milestone-level rather than architectural: it
reorders two phases inside an existing verb and adds no method, no object and no syscall.
Recorded here because §16's revocation semantics are stated here, and a reader who learns that
reclamation refuses a busy region deserves to learn in the same place why it stopped refusing
forever.

## What changed

`sched::reap_region_objects` sweeps a region's **endpoints first, on every pass, refusal or
not.** It used to sweep them after the refusal check, and that ordering made one shape
permanently unreclaimable.

## The shape it could not reclaim, and why

§16's revocation arms a kill when it refuses a busy region: the resident dies at its next trip
through `schedule()`, and the next pass succeeds. That works for a thread that runs. It does not
work for a **server parked in `RECV`**, because a `Blocked` thread never reaches `schedule()`, so
it never spends the armed kill, so the region is refused on every pass forever. The test boot
builds exactly such a server six times over: `userspace_init_brings_up_the_console_server`, out of
init's own budget.

**Sweeping the endpoints first fixes it with a wake that was already written.** Removing an
endpoint drains its wait queue, which is precisely what a blocked resident needs in order to
become schedulable and spend the kill the refusal arms one paragraph later. Nothing new had to be
built; the two phases were in the wrong order.

The semantics a reader should carry away: **a server whose endpoints came out of the region being
destroyed dies with it; one parked on somebody else's endpoint does not.** That is the same
ownership rule §16 already states for memory, applied to the thing that keeps a thread blocked.

## Why this is worth a section rather than a comment

Because the failure was invisible and expensive. The region was not leaked in any way a reader
could see: the refusal was correct at each individual call, and only the *repetition* was wrong.
The cost showed up two layers away as a boot that ran out of contiguous frames and blamed
whichever test happened to spawn last, which misled three milestones before it was measured
(notes/frames.md and notes/net.md carry the receipts, and the measurement is the durable half:
free frames at boot's end went from 216 to 15,305, longest contiguous run from 117 to 14,080).

## BUGS

- **This does not reclaim everything, and the remainder is measured rather than assumed.** A
  `SPLIT` parent whose children's owners are dead can never be reclaimed (2,146 frames in the
  test boot), because that would need a capability derivation tree, which this kernel
  deliberately does not have (§16 records that choice). Several services still hold their DMA
  page and shadow ring on purpose, since freeing those needs a device reset through the transport
  seam. The per-holder list is in notes/frames.md.

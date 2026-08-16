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

## What everyone else does, and where this diverges

**Approved by calef on 2026-08-16 after asking for the alternatives and the prior art**, which is
the useful half of this section: destroying a communication object and waking whoever is blocked
on it is the mainstream answer rather than an invention here.

- **seL4**, the closest relative, does exactly this. Deleting the last capability to an endpoint
  runs `finaliseCap`, which calls `cancelAllIPC` and restarts every thread blocked on it with a
  failed IPC. Waking the waiters is *part of* endpoint destruction by design.
- **Zircon** closes a channel's peer and raises `PEER_CLOSED`; blocked waiters wake with an error.
- **Mach** invalidates in-flight and queued sends when a port dies (`MACH_SEND_INVALID_DEST`).
- **Unix** says the same thing in its own idiom: closing a pipe's write end wakes its readers with
  EOF. Its exception is uninterruptible `D`-state sleep, where a thread genuinely cannot be woken,
  and that is treated everywhere as a wart rather than a design. **The behaviour this section
  replaces was closer to the wart than to the rule.**

**Where nife diverges, and it is worth knowing:** seL4 triggers the cancellation at *last
capability* finalisation, which it can ask because it has a capability derivation tree. This
kernel deliberately does not have one (§16 records that choice), so the trigger here is
**ownership by region**: the endpoint's backing frame lies inside the region being destroyed,
therefore it dies with it. Same wake, a different question asked, and it is the same ownership
rule `DESTROY` already applies to the memory itself.

**The alternatives, and why they lost.** A second in-kernel entry point that swept early and left
`DESTROY` alone was the lane's own fallback; it costs a second teardown path beside the first, and
a pair like that drifts. Killing the blocked thread directly, without touching the endpoint, would
need surgery on the one intrusive wait-queue link, which is the invariant the undelivered-wake
work spent a week on. And making the refusal merely *informative*, so a caller destroys the
endpoint first, moves a kernel invariant into every caller, which is the silent-degradation shape
§42 exists to refuse.

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

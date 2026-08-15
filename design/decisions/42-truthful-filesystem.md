# 42. A filesystem declares what it offers and must be truthful; it is not required to be capable

**Status: DECIDED.**

**Decided 2026-07-30, not yet built.** The rule that governs every filesystem backend behind the
§27 contract, arrived at by calef over two corrections of mine. Milestone 47 (navigation and naming)
owns `mv`; this is the contract rule underneath it.

## The constraint that started it

Behaviour must not be filesystem-specific. We expect to change backends over time and to run several
at once, so an operation that means one thing on RedoxFS and another on the next backend is a
compatibility mess that arrives slowly and is discovered in production. §27 already implies this: the
FS service is "a capability-shaped contract over a component we did not write", and *capability*-shaped
is not *RedoxFS*-shaped. Letting `mv` inherit whatever the backend happens to do would violate an
established decision, not merely be untidy.

## The split

Two different things get conflated when someone says a filesystem "can't do that".

- **Rights say what is offered.** A read-only mount offers no `remove` and no `create`. That is not
  variation in behaviour, it is an honest absence, and milestone 47's directory rights ladder
  (`enumerate` / `open` / `create` / `remove`) already expresses it.
- **Guarantees say how an offered operation behaves.** If a backend offers a verb, that verb means
  exactly one thing, with one atomicity story, on every mount.

**Behaviour never varies. Availability may.** Uniformity was never "every mount offers everything";
it is "any operation you can perform behaves the same everywhere", which is why nobody finds a
read-only mount troubling.

## The two atomicities, which POSIX taught everyone to conflate

Stated separately in the contract, because saying "atomic" and letting readers assume the stronger one
is the mistake POSIX made.

- **Concurrency atomicity**: no concurrent observer sees an intermediate state. POSIX *mandates* this
  for `rename(2)` within a filesystem, cross-directory included. Near-universal on anything with
  directories, and cheap to conform to.
- **Crash atomicity**: after power loss you see old or new, never garbage. **POSIX requires nothing.**
  This is where a decade of data-loss bugs lived, ext4's 2009 delayed-allocation saga being the
  cautionary tale: the rename hit the journal before the data blocks landed, so a crash left a
  correctly-named empty file, and the fix was a heuristic rather than a guarantee.

The temp-file-then-rename idiom, which is how applications get crash-safe updates, needs **both**.
`fs_proto` has no `RENAME` at all today and `rename` is `Unsupported` in the std PAL, so this is a
missing verb rather than a refinement.

## The rule: truthfulness is the gate, not capability

**A backend that cannot meet a verb's guarantee does not offer that verb.** It still mounts, and it is
still useful. FAT offers read, write, create and remove but not `rename`; an object store offers no
`rename` because it has no directories at all; the read-only initrd (crickerfs) offers neither
`remove` nor `create`. All three fail `mv` on **authority**, uniformly, and none of them is excluded
from the system.

What mount-time conformance is for is **honesty**: a backend declares what it offers and the
conformance suite verifies the declaration. A filesystem *claiming* atomic rename that fails the suite
does not mount, because it is lying. This is the architecture-parity gate's discipline (rule 5) on a
new axis: a capability ships everywhere it claims to, proven by one suite, or the gap is recorded.

**Rejected: requiring crash-atomic rename for mountability.** This was my position and it is wrong.
An operating system exists to be useful, and it has to work with impure components; refusing to mount
a FAT SD card to protect a guarantee nobody asked for on that mount trades real utility for purity.
The concrete cost is not hypothetical: the VisionFive 2 boots via U-Boot from an SD card whose
firmware partition is conventionally FAT, so the purity rule would have blocked milestone 16a on a
technicality. It also mis-applies the split above, gating on mountability when the split says to gate
on the verb.

## The danger was never the impure backend; it is the silent fallback

Unix's `rename()` on FAT **succeeds** and simply is not crash-safe, and the caller cannot tell. That
is the actual defect: not that FAT is weak, but that the interface lies about it. So the operative
rule is **no silent degradation**. A verb that is not offered fails loudly and the application
decides; it may choose copy-then-truncate with its eyes open. What it must never receive is an unsafe
operation wearing a safe operation's name.

## Discovery costs nothing new

Availability varying would be useless if it were not discoverable, and **the rights are the discovery
mechanism**: a directory capability carries `rename` or it does not, and rights are already
introspectable, which is what `caps` prints. No feature-query verb, no capability negotiation.

## What the contract requires, and what it deliberately does not

- **Require** concurrency-atomic rename, same and cross directory, for any backend offering the verb.
  POSIX mandates it and every local filesystem delivers it, so requiring it costs nothing real. An
  earlier draft of mine proposed levelling cross-directory down to non-atomic everywhere; that
  discards something near-universal in exchange for portability that would never be cashed in.
- **Require** crash atomicity for the verb, and expect this to be the demanding clause. It is what
  FAT cannot do and what ext4 only approximates.
- **Do not require** the extended operations. Linux's `renameat2` flags (`RENAME_EXCHANGE`,
  `RENAME_NOREPLACE`) work on ext4, btrfs, XFS, f2fs and tmpfs and nowhere else, and emulating
  `NOREPLACE` with `link`+`unlink` is racy. Add a swap primitive later if a use appears.

**Cross-filesystem move is a different verb, and this constraint is what forces it.** No amount of
contract discipline can make an operation spanning two filesystems behave like one inside a single
filesystem: it is copy-then-unlink, a different object with a different identity, non-atomic by
nature. If `mv` silently became that operation depending on where its arguments happened to live,
behaviour *would* be filesystem-specific, which is the thing this entry exists to forbid. Unix hides
that seam behind one command, which is why a `mv` across a mount point can leave a partial file.

## Caveat worth carrying

The survey behind the atomicity claims above is from knowledge, not measurement: the `renameat2`
support matrix, current APFS specifics, and the fact that "journaled metadata" hides real per-mount
variation (ext4's `data=ordered` versus `writeback`) should each be verified before being quoted
outward. The conclusion does not rest on the details, but a published claim would.

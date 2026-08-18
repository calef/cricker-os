# 135. The region claim, under loom

**Status: BUILT.** Raised and closed 2026-08-18, out of the double-free fix (pull request #316),
which closed the bug and said plainly which half of it was not gated. The gate is
`script/interleaving-check`, which now covers `crates/regions` and searches 1,364 executions of the
claim across five harnesses. **Verified it can fail**, twice and in two different places: deleting
the slot removal from `claim_for_destroy` fails three of the five, and moving the parent's child
count decrement to claim time fails exactly the one harness written for it. A third piece of
evidence is permanent rather than a demonstration, and is the part worth carrying forward: one
harness reconstructs the pre-fix protocol and **passes only when loom finds its double free**.

## In brief

`untyped::destroy` decides whether a region may be reclaimed and then reclaims it. Before #316 those
were two separate hold of the `REGIONS` lock with a gap between them, and two callers holding a name
for one region could each pass the refusal check inside that gap and each run the free loop over the
same pages. #316 closed it by removing the slot under the same hold that decided to destroy it, so
the generation bump precedes every free and the loser's name no longer resolves.

**That fix is argued from lock discipline and nothing gates it.** The literal double free needs two
`destroy` calls to overlap inside a window of a few instructions, and no test in the kernel suite can
schedule that; #316 said so in the new `BUGS` section of notes/object-revocation.md. This milestone
closes it the way milestone 80 closed the same class three times before: lift the protocol into a
host-testable crate and let loom search every interleaving.

The work, in three pieces, all of them built:

- **Lift the region table out of `kernel/src/untyped.rs` into `crates/regions`**, beside the
  `destroy_outcome` arithmetic Kani already proves. The kernel keeps the I/O (the frame allocator,
  the zeroing, the revoke) and the lock; the crate owns the table and every decision taken over it.
  The claim becomes one `&mut self` method, which is rung one of the ladder: a caller cannot
  express check-then-release-then-remove, because there is no released state to express it in.
- **Model it under loom.** Two destroyers racing for one region, a destroyer against a retype, a
  destroyer against a split, and the child's return of pages to its parent, each searched over every
  interleaving and every reordering C11 permits.
- **Wire it into `script/interleaving-check`**, so the property is gated rather than demonstrated
  once. That script covers four crates today and none of the memory-reclamation path, which is the
  gap that makes this worth a milestone rather than a comment.

## Why it matters

**A double free in the reclamation path is the worst failure this kernel has.** It hands one physical
page to two owners, arbitrarily far from the code that made the mistake, and the symptom is memory
corruption in whichever subsystem touches it next. #316's instance surfaced once in 45 loaded runs on
riscv64 and needed two cores to reproduce, which is the honest shape of the whole class: rare enough
to look like a flake and severe enough that a flake is the wrong word for it.

**The single-winner claim is the property the whole reclamation path rests on**, and it is exactly the
kind of property a test cannot reach. `destroy_outcome` is proved by Kani, which is a statement about
one caller's arithmetic; the racing case is a statement about two, and Kani does not model threads.
The kernel suite runs the code but cannot choose when the second caller arrives. Loom chooses for it,
and chooses every time.

**The customer path runs this code on every teardown.** A file service that spawns and reaps a
process per connection destroys a region per connection, and #316's bug was found by a force-kill
test. This is not a corner of the kernel that a backup server avoids.

**And it extends milestone 80's method to the subsystem where it pays most.** The three protocols
already modelled are a work-steal handshake, a seqlock and a corruption canary; two of the three had
real bugs in them, and the one place we already know a concurrency bug lived is not covered. That is
the wrong distribution.

## Prior art

seL4's answer to the same question is a **capability derivation tree** and a revoke that walks it,
verified in Isabelle/HOL against an abstract specification that includes the concurrent case only by
excluding it: seL4 is a single-kernel-lock design, so its proof never has to answer what two
concurrent `Untyped_Delete` calls do. That is a legitimate choice and it is not ours; nife runs the
kernel on every core (DECISIONS §28) and pays for it here. The design to copy is the *shape* of the
claim (a name that stops resolving the instant the decision is taken), not the mechanism.

Loom itself is tokio's, and the retrofit pattern is milestone 80's, established over three crates.
Nothing here is new tooling; the work is which protocol gets the treatment.

## Scope note

No syscall surface, no wire format, no new dependency (loom is already `cfg(loom)`-gated in four
crates and never enters the shipping graph). It moves logic between a kernel module and a crate that
already exists, and adds harnesses to a script that already exists.

## BUGS

- **Loom models C11, not ARM and not RISC-V.** A bug it finds is real; a clean run is not a proof
  about the silicon. This is milestone 80's standing caveat and it applies unchanged; milestone 81's
  Hypervisor.framework leg is the complementary evidence, and it samples rather than searches.
- **The model checks the claim, not the free loop.** What loom searches is who wins the right to
  reclaim a region. That the winner then frees the right pages, exactly once, is `destroy_outcome`'s
  Kani proof plus the kernel's own tests, and the two arguments meet only in the reader's head. A
  model that covered both would need the frame allocator lifted too, which is a much larger crate
  than this milestone should mint.
- **The kernel's real lock is not the lock loom searches.** `IrqSafeMutex` masks interrupts and
  carries a rank for the deadlock order; the model uses `loom::sync::Mutex`, which has neither. So
  the model says the protocol is correct *given* mutual exclusion, and says nothing about whether
  `IrqSafeMutex` provides it or whether the rank order is right. `script/lint`'s rank check and
  notes/locking.md are the separate arguments for those.
- **Nothing gates the crate against the kernel drifting back.** *Closed by milestone 136, on the
  same day, and the mechanism named here was the wrong one.* Milestone 113's shim shape does not
  transfer, because this milestone already got its whole benefit for one flag: loom is an ordinary
  `cfg(loom)` dependency where `kani` is not resolvable at all, so `script/interleaving-check`'s
  `RUSTFLAGS="--cfg loom -D warnings"` is the shim's equivalent and needs no shim. The property
  actually at risk was a different one that no lint over harness code could reach, and 136 found
  that **the gap can be rebuilt in `untyped.rs` from `has_children` and `bounds` alone**, with no
  edit to this crate. See design/roadmap/136-one-decision-path.md.

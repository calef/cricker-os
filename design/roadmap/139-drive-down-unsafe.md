# 139. Drive the unsafe count down, and cinch the ratchet behind it

**Status: NOT-STARTED.** Minted 2026-08-18 by calef, immediately after folding the unsafe census into
milestone 134: *"Can we also create a milestone to drive down the unsafe metrics and cinch up the
ratchet?"*

**Gate: MILESTONE 134.** 134 builds the instrument, which is the census and the ceiling relation in
`script/lint`. This milestone spends it. Starting before the ceiling exists means driving a number
nobody is holding, which is the state this project has been in since the first `unsafe` block.

**In brief.** Reduce the hand-written `unsafe` this tree carries outside `kernel/src/arch/`, and lower
the ceiling after each reduction so the ground gained cannot be given back quietly.

## The measurement it starts from

Taken on `main` 2026-08-18 and to be re-taken by 134 rather than trusted here:

| | count |
|---|---|
| `unsafe { }` blocks, ours, `vendor/` excluded | 893 |
| `unsafe fn` | 53 |
| `unsafe impl` | 28 |
| `// SAFETY:` comments | 885 |

By location: `kernel/src/arch/` 139, kernel outside `arch/` 203, `user/` 285, `ipc` 44, `user_heap`
41, `user_rt` 31.

**`arch/` is not a target and this block will not accept a reduction there.** Rule 1 says
architecture-specific code lives under `arch/`, and unsafe is what architecture is made of. Driving
that number down means either writing the assembly wrong or moving it somewhere it does not belong,
and both are worse than the number.

## What counts as a reduction, which is the whole design of this block

**The metric is gameable and the obvious way to game it is invisible.** Moving three `unsafe` blocks
into one helper function reduces the count by two and reduces the risk by nothing: the same
invariants are asserted by the same code in a different place. A milestone that rewarded that would
make the tree worse while the graph went the right way, and the graph would be the reason.

**So the test is not the token count, it is the number of distinct invariants asserted by hand.**

**§94 is the worked example and it is what a real reduction looks like.** The trap instruction was
inlined at **48 sites in 7 variants** across 58 panic handlers, each one a hand-written assertion of
the same invariant. Lifting it into `user_rt::trap()` left one. That is 47 chances to write it
differently, removed. §94 states the general form: *a per-binary item whose body is copied verbatim
into every binary is not per-binary; only its declaration is,* and copying it is asserting the same
invariant N times by hand, which §61 says a `// SAFETY:` comment must never be.

**And §94 found the cost of the copy by counting the copies**: one of the 58 handlers was different.
`terminal_sink_caretaker` called `exit()` and never trapped, so a panic there reported `EVENT_EXIT`
where every other program reports `EVENT_FAULT`. **A supervisor would have been told a panicking
program finished cleanly.** That is the argument for this milestone in one incident: the duplication
was not only ugly, it was already wrong in one place and nobody knew.

So a reduction qualifies when it does one of these:

- **Collapses N hand-written assertions of one invariant into one**, the §94 shape. Best available,
  and the only one that reliably reduces risk rather than moving it.
- **Replaces raw pointer arithmetic with a typed abstraction** whose invariant the compiler or Kani
  holds, so the assertion stops being a comment. Rung one on the ladder.
- **Deletes unsafe that was never needed**, which is the cheapest and rarest.

It does not qualify when it merely relocates unsafe, wraps it in a function whose safety argument is
the same argument, or hides it behind a macro.

## The ratchet

After each reduction, **lower the ceiling to the new count in the same commit.** A ceiling left above
the true number is not a ratchet, it is a budget, and a budget gets spent by whoever arrives next
without knowing it was won.

## Why it matters

**The verified-Rust claim in this project's own thesis is measured here.** DECISIONS §14 calls this a
verified-Rust capability microkernel; `unsafe` is where that claim is suspended, and 203 suspensions
outside the architecture layer is the honest size of the gap. Every one of them is a place where the
proofs and the type system are standing aside and a person's comment is the whole argument.

## BUGS

- **This block sets no target number and should not, until 134 measures whether the ceiling fires on
  honest work.** `script/lint` has already had three checks deleted for the signature "only ever
  rejects legitimate work", and a ceiling cinched past what the tree can sustain would be the fourth.
  The first lane should report what a realistic floor looks like rather than picking one here.
- **`user/`'s 285 is unexplained.** A userspace program in a capability system arguably needs little
  unsafe, and nobody has read enough of it to say whether that number is raw shared-page handling, a
  missing safe wrapper, or something else. That reading is the first lane's cheapest useful output
  and it may change what this milestone does.
- **A reduction can be real and still not show in the count.** Proving an existing `unsafe` block's
  invariant with Kani leaves the block where it is and makes it safer; the metric cannot see that.
  Do not let the number decide which work is worth doing.

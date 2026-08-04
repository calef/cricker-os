# 35. What a scanner is for here, and how its findings get dispositioned

**Status: DECIDED.**

**Decided 2026-07-30 (milestone 45), the first time code scanning actually ran.** CodeQL found nine
things, and **all nine are fixed**: seven CI jobs holding a `GITHUB_TOKEN` with permissions they never
used, and two `rust/access-invalid-pointer` in `crates/intrusive` that moving the queue API to
`NonNull` cleared outright: `/language:rust` reports **2 results on `refs/heads/main`** and **0 on
`refs/pull/5/head`**, holding across four commits on each side, the oldest zero being the NonNull
commit itself.

**The evidence path is worth recording, because the first one was invalid.** I originally checked
`?ref=refs/heads/<branch>` and read the zero it returned as "cleared". CodeQL does not store a PR's
analyses under the branch ref: that ref has **zero analyses**, so the query would have returned zero
whatever the code did. A right answer from a query that could not have produced a wrong one is not
evidence, and this section is the wrong place to be sloppy about that. The controlled comparison
above is the real result.

The policy below is recorded anyway, and deliberately, because the *next* finding will not be so tidy
and the question milestone 44 left open is still open: what happens to a finding we do not intend to
change code for.

**A prediction worth recording because it was wrong.** I twice said the `NonNull` change would
probably improve the code *without* satisfying CodeQL, reasoning that the rule is about pointer
validity in general rather than nullness specifically, and that the honest outcome would be a written
dismissal. It cleared both alerts; the rule was more precise than I credited. The lesson is not "trust
the scanner", it is that a hedge stated confidently is still a guess, and this one cost nothing only
because the fix was worth making on its own merits.

## The rule

**Every alert gets a disposition, and a dismissal is a written argument, not a click.** An alert list
nobody triages decays into wallpaper, and then the scanner is worse than nothing: it manufactures the
appearance of review. Three dispositions, and only three:

1. **Fixed.** The code changes. Default for anything where the fix is real.
2. **Dismissed with a reason**, recorded where the *code* is, not only in GitHub's UI. GitHub's
   dismissal comment is fine as the audit trail; it is not fine as the only copy, because it is
   invisible to anyone reading the source and it does not survive a change of tool.
3. **Deferred to a milestone.** For a finding worth fixing that is bigger than the alert.

## The distinction that made this concrete

The two `intrusive` alerts look like one finding and are two, and separating them is what made the
milestone tractable:

- **Nullness was structurally fixable, and the type was failing to say so.** Every pointer entering
  the queues comes from `tcb_ptr`, which derives it from a `&mut Thread` the thread table hands out,
  or from a `&mut Thread` directly. Non-nullness is a fact of construction, not a promise the caller
  keeps. So the API moved to `NonNull<T>`, and **every conversion at every call site is infallible**;
  nothing was relocated into an `unwrap`, which is the move that would have made this cosmetic.
- **Validity and aliasing are not structurally fixable**, and that is the design rather than a gap. An
  intrusive queue borrows nodes it does not own with no lifetime the borrow checker can see. That is
  the entire reason it exists (no allocation, no lookup, a pop hands back the object), and the price
  is stated in the crate docs as a three-rule caller contract. **No type available to us can carry
  rule 2**, "a node outlives its time on the queue", for a structure whose whole purpose is that the
  queue does not own its nodes.

## What actually upholds the half no tool covers

This is no longer a dismissal justification, since nothing was dismissed. It stands as the queue's
standing caveat, which is the more useful role: the kernel's own state
machine. A thread is on exactly one run queue or inbox, or blocked on one endpoint queue, or running,
and never two at once, because there is only one link inside it. Only `Finished` threads are ever
freed, and a `Finished` thread is on no queue. All access is serialized (a run queue is single-core
with interrupts masked; an inbox is behind its mutex; endpoint queues are under `SCHED`). Those are
the three rules, and they are enforced by the scheduler's structure and the lock ranking of §9, not by
the type system.

## The honest limit, which is the point of writing this down

**Zero alerts is not a proof of safety.** The queue is safe because
the scheduler uses it correctly, and nothing in `crates/intrusive` can check that. A future caller
that violates rule 2 gets a use-after-free that neither CodeQL nor Kani would catch: Kani proves the
queue's *logic* over a symbolic operation sequence with nodes it holds valid by construction, so it
answers "is the FIFO correct" and never "did a caller free a queued node". That gap between the two
tools is real and worth naming rather than papering over with a green checkmark on both.

## Rejected

- **Suppressing the rule crate-wide.** It would also silence a genuine future null or dangling
  dereference in the same file, which is the one place we most want to hear about one.
- **Restructuring to satisfy the tool.** An owning queue would reintroduce allocation on the IPC path,
  which is what milestone 14 removed and what `VecDeque` cost us. Chasing a scanner into a worse
  design is the failure mode this section exists to prevent.

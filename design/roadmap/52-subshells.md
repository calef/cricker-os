# 52. Subshells without `fork`, and what copying an endowment means

**Status: RECORDED.**

**Gate: DECISION.** Recorded and deliberately not designed: calef asked to design this one
together, and the block says not to build from it without that conversation. It was also sequenced
after milestone 50, which is BUILT as of 2026-08-14's status catch-up, so that half of the gate is
satisfied and what remains is exactly the conversation: 50's landing removed most of the
requirement and changed what is left, which is now the thing to design from.

**STATUS: RECORDED, NOT DESIGNED.** calef asked for this to be captured as a milestone *and*
explicitly asked to design it together. This block lays out the problem, the options and the
constraints; **it deliberately does not choose.** Do not build from it without that conversation.

**In brief.** `( commands )` is `fork(2)`: Unix runs the group in a copy-on-write duplicate of the
whole process, so changes to variables, the working directory, options and descriptors evaporate on
exit. We have no `fork`, on purpose, and cannot get one cheaply. So the question is what replaces it.

## Why there is no fork, and why that is not a gap to fill

Spawning here is **build-from-parts**: retype an address space and a TCB, map pages, insert
capabilities, configure, start. That is what makes a cricker-os process a lighter object than a Unix
one, which is a claim the benchmarks rest on. `fork` would need copy-on-write duplication of an
address space *and* duplication of a capability space, neither of which exists.

It is also a primitive with a serious case against it: Baumann, Appavoo, Krieger and Roscoe, **"A
fork() in the road" (HotOS 2019)**, argues `fork` is a poor abstraction, not merely an expensive one.
It does not compose with threads, breaks buffered I/O and locks, is a security hazard, and its
semantics are defined by what Unix happened to be able to implement. Plan 9's `rfork(flags)` is the
better-known fix: choose per-resource what is shared and what is copied, which is *exactly* the shape
of the question below.

## Most of what subshells are used for, milestone 50 already answers

Worth establishing before designing anything, because it shrinks the problem a lot:

| Unix subshell use | Answered by |
|---|---|
| Each side of `a \| b` | Milestone 50: the shell spawns two children and grants an endpoint. No subshell needed |
| `$( ... )` command substitution | Milestone 50: a pipe whose reader is the shell |
| `( ... ) &` backgrounding a group | Milestone 48: a job is what the shell holds capabilities for |
| **`(cd /tmp && make)`** | **Nothing. This is the residue** |
| **`(umask 077; ...)`, `(set -e; ...)`** | **Nothing, and `umask` is void anyway (§39-era: no permission bits)** |

So once 50 lands, the remaining need is **scoping**, not process duplication.

## The conflation, which is this project's recurring pattern

`( ... )` means two different things that Unix could not separate because `fork` was the tool it had:

- **Scoping**: run this with a temporarily different working directory / variable / option, and put it
  back. Almost every real use.
- **Isolation**: run this so that *arbitrary* effects cannot escape. Rare, and the only one that
  actually needs a separate process.

That is the same shape as `mv` conflating rename with copy-and-unlink (§42), and `rm` conflating
unlink with revoke (milestone 47). Separating them has been the right answer twice.

## The question with no Unix analogue: duplication is not total

If a subshell is a real child granted "a copy of the parent's endowment", then **what is a copy of a
capability set?** This is the part that needs design, and it is genuinely new.

- Some capabilities duplicate harmlessly: a read capability to a directory.
- Some **cannot** be duplicated, and we have already proved it. §41 gave `Frame::REVOKE` take-back
  semantics on a `DeviceFrame` precisely because **a device must never have two owners**; milestone
  23's whole witness is that the version never goes backwards. A one-shot Reply capability is
  similarly not copyable: it is consumed once by construction.
- So **"copy the endowment" is not a total function**, and any fork-like design needs a defined rule
  for the rest: refuse the subshell, silently omit those capabilities (a silent downgrade, which §42
  forbids), or require the parent to name what crosses.

There is a promising fit with machinery that already exists. If a child's capabilities are
**derived** from the parent's rather than duplicates of them, then §16's revocation and the derivation
tree already give "destroying the child revokes exactly its copies", and §40's supervisor-death-is-
subtree-death makes cleanup automatic. That is an argument for derivation over duplication, and it is
the first thing to test in the design conversation.

## The options, none chosen

1. **Simulate in-process.** The shell saves its own mutable state (working-directory capability,
   variables, options), runs the group, restores. Cheap, no new mechanism, and correct *exactly* when
   effects are confined to shell-local state, which covers `(cd x && y)`. It cannot undo anything a
   command did to a capability, so it is a lie for the isolation case.
2. **A real child shell** granted a derived endowment. Honest isolation; costs a full spawn for
   `(cd /tmp && ls)`; and forces the duplication question above to be answered.
3. **Scoped bindings instead of subshells.** `with cwd = /tmp { ... }` says what it means rather than
   reaching for process duplication because that was the available tool. Earns its divergence under
   milestone 47's rule only if it genuinely covers the uses; it does not cover isolation.
4. **Hybrid**: scoping by binding, isolation by an explicit verb, so the two uses stop sharing a
   syntax.

**Open questions for the conversation**, in the order they probably matter: does derivation beat
duplication; what happens when an endowment contains a non-duplicable capability; is the isolation
case common enough to build for at all once 50 lands; and does `( ... )` keep its Unix spelling if it
means something materially different.

**Sequencing.** After milestone 50 (pipes and redirection), because 50 removes most of the
requirement and changes what is left. **Effort: not estimated**, because the design is not chosen and
the options differ by more than an order of magnitude.

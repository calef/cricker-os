# 14. The project's direction: a verified-Rust capability microkernel that runs real workloads

**Status: AMENDED.** (§82 moves the end state from confining the existing ecosystem to replacing
it; the technical shape recorded here is unchanged. See the amendment at the end.)

Committed 2026-07-23. This is the North Star, recorded because everything downstream (which
milestones are on the critical path, what "done" means) now answers to it. It does not replace the
learning ethos (CLAUDE.md: understanding over velocity); it gives that learning a destination and
settles the forks that were parked *because* the project was "just learning."

## The differentiator, stated precisely

The goal is **a verified-Rust capability microkernel: a small, machine-checked trusted core that
hosts real, unverified workloads with strong isolation guarantees.**

The precision matters, because the obvious phrasing ("a verified capability OS that runs real
workloads") is *already seL4*: verified C, capabilities, running Linux VMs and safety-critical
components in real deployments. The edge that is not already taken is **verified in Rust**. seL4's
proof carries the entire safety burden because C gives it nothing; here the language already removes
the ~70% memory-safety class and machine-checked proofs cover the security-critical logic on top. A
verified-Rust capability kernel that runs real workloads is a live research frontier no shipping OS
occupies. The Rust angle is the whole novelty, which is why the existing choices (capabilities,
share-not-move, no fork, memory safety as a language property) were the right seed.

## The shape that makes "verified" and "real workloads" compatible

They pull opposite ways: "verified" wants a tiny kernel, "real workloads" wants a large system.
seL4's resolution, adopted here: **verify the small microkernel TCB; run real, unverified workloads
in confined userspace on top.** What is promised is not "the whole system is proven" but "the trusted
core is proven, and it confines everything above it, so a compromised workload cannot escape." The
microkernel structure was built for exactly this.

## How we verify, and the evidence it is tractable

**Kani** (bounded model checking for Rust), chosen over an Isabelle/HOL refinement proof (seL4's way,
person-decades, not a solo endeavor). The experiment that earned the choice is in the tree: five
harnesses in `crates/capability` prove the capability model's core theorems for *every* input rather than
sampled cases, including "`derive` never widens rights" and "userspace cannot forge a right"
(`script/verify`, notes/verification.md). It installed and ran in minutes, and the proofs read like
the properties they state. That is the green light.

Verification spreads **inward from the capability core**: the `capability` logic now, then IPC (the
rendezvous and the one-shot reply), then the MMU isolation invariants. Pure-logic crates (§7) are the
natural frontier because they already compile for the host; the proofs live behind `#[cfg(kani)]` and
never touch an ordinary build. **(Milestone 18 delivered all three steps**: the rendezvous state
machine is extracted and proved and the scheduler runs it; the one-shot Reply's mechanism is proved
in `capability` and `ipc`; the MMU isolation invariants, including the user-VA gate the syscalls now call,
are proved in `paging`. See notes/verification.md for what each proof says and what stays on tests.)

## What this resolves and what it changes

- **The verification-endgame fork (design/roadmap/README.md) is resolved: verification is the goal.** So
  milestone 14 (remove the kernel heap) moves from optional purity to **prerequisite** on the
  critical path: a verifiable kernel cannot allocate dynamically.
- **A verification track becomes first-class**, spreading proofs inward as above.
- **"Real workloads" becomes a named track** with its own sub-decision (a native-ABI target first, a
  Linux-compat personality or VM hosting later), replacing the old "POSIX posture" fork, which was an
  optional study back when reach did not bind. It binds now.

## Calibration, the honest limits

- **Not** a from-scratch seL4-scale functional-correctness proof of the whole kernel. That is
  person-decades. The target is machine-checked proofs of the security-critical core, which is both
  novel (in Rust) and reachable.
- **Staged ambition.** The near-term deliverable is the *demonstrator*: a verified core running real
  confined workloads. A general-purpose competitor is an explicit *later optionality*, not the current
  goal; the competitor questions stay parked until the demonstrator earns them.
- **Still a learning project.** The destination is committed; the method (write it together, explain
  the hardware, write the notes) is unchanged. A demonstrator he cannot explain is a failed
  demonstrator.
- **init is the privileged unverified component, and that is a known soft spot.** "Verified core,
  confined unverified workloads" is honest about the *kernel*, but init (which builds every other
  process) is unverified and privileged. The kernel confines it and a compromised init cannot break
  the kernel or escape confinement; but init's bytes are loaded unsigned today and its authority is
  broad. Milestone 22 (design/roadmap/22-trusted-init.md) is where this is closed: verify init before it runs, and
  shrink what a broken one can do. Recorded here so the thesis is not read as claiming more than it
  proves.

## Amendment (2026-08-13): the end state is replacement, not confinement (§82)

This section adopted seL4's resolution, which is to verify a small trusted core and run **real,
unverified workloads** in confined userspace above it. That shape is unchanged and everything above
still holds.

What §82 changes is the destination. This section reads as treating the existing C and
ambient-authority ecosystem as permanent, with the kernel's job being to build a box strong enough to
hold it. §82 records calef's thesis that the box is instead what makes rewriting that ecosystem worth
doing, and that LLMs are why the rewrite is affordable now when it defeated KeyKOS, EROS and Coyotos.

The practical difference is how a port is classified. Under this section, running somebody else's
program is a **demonstration** that confinement works. Under §82 it is the **product**, and the
question becomes how narrow a grant it can run under rather than whether it runs at all. §82 also
carries the falsification conditions, which this section never stated.

# 92. Security audits as a mechanism: cadence, docs, and findings that become milestones

**Status: NOT-STARTED.** Raised 2026-08-03 by Chris. The tree has had one security audit and
milestone 43 asks for a second with a different lens; this milestone is the machine that makes
them routine, so that auditing stops depending on someone remembering to ask.

**Gate: DECISION.** Two things are Chris's before it starts: the cadence itself (quarterly is the
block's proposal, not a decision) and the audit index's name and location, which the overdue
tripwire reads.

**Why a mechanism rather than a habit.** The same argument as script/gates: a practice that lives
in memory gets skipped exactly when it matters. And the failure mode of audits specifically is
already named in this tree: DECISIONS §35's wallpaper, a finding nobody dispositions. An audit
that produces a report and no state change is wallpaper with a security label.

**The mechanism, in four parts:**

1. **A cadence and its tripwire.** Audits run on a recorded schedule (quarterly is the starting
   proposal; Chris sets it) plus event triggers that queue one early: a new syscall method, a new
   component holding device or network authority, a new dependency class (§46), or first boot on
   a new machine class (a board, a cloud). The tripwire follows the toolchain-drift pattern, a
   signal rather than an automation: a scheduled workflow compares the last audit's date (recorded
   in the audit index) against the cadence and goes red when one is overdue. Red means "run the
   audit", not "an automation ran it for you".
2. **Each audit is a lane with a lens.** Milestone 43's insight generalized: the value of the
   next audit is the lens the last one lacked. The lens rotation is recorded in the audit index
   (capability model, supply chain, network surface, userspace confinement, docs-versus-reality
   truthfulness, ...), and each audit names its lens, its scope, and what it deliberately did not
   look at.
3. **Every finding ends in exactly one of three states**, and the report is not done until each
   has one: **fixed** (trivial, done in the audit lane itself), **minted as a milestone** (the
   audit report proposes it, the integrator mints the number at merge, severity and rationale in
   the block, exactly how milestone 90 was born from milestone 84's finding), or
   **recorded-accepted** (with the reason, in the report and, where a reader would meet the risk,
   in the affected doc's BUGS section). "Noted" is not a state.
4. **The docs re-baseline with every audit.** SECURITY.md's claims, the confinement scope, and
   the affected notes' BUGS sections are part of each audit's diff: an audit that finds the docs
   overclaiming fixes the docs in the same lane, because the demonstrator's docs are part of the
   deliverable and an overclaim in SECURITY.md is itself a security finding.

**The audit index** (location and name provisional, Chris settles both): one file listing every
audit with date, lens, finding count by disposition, and a link to its report note. The overdue
tripwire reads this file, which keeps the mechanism honest the same way the roadmap gate reads
the milestone files: the record and the signal cannot drift apart.

## Scope note

Milestone 43 is unchanged and becomes the first audit run *under* this mechanism, inheriting its
"different lens" mandate as lens-rotation's first step. This milestone builds the machine, not an
audit: the deliverable is the index, the tripwire workflow, the disposition rule written where
audit lanes will read it, and SECURITY.md gaining a sentence that routine audits exist and where
their reports live. What this cannot promise, said plainly: a mechanism guarantees audits happen
and findings get dispositioned; it does not make any audit good. The lens list is a prompt, not a
proof of coverage, which is the same honest limit the cpu matrix records about five models.

# 92. Security audits as a mechanism: cadence, docs, and findings that become milestones

**Status: NOT-STARTED.** Raised 2026-08-03 by Chris. The tree has had one security audit and
milestone 43 asks for a second with a different lens; this milestone is the machine that makes
them routine, so that auditing stops depending on someone remembering to ask.

**Gate: DECISION.** One thing is Chris's before it starts: **the cadence**. Quarterly is this
block's proposal, not a decision. The index name and location were settled on 2026-08-04,
`design/audit-reports/`, which is what the overdue tripwire reads.

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

**The audit reports live in `design/audit-reports/`** (Chris, 2026-08-04): one file per audit, with
`README.md` as the index listing every audit's date, lens, finding count by disposition, and a link
to its report. The overdue tripwire reads the index, which keeps the mechanism honest the same way
the roadmap gate reads the milestone files: the record and the signal cannot drift apart.

The shape is `design/roadmap/`'s and `design/decisions/`', and for the same reason: **the index is a
table and a report is a document.** Audits arrive slowly (quarterly plus triggers, so a handful a
year), so the count is not what would have outgrown a single file; the reports are, because each
carries its lens, its findings, their dispositions, and what it deliberately did not examine.

`audit-trail` was considered and refused: `design/decisions/35-scanner-findings.md` already uses that
phrase in its established sense, a chronological record of who did what, which is also what an
operating system means by it (Linux's `auditd`, BSD's audit subsystem). A kernel whose thesis is
confinement is a plausible future home for that feature, and this is not it. `audit-reports` over
bare `audits` because every file in there is literally a report.

## Scope note

Milestone 43 is unchanged and becomes the first audit run *under* this mechanism, inheriting its
"different lens" mandate as lens-rotation's first step. This milestone builds the machine, not an
audit: the deliverable is the index, the tripwire workflow, the disposition rule written where
audit lanes will read it, and SECURITY.md gaining a sentence that routine audits exist and where
their reports live. What this cannot promise, said plainly: a mechanism guarantees audits happen
and findings get dispositioned; it does not make any audit good. The lens list is a prompt, not a
proof of coverage, which is the same honest limit the cpu matrix records about five models.

# The untracked-work sweep, and what each finding became

On 2026-08-04 milestone 94 (the untracked-work sweep) read the tree for work somebody had
identified and never given a home: a `TODO` nothing lists, a note saying "not built" with no block
behind it, a decision's follow-up aside, a roadmap stretch item. Every finding had to end in one of
three states. **"Noted" is not a state.**

The sweep ran and its inventory then lived in pull request #91's description and nowhere else, for
twelve days, which is precisely the failure the milestone was written about. A pull request body is
a report, and a report is not a record. This note is the record.

## What was swept

Four surfaces: `TODO`/`FIXME`-class markers in the Rust tree, notes carrying deferral phrasing
(`someday`, `follow-on`, `not built`, `deferred`), `DECISIONS` follow-up asides, and the roadmap's
own stretch items.

**The measurement corrected the block's own floor, in a useful direction.** The block predicted 11
TODO-class markers. There were 11 hits and only **two markers**: seven were comments *about* a
`TODO` ("see the `TODO` on `paging::unmap`", "Not a TODO."), and two were literal text, the GUID
formatter's `XXXXXXXX-XXXX` and `gl-one.txtXXX` in a glob test. `git grep -w TODO` is 82% false
positives on this tree.

That is what shaped the gate rather than a taste for tidiness. `script/lint` now fails a marker
that does not name a milestone with a block, and **a marker is the word with its punctuation
attached** (`TODO(` or `TODO:`); a mention is not. Markdown is exempt on purpose, because prose
explaining the convention has to spell the shape it forbids, and a note may quote a marker resolved
five milestones ago as history rather than intent.

The two real markers were both `TODO(portability):` on the same assumption, and neither named a
home. They became a limitation stated where a reader meets the hardcoded value
(`kernel/src/arch/aarch64/mod.rs`, `kernel/src/smp.rs`, each naming the other) and a milestone,
rather than an invented citation, which would have been the well-formed-wrong citation this tree
already collects.

## The three states, and the count in each

| State | Count | Where they went |
|---|---|---|
| Minted as a milestone | 12 | Milestones 100 through 111 |
| Folded into a block that already owned it | 3 | 44 (signed commits), 62 (a per-test heartbeat), 78 (the icount timer-grid check) |
| Already tracked | 10 | Deduped rather than double-minted. Item-level list not recovered; see BUGS |
| Recorded-accepted, and blessed | 9 | Listed below, and each blessing is written beside its limitation |

Milestones **112 and 113 share the numbering run and are not this sweep's**: they came from
milestone 82's unsafe-lint lane. Fourteen blocks landed in one pull request (#94) and twelve of them
are the sweep's, which is where the two counts get confused.

## The twelve that became milestones

| Milestone | Found where |
|---|---|
| 100, read the machine's PSCI and its CPU list | the two `TODO(portability):` markers, converted to a recorded limitation the same day |
| 101, the L4 calibration | raised as "the EL0-to-EL0 IPC benchmark nobody owns", which the tree contradicted: the benchmark had been published for days and the *comparison* was what had not followed |
| 102, what a confined device's fault reaches | three documents deferring to "a fault-handling milestone" that has never existed |
| 103, `^C` stops spinning | raised from a stale handoff note as "decided and never built"; `^C` was built, and §24's own named interim was not |
| 104, the measurement continues past init | a sentence milestone 22 left behind after it went BUILT |
| 105, the two forks milestone 22 named and left | two questions `notes/trusted-init.md` marked "calef's call, not a thing to slip in" |
| 106, a wait that ends on either the interrupt or the deadline | `notes/net.md`, where milestone 30's lane recorded the cost of not having one |
| 107, the socket contract learns to accept | the contract has no listen verb and two milestones need one |
| 108, the drivers move onto frame capabilities | `notes/frames.md`, closing with the migration it deliberately did not do |
| 109, `xargs` at the grant bound | both glob notes ending on the same sentence about an eight-name cap |
| 110, the recovery tool takes a device and a partition | `notes/host-recovery.md`, milestone 57's residual |
| 111, a shell that can endow a child with entropy | `notes/entropy.md`, "future work with no design problem in it" |

Two of the twelve are worth reading as findings in their own right, because the sweep was wrong
about both and the tree said so: 101 and 103 were each raised as unbuilt work and each turned out
to be built, with a different, smaller thing genuinely owed. A sweep that reads prose inherits the
prose's staleness. That is milestone 93's territory (documentation audits as a mechanism) and it is
why the two milestones exist as corrections rather than as tasks.

## The nine that were blessed where they sit

A **blessing** is the third state made durable: this limitation is the design, it is staying in its
`BUGS` section or its section of a note on purpose, and an audit may pass over it without arguing
it again. Each of these nine now carries the blessing in the file itself, in the paragraph a reader
meets it in, because a registry of blessings would be the same evaporating medium one level up.

| The limitation | Where the blessing is written |
|---|---|
| A `BootInfo` block and a POSIX shim, both deferred with recorded triggers | `notes/abi.md`, "What is deliberately deferred" |
| No IPC timeouts and no revocation: the seL4 depth parked | `notes/security.md`, "What is deferred, on purpose, and named honestly" |
| A reclaiming `unmap`, considered and not built, because an address space dies all at once | `notes/teardown.md` |
| A minted endpoint per socket, deferred with a named trigger | `notes/net.md`, the socket contract |
| A delegable clock: no program is asking, so it is recorded and not built | `notes/grant-expression.md`, "What a delegable clock would still need" |
| The capability derivation tree, not built; generational names are the other way to be safe | `notes/generational-names.md` |
| zsh's glob qualifiers, settled and out, because a qualifier needs a right beyond enumerate | `notes/glob.md` |
| `ArgSpec` has no position and no arity, deferred until a program wants both an argument and a file | `notes/date.md` BUGS |
| The signature variant over init, recorded and not built | `notes/trusted-init.md` |

**A blessing is not a promise to keep the limitation forever.** §71 (a limitation is promoted when
it becomes a plan) decides when a `BUGS` entry turns into a roadmap row, and any of these nine is
promoted the moment one of its three triggers fires. The blessing asserts only what was true at the
sweep: no trigger had fired, and the limitation is a decision rather than an oversight.

**One of the nine touches that convention directly.** §71 names the signature variant as the shape
of its second trigger, a design fork calef must rule on before any lane could start, which is the
one case that lands as a `RECORDED` row. No such row exists. Promotion is the integrator's act and
not a lane's, so the blessing stands and this paragraph is the flag.

## The candidates that were looked at and not blessed

Recorded so the next reader does not have to redo the triage, and so the set above reads as a
decision rather than as everything that happened to be found.

| Candidate | Why it is not in the nine |
|---|---|
| `notes/benchmarks.md`, the seL4 comparison deferred to real hardware | tracked since 2026-08-15 by milestone 127 (the seL4 machine), whose board is bought |
| `notes/sched-lock-inventory.md`, contention that only exists on hardware we do not have | the same shape as above and waiting on the same silicon; a hardware trigger, not a decision |
| `notes/load-sensitive-assertions.md`, "Recommended here, not built here" | milestone 78 owns it |
| `notes/host-recovery.md`, a backup when the primary fails | the neighbouring finding in the same note became milestone 110; this one is a proposal nobody has taken, which is trigger 3's territory rather than a recorded limitation |
| `notes/live-replacement.md`, the real derivation tree still wanted | the same object as the `notes/generational-names.md` blessing; blessed once, at the note that explains the alternative |
| `notes/verification.md`, whole-parse totality deferred | a verification bound with its own analysis, and milestone 18's surface |
| `notes/supervision.md`, runtime reattach deferred to milestone 23 | already tracked, and it names the milestone |
| `notes/untyped.md` and `notes/userspace-drivers.md`, §10's deferred third axis | already tracked as milestone 11 |

## The lesson this note is

Deliverable one of milestone 94 was a sweep, and the sweep did its job. What went wrong afterwards
is the thing the milestone was written to abolish, committed by the milestone itself: the inventory
was written into a pull request body, the nine blessings were described in a lane report, and both
media are unreadable a fortnight later. The tree keeps records; GitHub keeps correspondence.

The rung to have reached for was the third one, a written record at the thing itself. That is what
the nine blessings are now, and what this note is for the inventory around them.

**It repeated once more on the way out, one level up.** The commit that landed this note and the
nine blessings did not touch milestone 94's roadmap block or the index, so both records went on
saying the work was outstanding while agreeing with each other. That is §76's shape and
`script/roadmap --check` cannot see it: the gate compares the index against the block, never either
against the tree. A second lane corrected the record. The general lesson is the narrow one: **the
commit that does the work is the commit that moves the record**, because a follow-up nobody is
assigned is the same rung-zero "somebody will notice" this whole family exists to retire.

## BUGS

- **The original nine are not recoverable, and the nine above are a re-derivation.** Pull request
  #91's body states the count and describes the category; it never lists the items, its commits
  touch only the lint and the PSCI comment, it has no review comments, and no local session
  transcript of the lane survives. So the list above is the sweep's own rule applied a second time
  to the tree as it stood at the sweep's merge commit (`3eeee7df`), keeping only limitations that
  are deliberate, still present, and carried by no roadmap row. Where the two sets differ, nobody
  can now tell. This is stated rather than smoothed over because the whole point of the milestone
  is that a record which cannot be checked is not a record.
- **The ten already-tracked items are a count and not a list.** They were deduped against existing
  blocks and named nowhere that survived. A milestone 93 audit meeting one of them will find the
  block that owns it, which is the outcome the dedupe was for, so the loss costs a reader
  provenance rather than correctness.
- **Nothing gates a blessing.** A limitation can lose its blessing by being edited out of the
  paragraph that carries it, and no check would notice. The higher rung is not obviously worth its
  cost for nine paragraphs, so this is a marked exception: if the set grows past what a person can
  hold, it wants a gate rather than another sweep.
- **The sweep was one-time by design and this note describes one event.** The recurring duty is
  split between milestone 93's audits, which meet deferral phrasing in prose, and `script/lint`,
  which meets it in code. Do not read this note as a live inventory; read it as what the tree
  decided on 2026-08-04 and what happened to each finding.

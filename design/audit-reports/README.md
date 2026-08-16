# Audit reports

*Name: ratified (calef, 2026-08-04; §75 covers this directory). `audit-trail` was refused because
[35-scanner-findings.md](../decisions/35-scanner-findings.md) already uses that phrase in its
established sense, a chronological record of who did what, which is also what an operating system
means by it (Linux's `auditd`, BSD's audit subsystem); a kernel whose thesis is confinement is a
plausible future home for that feature, and this is not it. Bare `audits` was passed over because
every file in here is literally a report. Recorded here rather than in a registry, per §75: a
directory has no header file, so its provenance lives in its own README.*

One file per audit, this file as the index. An audit is a deliberate adversarial read of the tree
through one named lens, and the reason there is a directory rather than a habit is
[milestone 92](../roadmap/92-security-audit-cadence.md): a practice that lives in someone's memory
gets skipped exactly when it matters.

Two kinds of audit share this index, because a documentation sweep asks the same scheduling
question a security audit does, "what has changed since the last one", and answers it from the same
counts. `script/audits` reads both.

## When the next audit is due

`script/audits` answers, from this file and from the tree. It never runs an audit and never edits
anything:

```
$ script/audits
security      last 2026-08-15 (Untrusted counterparty input: a value a hostile counterparty)
  milestones built     71 -> 72  (+1, fires at 15)
  components           104 -> 106  (+2, fires at 8)
  ABI constants        43 -> 43  (+0, any change fires)
  external packages    108 -> 108  (+0, any change fires)
  calendar             1 day since (6 weeks)
  not due

The two triggers nothing here can count, which are a judgment and not a number:
  - has a new component taken device or network authority since the last audit?
  - has this booted on a new machine class (a board, a cloud) since the last audit?
  If either is yes, an audit is due regardless of everything above (milestone 92, §74).

audits: 4 on record, 1 kind, none due
```

The triggers, and the ruling behind each, are
[§74](../decisions/74-audit-cadence.md): **event triggers first, a count second, the calendar a
backstop.** The count numbers are the interval this project chose when nobody was counting, rounded
to something a person can hold.

| Kind | Milestones | Components | ABI constants | External packages | Weeks |
|---|---|---|---|---|---|
| security | 15 | 8 | 1 | 1 | 6 |

Read a cell as "fires when this many have been added since the last audit of this kind". So `1`
means any change at all fires, which is how the two event triggers that a script can actually count
are expressed: a new syscall method shows up as a new ABI constant, and a new dependency (§46) shows
up as a new external package. `15` and `8` are §74's count trigger. `6` is the calendar backstop, and
its job is the opposite of what a reader assumes: it does not catch a busy period, because the count
catches that sooner. It catches a **quiet** one, a tree that sits untouched while the field's threat
model moves anyway, which no measure of this project's own change can see.

**Two of milestone 92's four event triggers are not in that table, and cannot be.** "A new component
holding device or network authority" and "first boot on a new machine class" are judgments about what
a component *does* and about hardware that may not be in the tree at all. Nothing counts them, so
`script/audits` prints them as a question rather than pretending to answer it. The count triggers are
the backstop for exactly this: a new network-facing parser is also a new component, so the batch
trigger reaches the same place, later. Later is the wrong word for an attack surface, which is why
the judgment stays a human's and why it is printed on every run rather than filed somewhere.

**Red means "run the audit", never "an automation ran it for you".** That is why the overdue check
lives in a scheduled workflow (`.github/workflows/audit-cadence.yml`) and not in the gate every pull
request runs. An audit coming due is not a defect in the commit that happened to trigger the count,
and blocking every unrelated merge behind a review nobody can hurry is the wall this mechanism was
written not to build. It is the same split, and the same reason, as `toolchain-drift.yml`.

`script/lint` does run `script/audits --check`, which is the *structural* half: the two tables below
describe the same audits, every report link resolves, every kind has a cadence row, the dispositions
parse. That can fail a pull request, because a malformed index is a defect in the commit that
malformed it.

## The audits

| Date | Kind | Lens | Findings | Report |
|---|---|---|---|---|
| 2026-07-15 | security | The whole kernel read cold, four reviewers, one dimension each, after milestone 11 | fixed 4, milestone 0, accepted 0 | [A security audit](../../notes/security.md) |
| 2026-07-29 | security | The hand-written architecture assembly, for state staged in single-copy hardware registers across more than one instruction | fixed 2, milestone 0, accepted 1 | [Auditing the hand-written architecture assembly](../../notes/arch-audit.md) |
| 2026-08-04 | security | Time of check to time of use across every page shared by two address spaces | fixed 5, milestone 1, accepted 1 | [Auditing the shared pages](../../notes/shared-page-audit.md) |
| 2026-08-15 | security | Untrusted counterparty input: a value a hostile counterparty supplies in one message or completion | fixed 0, milestone 0, accepted 1 | [Auditing untrusted counterparty input](../../notes/untrusted-input-audit.md) |

## What the tree looked like when each ran

The tripwire's half of the index. It is a second table rather than four more columns on the first
one because the two have different readers: a person wants the lens and the findings, and
`script/audits` wants the counts. The `--check` gate insists the two tables list exactly the same
audits, which is what stops them drifting apart.

Every number is **counted, not remembered**, by `script/audits --baseline` at the commit that landed
the report. Milestones built is the `BUILT` rows in
[design/roadmap/README.md](../roadmap/README.md); components is `crates/*/` plus `[[bin]]` targets in
`user/Cargo.toml`; ABI constants is the `pub const NAME: u64` surface of `crates/abi`; external
packages is the distinct registry packages across every committed lockfile but the vendored one.

| Date | Kind | Milestones built | Components | ABI constants | External packages |
|---|---|---|---|---|---|
| 2026-07-15 | security | - | 10 | 9 | 3 |
| 2026-07-29 | security | - | 43 | 42 | 20 |
| 2026-08-04 | security | 55 | 95 | 43 | 87 |
| 2026-08-15 | security | 71 | 104 | 43 | 108 |

The two `-` cells are honest rather than lazy: `design/roadmap/README.md` did not exist until
milestone 76 split it out on 2026-08-03, and milestones 1 to 11 were backfilled the same day, so
there is no contemporaneous count to take. `script/audits` refuses to compute a delta from a `-` and
says which trigger it therefore cannot evaluate. It only ever reads the newest row per kind, so the
gap costs nothing today.

**One number here disagrees with §74 by one.** That entry says the shared-page pass landed with 54
milestones built; counted from the roadmap index at the commit that added the report, it is 55. Both
were counted honestly at slightly different commits, which is the same class of thing CLAUDE.md
records about the Kani harness count. The method above is stated so the number is re-derivable, which
matters more than which of the two is right.

## Where the reports live, and why none of the four is in this directory

**New audit reports land here.** The four on record predate this directory and stay in `notes/`,
linked rather than moved, and the reason is a measurement rather than a preference: those four files
are referenced **85 times from 27 files**, including kernel source comments
(`kernel/src/arch/riscv64/trap.s`, `kernel/src/drivers/plic.rs`, `kernel/src/sched.rs`), a dozen
notes, six user programs, `SECURITY.md`, `README.md`, and four files under `design/` that a lane may
not edit at all. Moving them means rewriting all of that by hand or by a blind `sed`, and a blind
`sed` is the specific mechanism that destroyed a naming refusal in this tree once already.

The cost of not moving them is one hop for a reader, and it is the smaller cost. The index carries
the date, the lens, the dispositions and the link, which is everything the mechanism needs and most
of what a reader wants before deciding to open a 600-line report.

`notes/redoxfs-audit.md` is deliberately **not** in the index. It carries the word and it is a
different act: it costed a port by building the crate, and it did not look for vulnerabilities. An
index that took every file called an audit would report a coverage it does not have.

## Running one

1. **Pick the lens the last audit lacked.** That is milestone 43's insight and it is the whole
   reason the `Lens` column exists: read the rows above and ask what they did not look at. The
   rotation so far is the whole kernel, then the assembly, then the shared pages, then untrusted
   counterparty input. Candidates not yet taken: supply chain, userspace confinement, the syscall
   surface itself, docs against reality (that one is milestone 93's kind).
2. **Say what you did not look at.** Every report on record has a "what was deliberately not
   examined" section, and it is the section an outside reviewer uses first.
3. **Every finding ends in exactly one of three states**, and the report is not done until each has
   one:
   - **fixed**, in the audit lane itself, for anything trivial enough to fix while you are there;
   - **minted as a milestone**, where the report proposes it and the integrator mints the number at
     merge, with severity and rationale in the block. That is how milestone 90 was born from
     milestone 84's finding;
   - **recorded-accepted**, with the reason, in the report *and* in the affected doc's `BUGS`
     section wherever a reader would meet the risk.

   **"Noted" is not a state.** DECISIONS §35 already names the failure mode: a finding nobody
   dispositions is wallpaper, and a security label does not change that.
4. **Re-baseline the docs in the same lane.** `SECURITY.md`'s claims, the confinement scope, and the
   affected notes' `BUGS` sections are part of the audit's diff. An audit that finds the docs
   overclaiming fixes them there and then, because an overclaim in `SECURITY.md` is itself a security
   finding.
5. **Add both rows**, here and in the baseline table. `script/audits --baseline` prints the counts
   to paste.

## EXAMPLES

Is anything due, and why or why not:

```sh
script/audits
```

The tripwire's own question, as the scheduled workflow asks it. Exit 0 when nothing is due, 1 when
something is:

```sh
script/audits --due; echo "exit $?"
```

The structural gate, which is what `script/lint` runs:

```sh
script/audits --check
```

The counts for a new row, at the tree you are looking at:

```sh
script/audits --baseline
```

## BUGS

- **A mechanism guarantees that audits happen and that findings get dispositioned. It does not make
  any audit good.** The lens list above is a prompt, not a proof of coverage, and the same honest
  limit applies here as the CPU matrix records about its five models.
- **Two of the four event triggers are uncountable and are printed as a question, which is rung four
  on CLAUDE.md's ladder.** "A new component holding device or network authority" and "first boot on a
  new machine class" rely on somebody reading the output and thinking. That is a known weak spot,
  mitigated only by the count triggers reaching the same place later.
- **The disposition columns for the four retroactive rows were read out of reports written before the
  three-state rule existed.** They are a faithful summary of what each report's own summary table
  says, but no author of those four was working to this vocabulary, and "left documented" was mapped
  to `accepted` by a later reader. Audits run under the mechanism state their own dispositions.
- **The count triggers cannot see a change that lands as neither a milestone nor a component.** A
  substantial rewrite inside an existing crate moves no number in the baseline table. The calendar
  backstop is the only thing that eventually catches it, and six weeks is a long time.
- **Nothing checks that the baseline numbers were taken at the commit they claim.** They are prose
  in a table, verified by re-running `--baseline` against that commit, which nobody does
  automatically. The `--check` gate validates that the cells are integers or `-`, not that they are
  true.
- **The scheduled workflow does not report its own death.** Same limitation `notes/merge-queue.md`
  records for the merge drain: a workflow that is disabled, or whose schedule GitHub drops on an
  inactive repository, goes quiet in exactly the way a green run does.

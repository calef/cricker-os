# The documentation sweep: how to run one, and what counts as a finding

*Name: unrecorded. `documentation-audit.md`, and the phrase **documentation sweep** it introduces,
are **provisional**, minted by milestone 93's lane on 2026-08-16 and not yet put to calef, who names
things. `doc-audit.md` was passed over because the tree spells words out (`design/decisions/`, not
`design/dec/`); `claim-rot.md` names the disease rather than the procedure, and this file is a
procedure; `docs-versus-reality.md` is the **lens** the first sweep took, which is a row in an index
rather than the name of the method.*

A documentation sweep is milestone 92's audit mechanism pointed at a second target. 92 keeps the
security story from rotting; this keeps every other documented claim from rotting. It shares 92's
index, 92's tripwire, and 92's disposition rule, and adds one row to a table rather than a second
copy of the machine. See [design/audit-reports/README.md](../design/audit-reports/README.md), which
is the index, and [milestone 93](../design/roadmap/93-doc-audit-cadence.md), which is the block.

## What it is looking for, and what it is not

**Claim rot**: a sentence that was true when it was written and that the tree has since moved past.
A path that was renamed. A number that grew. A "currently" or a "still open" describing a state that
ended. A plan a later decision superseded, still written in the future tense.

The four exhibits that raised this milestone were all found in one day, 2026-08-03, and every one of
them **by accident, while somebody was looking for something else**: `notes/verification.md` said
the proof suite ran in "a few minutes" a month after that stopped being true; two roadmap status rows
claimed more remained than their own blocks said; milestone 47's in-brief listed shipped work as
pending; `notes/cpu-models.md`'s closing line prescribed a fix a later milestone had superseded. The
sweep exists so that finding these stops being luck.

**It is not the other four things, and the boundaries are worth knowing before you start**, because
three of them are already somebody's job:

| what | who owns it | why it is not this |
|---|---|---|
| a number in the prose, marked | milestone 125, `<!--count:NAME-->` and `script/lint` | mechanically re-derivable, so it is gated, not read. If a sweep meets an **unmarked** number the output is "this wants a marker", not a second gate |
| whether a newcomer can succeed at all | milestone 117, the stranger test | 117 hands the repository to a reader with **no** context and treats their questions as the deliverable. A sweep's reader is deliberately the opposite: someone who knows the tree, checking prose against it |
| dead relative links, status vocabulary, `§N` citations, spelling | `script/lint`, `script/roadmap`, `script/decisions`, `typos` | structural rot, already gated. A sweep that reports these is reporting the gates' output |
| documentation that does not exist | milestone 68 (code-quality gates), milestone 40 (documentation as a system service), milestone 91 (a glossary) | a sweep audits prose against reality. It does not write missing prose, and it does not restructure it |

The line between this and milestone 125 is the sharpest of the four and the easiest to blur, so say
it in one sentence: **125 insists that a marked number be right and never that a number be marked;
this is the reader who meets the unmarked ones.** 125 is a ratchet that a gate turns, this is a
ratchet that a person turns, and part 3 below is how the second feeds the first.

## Running one

Milestone 92's five steps apply unchanged and are in the index. These are the four that are specific
to a documentation lens.

### 1. Take the worklist, then choose a scope smaller than it

```sh
script/audits --worklist
```

That ranks every document in `notes/`, `design/roadmap/` and the repository root by **how many of the
files it cites have moved since the document itself was last edited**. It is milestone 93's block
asking for exactly this ("a note whose cited code has changed substantially since the note's last
edit is the highest-value candidate for the next sweep, and git can compute that"), and the answer to
the question that block left open is: **a heuristic, not a signal.** Nothing consumes its exit
status, and there is no red. A ranking cannot read a sentence, so a document at the top may be
perfectly true and one that is absent may be a year out of date.

Then choose a scope you can actually finish. A sweep that opens forty documents produces a list
nobody dispositions, which is DECISIONS §35's wallpaper wearing a documentation label. Four to six
documents, read properly, beats forty skimmed.

### 2. Read each claim against the tree, not against your memory of it

The claims that rot are boring and specific. Check these, in this order, because they get more
expensive as you go down:

1. **Names of things a reader would type.** A path, a script, an environment variable, a function, a
   crate. Cheap: `ls`, `git grep`, `git log --diff-filter=D -- <path>` to find what a deleted file
   became.
2. **Numbers.** Re-derive it. If it is stable and worth gating, it wants a milestone 125 marker; if
   it is a hedged magnitude ("about 1,500 lines") re-measure it and keep the hedge.
3. **Tense.** `grep -n "currently\|not yet\|still\|today\|for now\|will "` is the whole technique.
   Every hit is a claim about a moment, and the moment has passed at least once.
4. **Plans.** A note that describes proposed work, or a roadmap block that says what remains, rots in
   the other direction: **planned-as-built and built-as-planned are the same defect.** Check the
   block's own status and the decisions that landed after it.
5. **Arguments that rest on a fact.** The most expensive and the most worth doing. A recommendation
   whose reason has changed is worse than a wrong number, because the reader acts on it.

### 3. Convert what you can into something a gate re-derives

**This is the compounding half, and a sweep that skips it is a sweep that will cost the same next
time.** Each pass should leave behind at least one class that can never rot again. The bar is
DECISIONS §39's, read forward: a name is a claim, a number is a claim, and a claim a machine can
re-derive should not be left to a reader.

The first sweep's worked example is in `script/lint`'s `==> environment names in prose` block. It
found thirteen `CRICKER_*` environment variables in nine documents that name nothing the tree
reads, survivors of milestone 120's rename, every one of them an instruction a reader would type and
get nothing from. Fixing the nine a lane may edit took ten minutes; the gate that means there is
never a fourteenth took thirty, and it derives
liveness from the tree rather than spelling `NIFE_` into a rule, which is why it correctly passes the
one document that names `CRICKER_CC` (`script/bootstrap` really does read it, so the *script* is the
stale one and the note is right).

If nothing in your scope is convertible, say so in the report. That is a finding about the sweep, and
it is how the mechanism learns it has reached its limit.

### 4. Disposition every finding, in 92's three states

**fixed**, **minted as a milestone**, or **recorded-accepted**, and "noted" is not a state. Most
documentation findings are `fixed` in the lane, which is the difference between this kind and a
security audit; a doc correction is usually one edit. Two cases are not:

- **A finding in `design/decisions/` or `DECISIONS.md`.** Lanes do not edit those, so the finding is
  handed to the integrator or grouped into a proposed milestone. Both `script/audits --worklist` and
  the environment-names gate exclude that directory for the same reason.
- **A doc gap that reveals a system gap.** That is the 84-to-90 path and it is a mint, not a fix.

## EXAMPLES

The first sweep, start to finish, as it actually ran on 2026-08-16.

**Ask whether one is due.** It was, on the strongest possible trigger:

```sh
$ script/audits
documentation  never audited
  DUE: no audit of this kind has ever run, and its cadence row says one is expected
  ? has a subsystem been rewritten inside its existing crate since the last sweep? ...
  ? has a decision superseded a plan that a note still prescribes? ...
```

**Take the worklist and read the top of it:**

```sh
$ script/audits --worklist
 moved/cited  commits  last edit   document
   20/24           31  2026-08-04  notes/shared-page-audit.md
   15/16           94  2026-08-03  notes/glyphs.md
                       xtask/src/main.rs, scripts/qemu-runner-aarch64.sh, ...
   11/12           48  2026-08-03  notes/framebuffer-contract.md
    8/8            40  2026-08-04  notes/compositor.md
```

**Check a name a reader would type.** `notes/glyphs.md`'s last line said the keyboard rides
`CRICKER_KBD`:

```sh
$ git grep -n 'CRICKER_KBD\|NIFE_KBD' -- ':!*.md'
xtask/src/main.rs:4701:    unsafe { std::env::set_var("NIFE_KBD", "1") };
scripts/qemu-runner-aarch64.sh:250:if [ -n "$NIFE_KBD" ]; then
```

**Ask whether the class is bigger than the instance.** It was both times the first sweep asked, and
this is the step that turns a typo fix into a mechanism:

```sh
$ git grep -no 'CRICKER_[A-Z_]*' -- '*.md' ':!vendor' | sort -u
design/decisions/18-pcie-transport.md:49:CRICKER_DISK
notes/c-seam.md:186:CRICKER_CC
notes/cpu-models.md:24:CRICKER_CPU
...
```

**Check each one against the tree before "fixing" it**, because one of them was not stale:
`script/bootstrap` reads `$CRICKER_CC` for real, so `notes/c-seam.md` is correct and the rename is
what is unfinished. A blind `sed` here would have made a right document wrong, which is the same
mechanism that destroyed a naming refusal in this tree once already (AGENTS.md).

**Then gate the class, run the gate, and file the report:**

```sh
$ script/lint
==> environment names in prose
environment names: 28 live, and every one named in prose is read or assigned
$ script/audits --baseline     # the counts to paste into both index tables
```

## BUGS

- **The worklist ranks documents that cite code in a form it can parse, and says nothing about the
  rest.** 138 of the 269 documents in its scope resolve no code path at all and are simply absent
  from the list rather than reported as clean. Many are conceptual notes that legitimately cite
  nothing (`notes/acronyms.md`), and some are notes that cite by bare filename (`sched.rs`), which it
  deliberately does not resolve because a basename is ambiguous across the tree.

- **It scores breadth, not staleness, and it cannot read a sentence.** The rank is how many cited
  files moved; whether any of that movement made a sentence wrong is exactly the judgment the sweep
  exists to supply. Ranking by commit count alone was tried and is worse: it puts every document
  that mentions `xtask/src/main.rs` on top, because that file and `kernel/src/user/tests.rs` are
  where every lane wires its test.

- **A `path::symbol` citation, and a bare filename, are invisible to the unresolvable-path survey**,
  though not to the worklist. The first sweep's own path check matched `` `user/src/virtio.rs` ``
  and missed `` `user/src/virtio.rs::write_block` `` on the next line of the same note. Four of the
  nine path corrections were found only because a `git grep` for the fixed name turned them up
  afterwards. **After fixing a name, grep for the old one again in every shape it could take.**

- **A sweep's fix can introduce its own rot, and the first one did.** `notes/fs-server.md` was
  corrected from `user/src/virtio.rs::run_blk_server` to `user/src/blk.rs::run_blk_server` and that
  was still wrong: the function moved to `crates/virtio` and `user/src/blk.rs` only dispatches to
  it. Caught by checking the new name the same way the old one was checked. **Verify the
  replacement, not just the original.**

- **The environment-names gate exempts three places, and one of them is this file.** An audit report
  and this procedure both have to be able to quote a stale name, which is the same exemption
  `script/lint` makes for `design/` and `notes/naming.md` in its rejected-vocabulary check. The cost is real: a
  stale name written into `design/audit-reports/` or into this file is unchecked, so if the sweep
  procedure ever tells you to *type* something, it is not covered by the gate that covers everything
  else. A fenced code block is deliberately **not** exempt, because a fence is where a reader copies
  from and one of the first sweep's findings was inside one.

- **`design/decisions/` is out of scope for the worklist, for the environment-names gate, and for a
  lane's edits, so findings there can only be handed off.** Five stale environment names sit there
  today for exactly this reason. The justification is that a decision is a dated record of an
  argument rather than a description of the tree, which is true of the reasoning and *not* true of a
  command the decision tells a reader to type.

- **The cadence's calendar backstop is weaker for this kind than for security, and is set to 12
  weeks rather than 6 to say so.** Documentation rots because the tree changed, which the count
  triggers see. A quiet tree does not rot its docs. The one thing the counts cannot see is a
  substantial rewrite *inside* an existing crate, which moves no milestone and no component; the
  worklist catches that better than any calendar, which is why it exists.

- **The external-packages trigger is a poor proxy here and is set to 30 rather than 1.** A
  documentation sweep does not care that a transitive lockfile entry moved; it cares that a
  dependency *class* arrived, and nothing counts classes. Thirty is a batch size that means the graph
  really moved. The honest reading of that cell is "effectively off, kept because the table has a
  column".

- **Nothing checks that a sweep happened.** A row in the index closes the tripwire, and a row is
  cheap to type. That limit is 92's and it is not fixable by a script: the mechanism makes the sweep
  *scheduled*, and only a person makes it real.

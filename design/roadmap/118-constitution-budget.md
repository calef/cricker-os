# 118. CLAUDE.md has a budget, and the rules that get violated move up the ladder

**Status: NOT-STARTED.** Minted 2026-08-05 by Chris, who noticed the file had gotten huge and asked
what that costs.

**Gate: NONE.** The measurement is already taken and the first cut needs one lane.

## What it costs, measured 2026-08-05

**738 lines, 8,306 words**, roughly 11,000 tokens, loaded into every session and into every lane
brief, because every brief says "read `CLAUDE.md` first, in full". About a dozen lanes ran on
2026-08-04 alone.

**It grew 336 lines in one day, across 11 commits.** Two sections are 46% of it: the roles section at
224 lines and the naming section at 118.

**And the cost that matters is not tokens.** `CLAUDE.md` warns against `pkill`-ing QEMU and against
`git reset --hard` to take a measurement, once each. In one day **one lane killed another lane's
emulator mid-test with `pkill -f`, and four agents clobbered work** with `reset --hard`, `checkout`
or `stash`. Every one of them had been told to read the file in full.

So the warnings are present and were skipped. **Length is already costing compliance**, which is a
measurement rather than an aesthetic complaint.

## The diagnosis: not too long, wrongly stratified

Everything loads at once with equal weight, so "never `pkill` QEMU" competes for attention with the
`snake_case` conventions table. A reader skimming 738 lines cannot tell which three rules will bite
them this hour.

**The reasoning is why the rules stick.** "The `sed` that rewrote the very row recording that the name
had been refused" is what makes that rule memorable, and compressing it to a bullet would produce a
style guide nobody obeys. **So this milestone stratifies rather than compresses**, and a lane that
finds itself deleting arguments has taken the wrong turn.

There is also an uncomfortable connection to the third principle: a stranger's first encounter with
this project is a 738-line document addressed to an agent. Milestone 117 will find that
independently.

## Three pieces

### Split, the way this tree has split three monoliths already

`DECISIONS.md` became `design/decisions/` (milestone 114), the roadmap became `design/roadmap/`, and
`notes/` is indexed. `CLAUDE.md` is the last monolith and the one loaded most often.

A **core** of the rules that change behaviour on *every* task, with linked documents for the rest.
The roles section and the naming section are the obvious first moves at 46% of the file between them.
**The test is not a line count but whether an agent will genuinely read the whole core**, so the
lane should say how it judged that rather than picking a round number.

### The most-violated rules stop being prose

This is the ladder turned on itself. "Do not `pkill` QEMU" and "do not `reset --hard` to take a
measurement" are **rung four**, prose relying on memory, and they failed five times in one day. Both
have a higher rung available:

- a wrapper that finds and kills only the caller's own emulator, so the dangerous form is never the
  convenient one;
- `git show <sha>:<path>` as the read-only way to look at another revision, which is what every one
  of those four agents actually wanted.

**A rule that is violated repeatedly is not stated too quietly. It is on the wrong rung.**

### A budget, so this does not have to be done again

A one-time cut re-grows; this file added 336 lines in a day without anyone deciding to. So the lane
adds a **gate on the core's size** to `script/lint`. Crude, and that is the point: it converts "should
I add this rule?" into "**what does this replace?**", which is the question nobody asks unaided.

**Pair it with the signal that actually matters, which is not size.** A rule nobody breaks is cheap at
any length; a rule that gets broken is either mis-stated or on the wrong rung. So keep a short ledger
of **times a documented rule was violated anyway**, with the rule named. Three strikes and it must
move up the ladder or be deleted as unenforceable.

The evidence for that ledger already exists in lane reports, honestly self-declared: *"I killed one of
dev-97's QEMU processes by mistake"*, *"I clobbered my own working tree"*. Those reports are the
input; nothing currently reads them.

## Scope note

**Not a rewrite, and not a trim.** No argument in that file is deleted; the reasoning is the asset.
Text moves, and only the rules that need to change rung change form.

**The budget number is provisional and belongs to whoever builds this**, informed by what the core
actually needs rather than chosen first and filled to.

**The honest limit**: a size gate measures the wrong thing on purpose. It cannot tell a rule that
earns its lines from one that does not, and a lane that games it by moving text into a linked file
nobody reads has satisfied the gate and defeated the milestone. The ledger is the counterweight and
it is weaker than a gate, because it depends on lanes continuing to report their own mistakes
honestly, which is a culture rather than a mechanism. **Say so where the reader meets it.**

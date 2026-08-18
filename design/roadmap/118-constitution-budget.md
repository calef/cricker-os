# 118. CLAUDE.md has a budget, and the rules that get violated move up the ladder

**Status: NOT-STARTED.** Minted 2026-08-05 by calef, who noticed the file had gotten huge and asked
what that costs.

**Gate: NONE.** The measurement is already taken and the first cut needs one lane.

## What it costs, measured 2026-08-05

**738 lines, 8,306 words**, roughly 11,000 tokens, loaded into every session and into every lane
brief, because every brief says "read `CLAUDE.md` first, in full". About a dozen lanes ran on
2026-08-04 alone.

**Re-measured 2026-08-17: 868 lines, 10,001 words** (`wc -lw AGENTS.md`), so it has grown another 130
lines and 1,695 words in the twelve days this milestone has been open. The premise got stronger, not
weaker, which is the argument for the budget rather than against it. The 2026-08-05 figures above are
left as the measurement calef acted on.

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
`snake_case` conventions table. A reader skimming it cannot tell which three rules will bite them this
hour, and there are 130 more lines of it than when that was written.

**The reasoning is why the rules stick.** "The `sed` that rewrote the very row recording that the name
had been refused" is what makes that rule memorable, and compressing it to a bullet would produce a
style guide nobody obeys. **So this milestone stratifies rather than compresses**, and a lane that
finds itself deleting arguments has taken the wrong turn.

There is also an uncomfortable connection to the third principle: a stranger's first encounter with
this project is a document of this length addressed to an agent. Milestone 117 found that
independently, in both of its runs.

## The audit, 2026-08-18: which rules have a mechanism, and which are the budget

calef asked for the measurement this block had been describing: **count the rules, count how many
have a mechanism behind them, and treat the rest as the budget.** Done 2026-08-18, and it changes the
shape of the work rather than confirming it.

**868 lines, 10,001 words, 60,570 bytes, 16 sections.** 54 line-leading bolded claims, of which
**roughly 33 are rules** and the rest are argument, evidence or framing. Against that, `script/lint`
alone runs **32 named checks**, and `roadmap`, `decisions`, `names`, `citations`, `audits`, `verify`
and the suite run more.

**So the file is not mostly unmechanised prose.** That is the finding, and it is not what "the file
has gotten huge" implies.

### Rules that already have a mechanism, where the prose could shrink

| the rule | what enforces it |
|---|---|
| names are calef's; crates and modules in scope | `script/names`, `--check` in `script/lint` |
| `snake_case` for Rust things, hyphens for scripts | lint's naming-conventions check |
| what two binaries share is a crate, not a `#[path]` module | lint counts `#[path]` consumers and fails at two |
| architecture code stays under `arch/` | lint's rule-1 check |
| delete a lane's branch at merge | `delete_branch_on_merge`, so the platform does it |
| every fence names its counterpart | lint |
| benchmarks are first-class; measure, do not argue | the icount tripwire, and `script/icount` since milestone 78 |
| `nifefs` caps archive names at 32 bytes | the format, enforced by the compiler |

Eight rules whose paragraphs can become a sentence and a pointer, because the mechanism is the
argument now.

### Two rules whose prose has gone stale, which is the opposite failure

**The citations paragraph is wrong.** It says *"after any renumber, check citations by content, not by
running the gate"*, because `script/decisions --check` proves a `§N` resolves to *some* section and
never the right one. **Milestone 97 built that gate.** `script/citations` opens by naming exactly that
blind spot and closing it, and it caught a wrong gloss on pull request #305 on 2026-08-18. So the
constitution is instructing agents to do by hand what a check now does, which is worse than a rule
with no mechanism: it teaches distrust of a working gate.

**"Never squash-merge a branch" is prose against a permissive setting.** `allow_squash_merge` is
`true` on this repository, so the platform is configured to allow the thing the constitution forbids.
**Turning it off makes the rule unrepresentable**, which is rung one for one API call, and the
paragraph becomes an explanation rather than a prohibition anybody can violate. The reason is worth
keeping either way: milestone 96's lane put the loader unification in its own commit *ahead of* the
migration precisely so a boot failure could not be ambiguous between two changes, and a squash-merge
destroys that.

### The budget: four rules that could have a mechanism and do not

These survive only because somebody remembers, and **the first two both failed on 2026-08-18**:

1. **"The maintainer starts the two watchers at the beginning of every session."** The drain died when
   the maintainer pruned the worktree it was running from, then printed `No such file or directory`
   every 150 seconds for three hours while looking exactly like a quiet queue. `notes/merge-queue.md`
   already records that neither script reports its own death; this is that entry arriving.
2. **"Prune a lane's worktree the moment its pull request merges."** Nineteen worktrees and 44 GB had
   accumulated before anyone looked, on a machine that hit zero bytes free once already.
3. **"Squash against the base commit you branched from, never `origin/main`."** A recorded scar with
   no check: `origin/*` is not a fixed point in a worktree, and the failure stages other lanes' files
   as your own.
4. **`needs-architect` enforcement.** §88 is `PROPOSED` and unbuilt, so the label is enforced by *one
   script choosing not to arm it*. Nothing stops a merge by another route, and nothing stops work
   merging if the label was never applied. On 2026-08-18 a second session re-applied a label the
   maintainer had removed, which is the coordination half of the same gap.

**Everything else unmechanised is judgement and should stay prose**: the top-up rule, the handoff
rule, lane count against the collision surface, "correct yourself loudly", "push back when he is
wrong", "explain on request". A gate over any of those would be a gate about taste.

### The clause the tenet needs

calef, 2026-08-18: **prefer gates over prose in this file.** With one qualification the same day
earned: **a gate is only better when it can be right about the tree, so record what it measured when
you built it.** The branch-prefix check rejected legitimate work four times and was widened after each
one; `script/lint`'s own justification for a disabled check had rotted from "100% false positives" to
no longer true, and nobody noticed because a justification is not a measurement anybody re-runs. A
gate with its measurement beside it can be told stale from live; one without it gets deleted by
whoever it inconveniences.

## Three pieces

### Split, the way this tree has split three monoliths already

`DECISIONS.md` became `design/decisions/` (milestone 114), the roadmap became `design/roadmap/`, and
`notes/` is indexed. `CLAUDE.md` is the last monolith and the one loaded most often.

A **core** of the rules that change behaviour on *every* task, with linked documents for the rest.
The roles section and the naming section are the obvious first moves at 46% of the file between them.
**The test is not a line count but whether an agent will genuinely read the whole core**, so the
lane should say how it judged that rather than picking a round number.

### The first cut: delete the eight rules a gate already enforces

calef, 2026-08-18, asked whether a rule with a mechanism can simply be eliminated. **Yes, and the
reasoning does not go with it, because the reasoning is already somewhere better.**

**The evidence is what the gates' own comments contain.** `script/lint`'s rule-1 check carries twelve
lines: the scar that produced it (a raw `SPSel` read in `user/tests.rs`), the three spellings
architecture code has in Rust, why there is deliberately no allowlist, and the precedent for what to
do instead (`current_sp` and `sync_icache`, where the register read moves behind an `arch::` helper).
`AGENTS.md`'s version of the same rule is three sentences with none of that. The naming and dead-code
checks carry eight and twelve lines respectively.

**So this is deleting a duplicate, not an argument**, which is the distinction that keeps it inside
this block's own warning that a lane deleting arguments has taken the wrong turn.

**And the gate is the better home, not merely an equal one.** A rule in `AGENTS.md` is read at session
start by an agent that cannot know which three rules will bite it that hour. A rule in a gate's
comment is read by somebody who has just tripped it. On 2026-08-18 `script/lint` refused a lane's
crate-level `allow(dead_code)`, and that lane found the *right* fix (`icount = ["bench"]`, riding
conditions that already existed) because §38's reasoning was there when the check fired. That is rung
three, and it is milestone 115's shape exactly: the record beside the thing rather than in a registry.

**The test before deleting each one, and it is falsifiable.** *Does the gate's failure message plus
its comment tell somebody enough to fix it correctly, including the case where the gate is wrong about
the tree?* The counted-claims check passes: it prints "fix the number, **or fix the derivation** if the
tree is right and the gate is asking the wrong question". The branch-prefix check failed it, which is
why it was widened four times instead of fixed once. **Where the constitution holds something the gate
lacks, move it to the gate first; then delete.**

**No pointer left behind.** A "see `script/lint`" line is a tax on every session for a rule that can
no longer be violated silently. The eight are listed in the audit above; the paragraphs are on the
order of 90 lines, about a tenth of the file, and they are the tenth that needs the least reading.

**What this does not touch.** The four budget rules have no mechanism, so deleting their prose deletes
the rule. They move *up* the ladder (next subsection) rather than out of the file. And the judgement
rules stay prose, because a gate over "push back when he is wrong" would be a gate about taste.

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

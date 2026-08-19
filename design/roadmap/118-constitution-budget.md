# 118. CLAUDE.md has a budget, and the rules that get violated move up the ladder

**Status: PARTIAL.** Minted 2026-08-05 by calef, who noticed the file had gotten huge and asked
what that costs. Two lanes have run: #309 took the audit, and a second (2026-08-18) verified it and
produced an applicable cut. **The cut is proposed, not applied**, because a developer may not edit
`AGENTS.md`; the integrator applies it. The split, the size gate and the violation ledger are
untouched, so this stays open. **PARTIAL rather than IN-PROGRESS**: no lane is on it as this is
written, and the three remaining pieces are startable independently of each other.

**Gate: NONE.** That is now a recorded weakness rather than a description: the size measurement
is `wc -lwc AGENTS.md` and nothing runs it, which is the milestone's own budget piece arguing for
itself: this block has carried three different sizes in thirteen days, each one hand-entered by
whoever last looked. A size gate would make the number self-maintaining.

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
| ~~`snake_case` for Rust things, hyphens for scripts~~ | **wrong, corrected below**: no such check exists |
| what two binaries share is a crate, not a `#[path]` module | lint counts `#[path]` consumers and fails at two |
| architecture code stays under `arch/` | lint's rule-1 check, **but only partly**: see below |
| delete a lane's branch at merge | `delete_branch_on_merge`, so the platform does it |
| every fence names its counterpart | lint |
| benchmarks are first-class; measure, do not argue | the icount tripwire, and `script/icount` since milestone 78 |
| `nifefs` caps archive names at 32 bytes | the format, enforced by the compiler |

Eight rules whose paragraphs can become a sentence and a pointer, because the mechanism is the
argument now.

**Four of those eight did not survive verification.** This table was assembled from `script/lint`'s
banner list, and a banner names a check's subject rather than its coverage. The second lane read
each gate's body and recorded the result under "The applicable cut" below; two rows are struck
through or qualified above. Read that section, not this table, before deleting anything.

### Two rules whose prose has gone stale, which is the opposite failure

**The citations paragraph is wrong.** It says *"after any renumber, check citations by content, not by
running the gate"*, because `script/decisions --check` proves a `§N` resolves to *some* section and
never the right one. **Milestone 97 built that gate.** `script/citations` opens by naming exactly that
blind spot and closing it, and it caught a wrong gloss on pull request #305 on 2026-08-18. So the
constitution is instructing agents to do by hand what a check now does, which is worse than a rule
with no mechanism: it teaches distrust of a working gate.

**"Never squash-merge a branch" is prose against a permissive setting.** `allow_squash_merge` was
`true` on this repository when this was written, so the platform was configured to allow the thing the
constitution forbids. (**Since fixed**: it is `false` as of #334, 2026-08-18, verified live by the
second lane.) **Turning it off makes the rule unrepresentable**, which is rung one for one API call, and the
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

## The applicable cut, 2026-08-18, second lane

The audit above named eight rules with a mechanism. **This lane checked each claim against the tree
and four of the eight did not survive contact**, which is the finding rather than a footnote: the
audit was assembled by reading `script/lint`'s banner list, and a banner names a check's subject, not
its coverage. What follows is only what was verified by reading the gate's body and testing its
claim against the files it scans.

**This lane did not edit `AGENTS.md`, and could not.** That file forbids a developer to edit it, so
the deliverable is a cut the integrator applies. Line numbers are against `AGENTS.md` at
`64c08ec9`, 955 lines; apply the cuts **bottom-up** so earlier ranges stay valid.

### The measurement

**955 lines, 11,062 words, 66,724 bytes** (`wc -lwc AGENTS.md`), against 738/8,306 when calef minted
this block on 2026-08-05 and 868/10,001 when it was re-measured on 2026-08-17. **+217 lines and
+2,756 words since minting, +29% on lines.**

The growth is not diffuse, and that is the useful half. Counts are of the file as it stood at each
commit that touched it (`git show <sha>:AGENTS.md | wc -lw`, following the 2026-08-14 rename from
`CLAUDE.md`):

| date | commit | lines | what it added |
|---|---|---|---|
| 2026-08-05 | (b0cf1245, 08-04) | 738 | the measurement calef acted on |
| 2026-08-13 | 7c65f10a | 748 | the ranking function is a customer running it |
| 2026-08-14 | 150628ac, 3b54f6fc | 768 | the `AGENTS.md` rename, then usernames as the standard |
| 2026-08-15 | 4f215acb, 5d00b8e9, 9d1d49fc, 6f71615f | 787 | milestone 120, the two-directional leak check, §71, plural maintainers |
| 2026-08-16 | 1ff2a038, daf5e84a, 23aadabe | 822 | lane count against collision surface, draft-PR-first, the agent-voice line |
| 2026-08-16 | 1da0d68b | 868 | the elegance-and-performance tenet (+46) |
| 2026-08-17 | 77017f71 | **955** | milestone 94's convention (+87) |

**Two commits are 61% of the growth since 08-05.** `1da0d68b` (+46) and `77017f71` (+87) added 133
of the 217 lines, and both are whole new tenet sections rather than accretion. The file does not
creep; it steps. That matters for the budget's design: a per-commit size gate would have fired
exactly twice in thirteen days, on the two commits whose authors most believed they were adding
something load-bearing.

**One commit shrank it.** `daf5e84a` took it 807 to 794 while adding the draft-pull-request rule,
by deleting what that rule replaced. That is the shape the budget is trying to make normal, and it
already happened once unprompted.

### Applied 2026-08-18 by the integrator: four cuts, 1,212 bytes

**Landed.** `AGENTS.md` went 955 lines / 11,062 words to 942 / 10,880. The three deletions removed
292, 377 and 543 bytes; cut 4 is a reword and saves nothing. Every line number in the proposal below
was still exact when it was applied, except rule 7's block, whose stated range included a trailing
blank line; applying by text match rather than by index is the lesson, and it is why this was applied
by search rather than by `sed`.

The two rewordings the proposal said an integrator would have to write:

- *"The rule that matters more than the tidiness:"* became a standalone opener, **"An unmerged branch
  is either abandoned..."**, since the paragraph it referred back to is gone.
- *"Three reasons, and the second is the one that matters"* became **"Two reasons, and `script/lint`
  check 5 carries the third"**, which names where the deleted argument went rather than leaving a
  reader to wonder.

Cut 4 kept its prohibition and lost its overreach: *"Never squash across purposes"* stays a rule
because nothing gates it, and the squash-*merge* half now says the platform refuses it.

### Verified: four cuts, ~15 lines and ~1,150 bytes net

Small, and deliberately so. **The honest total is about 1.7% of the file**, against the audit's
estimate of "on the order of 90 lines, about a tenth". The gap between those two numbers is this
lane's actual result.

#### Cut 1: delete lines 508-511, the stale citations paragraph (293 bytes)

> **After any renumber, check citations by content, not by running the gate.** `script/decisions
> --check` verifies that a cited `§N` resolves to *some* section, never that it resolves to the right
> one, so a well-formed wrong citation is invisible to it. This has already produced two of them.

**Mechanism: `script/citations --check`, wired into `script/lint` at line 565** under the `==>
citations` banner. Verified by reading the check's body: it compares a citation's parenthetical
gloss against the record's own title or body, and confirms an attributed block quote still exists in
the file it names. `script/lint`'s comment above it (lines 555-563) names the exact blind spot this
paragraph describes and says it is closing it.

**Safe because the prose is not merely duplicated, it is wrong.** It instructs an agent to do by
hand what a gate now does, which teaches distrust of a working check. Nothing here is an argument
worth preserving: the reasoning (a well-formed wrong citation is invisible to a resolve-only gate)
is stated more fully in `script/lint`'s own comment and in `notes/citations.md`.

#### Cut 2: delete lines 544-548, the branch-deletion rule (377 bytes)

> **Delete a lane's branch when you merge it, and never use a branch as a filing cabinet.** Forty-seven
> branches accumulated in about two days of lane work ... So it belongs in the merge, not in a
> periodic cleanup.

**Mechanism: `delete_branch_on_merge` is `true` on this repository.** Verified live:
`gh api repos/crickertech/nife --jq .delete_branch_on_merge` returns `true`. The platform deletes
the branch, so the rule is rung one and cannot be violated by forgetting.

**The paragraph that follows must stay, and the integrator must reword its opening.** Lines 549-553
carry a different rule with no mechanism at all: *an unmerged branch is either abandoned or it is
holding knowledge that is not on `main`*, with `fix/redoxfs-write-loop` as its worked example.
That is ungated and stays. Its first clause currently reads "The rule that matters more than the
tidiness:", which refers back to the deleted paragraph; it becomes a standalone opener. **This is
the only cut in this set that requires the integrator to write a word rather than delete one.**

#### Cut 3: delete lines 686-692, rule 7's second reason (546 bytes)

> **A `#[path]` module inside a `no_std` binary is unreachable by host tests and by Kani.** ... `cseam`
> is the case that proves it ... A drift there shows up as a C component scribbling on the wrong page.

**Mechanism: `script/lint` check 5** (lines 1174-1212), which counts consumers per `#[path]` include
target and fails at two. Verified by reading it. Its comment carries this reason **verbatim in
substance**, including the `cseam`/`c_seam.c` worked example and the drift-as-wrong-offset
consequence, plus something `AGENTS.md` lacks: that a single-consumer module is fine and stays fine,
with `net_transport` and `socket_test_client` named as today's instances.

**Only this reason goes, not the rule and not the other two.** The rule statement (676-678) stays,
because a reader needs to know the rule before they trip the gate. Reason 1 (682-684, "it removes a
category that nothing enforces") and reason 3 (693-694, "makes location self-enforcing for free")
stay, because **neither appears in the gate's comment** and deleting them would delete an argument.
Line 680's "Three reasons, and the second is the one that matters" becomes "Two reasons", which is
the second word the integrator writes.

**The recorded caveat on this cut**: the gate scans `user/src` only. `fs_server/` also declares
three `[[bin]]` targets, so a `#[path]` module shared between two of them would be invisible.
`fs_server/src` contains no `#[path]` today (verified by grep), so the gap is latent rather than
live, but it is real and it belongs in the gate's `BUGS`-equivalent rather than in nobody's head.

#### Cut 4: reword line 878, the squash-merge prohibition (a reword, not a saving)

**Mechanism: `allow_squash_merge` is now `false`.** Verified live via `gh api`. **This corrects the
audit above**, which recorded it as `true` and proposed turning it off; that landed in the meantime
(#334, the commit this lane branched from). Squash-merging is now unrepresentable, which is rung one.

So *"and never squash-merge a branch"* becomes an explanation rather than a prohibition. **"Never
squash across purposes" stays a prohibition**, because nothing gates it: a lane can still collapse
two purposes into one commit locally, and milestone 96's loader-ahead-of-migration structure is
exactly what that would destroy. The milestone 96 reasoning (879-883) stays either way.

This one saves almost nothing. It is in the cut because a prohibition against something the platform
already refuses reads as a live hazard, and a reader who believes the constitution is guarding
something the settings guard will not check the settings.

### Declined: five rules examined and not cut

**This is the more informative number**, and four of the five were on the audit's list of eight.

| rule | why not |
|---|---|
| **rule 1, arch code stays under `arch/`** (643-646) | The gate greps `asm!\(` and `core::arch::` under `kernel/src`. Rule 1 also claims **system registers**, and `kernel/src/main.rs:733` and `:1123` read `aarch64_cpu::registers::CurrentEL` outside `arch/` today. Both are invisible to the gate. Cutting the prose would silently unenforce the clause that the check's own comment says the discipline already slipped on. **Extend the gate to the register crates first, then cut.** |
| **`snake_case` for Rust things, hyphens for scripts** (776-818) | **The audit's claim is false.** `script/lint`'s naming-conventions block runs seven checks and none is a case convention: no `-d` suffix, no "daemon", `*_proto` spelling, branch prefixes, milestone-branch-touches-roadmap, `#[path]` consumers, provenance presence. The only case gate is check 6, and it covers **one of the six rows** of that table (`notes/`, `design/` markdown filenames). Crates, programs, modules, `script/` entry points, repo-root markdown and directory names are ungated. |
| **names are calef's call** (700-775) | `script/names --check` verifies a name carries **a provenance state**, and `script/lint`'s own comment is explicit that it checks "presence of a STATE, never the state `ratified`", precisely so the gate is not a wall. 54 of 126 names are unratified. The gate does not enforce the rule; it records whether the rule was applied. |
| **benchmarks are first-class; measure, do not argue** (555-558) | The icount tripwire catches a performance regression. The rule is about **honesty in reporting**: state what a number means, name where it is not apples-to-apples, prefer a recorded tie to an overclaimed win. No gate reads a claim's honesty. This is judgement and stays prose. |
| **`nifefs` caps names at 32 bytes** (820-825) | The constant is real (`crates/nifefs/src/lib.rs:118`, `pub const NAME_LEN: usize = 32`) and the compiler enforces the cap. But the paragraph's operative content is advice the compiler cannot give: *do not let it pick a name, do not spend a format change on bytes nothing needs*. A build failure tells you the name is too long; it does not tell you to pick a different name rather than widen the format. **Could shrink to two lines; not a deletion.** |

### What this lane did not get to

Named as remaining work rather than guessed at, because a proposed deletion that turns out to be
unenforced is the failure this milestone exists to prevent.

- **"Every fence names its counterpart"** was on the audit's list. There is a gate (`script/lint`
  line 268), but this lane did not find the corresponding *prose rule* in `AGENTS.md` to cut: rule 4
  (664-666) is "assume weak memory ordering", which is judgement, not the fence-pairing bookkeeping
  the gate checks. Either the audit row names a rule that is not in the file, or it is in a section
  this lane did not read. **Unresolved.**
- **The split** (the core-plus-linked-documents piece) is untouched. So is the size gate and the
  violation ledger. Those are the three remaining pieces.
- **The four budget rules** are unchanged: the watchers, worktree pruning, squashing against the
  base SHA, and `needs-architect` enforcement all still have no mechanism.

### What this changes about the milestone's premise

**The file is not carrying much deletable duplicate.** Two lanes have now looked, and the verified
cut is 1.7%. The audit's own finding stands and is stronger than it reads: roughly 33 rules against
32 named `script/lint` checks plus six other gate scripts, and the overlap between those two sets is
much smaller than the counts suggest, because **the gates mostly check things the constitution never
claimed to rule on** (unsafe contracts, dead-code suppression, conflict markers, dependency
direction, counted claims).

So the growth problem is real and the duplication problem is mostly not. **The budget and the split
are where the value is**, and a future lane should not spend itself re-auditing for cuttable rules;
this lane and #309 have now covered that ground.

## Three pieces

### Split, the way this tree has split three monoliths already

`DECISIONS.md` became `design/decisions/` (milestone 114), the roadmap became `design/roadmap/`, and
`notes/` is indexed. `CLAUDE.md` is the last monolith and the one loaded most often.

A **core** of the rules that change behaviour on *every* task, with linked documents for the rest.
The roles section and the naming section are the obvious first moves at 46% of the file between them.
**The test is not a line count but whether an agent will genuinely read the whole core**, so the
lane should say how it judged that rather than picking a round number.

### The first cut: delete the eight rules a gate already enforces

**Superseded in part by "The applicable cut" above**, which executed this and found four of the
eight unenforced. The reasoning below stands and is why the cut is worth making at all; the
estimate of "on the order of 90 lines" does not, and the verified figure is about 15.

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

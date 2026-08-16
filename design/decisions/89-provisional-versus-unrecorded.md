# 89. Two words for an unratified name: `provisional` and `unrecorded`

**Status: PROPOSED.** (raised 2026-08-16 by milestone 117's second stranger run, which wrote the
word the rules told it to write and got a red gate for it.)

**Number is provisional**: §87 was in flight in an unmerged pull request when this was written, so
the integrator may renumber at merge.

**What happened, verbatim.** A newcomer added a program, read AGENTS.md's repeated instruction that
a lane "ships a **provisional** name and says so", wrote exactly that in the program's provenance
block, and `script/names --check` refused it:

```
names: program doubler: the block must start `ratified <date>` or `unrecorded`
       (found '**provisional**. Introduced 2026-08-16 while wal')
```

**The clash.** Milestone 115 gave the tree three provenance states, and they are machine-checked:
`ratified <date>`, `recorded`, `unrecorded`. AGENTS.md, in four separate places, tells a contributor
to ship a *provisional* name. Both are right about their own subject and they do not use the same
word for it, and only one of them is enforced. Two programs in the tree already work around it by
writing `unrecorded, and explicitly **provisional**`, which is the tell: a workaround appearing twice
is a convention forming in the dark.

**They are not quite the same idea, which is why this needs a ruling rather than a `sed`.**
`unrecorded` is a claim about the *record*: nothing outside this block says why the thing is called
that. `provisional` is a claim about *intent*: whoever chose it expects it to change. A name can be
`unrecorded` and settled (nobody wrote down why `hello` is called `hello`, and nobody needs to), and
a name can be recorded and still provisional. The states are orthogonal, and the tree currently
spends one word on both.

**Three ways out.**

1. **A fourth state, `provisional`,** accepted by `script/names`. Honest about intent, and it makes
   the naming backlog a query: `script/names --provisional` is the list calef would actually work
   from. Cost: another state to explain, and a lane that writes `provisional` forever never has to
   confront that nothing recorded its reasoning.
2. **AGENTS.md adopts the gate's word** and says "ship it as `unrecorded`, and say in your report
   that the name is provisional". Cheapest, no code, keeps three states. Cost: it puts the intent
   back into a report, which is rung four, and reports are exactly where milestone 115 found naming
   decisions going to die.
3. **Keep both words and make the block carry both**, which is what the two workaround programs
   invented: the state stays `unrecorded` and the sentence after it says provisional. Formalize it in
   `notes/adding-a-program.md` and in the gate's error message. Cost: the intent is prose, so nothing
   can query it.

**The recommendation is 1**, and the reason is milestone 115's own: a naming backlog that a machine
can list is a backlog that gets worked, and one that lives in pull request reports is the thing that
milestone existed to abolish. `provisional` is also the word calef already uses, and a mechanism that
spells its rule differently from the person who wrote the rule is a mechanism people get wrong.

**If it is refused**, option 3 is the fallback and costs almost nothing: the tree already does it
twice, and writing it down turns a workaround into a convention. Option 2 is the one to avoid, since
it is the only one that moves a fact *out* of the artifact and into a report.

**Blocked on nothing.** `notes/adding-a-program.md` now tells a newcomer to write `unrecorded` and
cites this decision, so the trap is documented either way. What this decides is whether the tree's
two vocabularies become one, and which one wins.

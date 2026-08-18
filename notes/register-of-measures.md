# The register of measures: every number this kernel owes itself

*(Milestone 134. The name `register-of-measures.md` is **provisional**; naming is calef's, and a
lane ships a provisional one and says so.)*

This tree measures a great deal and remembers almost none of it. A number gets taken once, written
into a note beside the reasoning that needed it, and then sits there being true on the day it was
written. `notes/counted-claims.md` found three such numbers on 2026-08-14 and **all three were
wrong**; every one had been right when somebody typed it.

That convention fixed the class of number a `grep` can re-derive. This register is the other half:
the numbers that need an instrument, a boot, or a walk over the source. It says which ones this
kernel is holding itself to, which ones it merely knows, and which ones it has defined and cannot
yet take.

## What belongs here, and the test

**A number belongs if something depends on its value and it can move without anybody editing it.**

Both halves are load-bearing and the second is the one that cuts.

- *Something depends on its value.* Not "somebody would find it interesting". A decision rests on
  it, a constant is sized against it, a claim in the documentation quotes it, or a customer notices
  when it moves. `manual::render::LINE_MAX` is 2048 because the longest markdown line was 1841, so
  that measurement has a **consumer**; the kernel's image size, which
  `notes/benchmarks.md` itself calls "the number that does not matter", has only a reader.
- *It moves on its own.* A constant somebody chose is not a measure, it is a decision, and it
  belongs in `DECISIONS.md`. The stack guard page is 4,096 bytes because a page is 4,096 bytes. The
  deepest chain that can reach that guard is a measure, because the compiler moves it every week
  and nobody is asked.

**A register that lists every number in the tree is worthless**, so the exclusions are as much of
this document as the rows are, and each one names the test it failed. They are in their own section
below rather than implied by absence.

## The three states, and the middle one is the finding

Every row is in exactly one of these. The state is a property of the **instrument**, not of the
measure's importance.

| state | means | what happens when the number moves |
|---|---|---|
| **gated** | an instrument re-takes it, and something fails on a bad move | a red build, at the commit that moved it |
| **dated** | a named command re-takes it; nothing fails | the recorded value goes stale, silently |
| **owed** | defined, with its instrument named; no instrument exists yet | nothing, because nothing is measured |

**The `dated` rows are the answer to the question this milestone was raised to ask.** They are the
numbers something depends on where a regression arrives as somebody's data being slow rather than
as a red check. Promoting one to `gated` is the work; recording that it is `dated` is what makes the
work visible.

`dated` is not a defect by itself. `notes/counted-claims.md` puts it plainly: *"A wall clock is not
a count... dating a measurement is the honest alternative to gating it, and the two should not be
confused."* A number that costs a forty-minute boot to re-take does not belong in a gate that runs
on every push. The defect is a `dated` row with no date, or with no command.

## Gated

Nine instruments, and it is worth seeing them in one table because four of them are the same shape:
a **ceiling** that fires when a number grows. That shape was in this tree four times before anything
named it, which is why milestone 134 added `count-at-most` rather than inventing a mechanism.

| measure | instrument | what fails |
|---|---|---|
| icount ticks, 14 benchmarks, both ISAs | `script/bench --check` | drift over 10% from `bench/baseline-*.txt` |
| IPC fastpath instruction footprint | `script/fastpath-footprint --check` | growth over 5% from `bench/fastpath-*.txt` |
| the largest kernel stack frame | `script/stack-frame-check` | any frame over the 4,096-byte guard page |
| the deepest reachable kernel-thread chain | `script/stack-depth-check` | a chain over the 24,576-byte stack |
| kernel stack high-water, at runtime | `script/test`, `report_high_water` | boot 61,440, secondary 16,384, thread 18,432 |
| eleven counted claims (harnesses, syscalls, rights bits, ...) | `script/lint` | a marked number disagreeing with the tree |
| unsafe density outside `kernel/src/arch/` | `script/lint` | over 100 blocks per 10,000 lines of code |
| `unsafe impl Send`/`Sync` claims | `script/lint` | over 17, which is today's tree exactly |
| per-file line coverage | `script/coverage` | any file under the 80% floor |

The four ceilings are rows 2, 3, 4 and 7, and each one's threshold is a different kind of thing:
5% is a tolerance, 4,096 is a hardware fact, 24,576 is a configuration constant, and 100 per 10,000
is a **claim about the tree that was false until two days before it was written**. Only the last is
a direction rather than a limit, which is what the `count-at-most` relation exists to express. See
notes/unsafe-obligations.md for the measurement behind it.

Two of these are the register doing its job on itself: the unsafe rows did not exist when milestone
134 opened, and the `unsafe fn` count that would have been a third turned out to be **already
derived** by `script/lint`'s `==> unsafe fn contracts` check. Finding a number already tracked is as
much a result as finding one that is not.

## Dated

The command is the point of each row. A dated measurement whose re-taking is folklore is a `dated`
row pretending to be one.

| measure | last taken | the command that re-takes it |
|---|---|---|
| IPC round trip in nanoseconds, both planes | 2026-08-04 | `script/bench --real` |
| filesystem throughput, milestone 38's four phases | 2026-08-18 | `script/bench --real --smp`, with a RedoxFS disk attached |
| primitives against Linux and macOS on the same host | 2026-07-29 | `bench/host/run_linux.sh`, then `script/bench --real` |
| `unsafe {}` blocks inside `kernel/src/arch/` | every run | `script/lint`, which prints it and asserts nothing |

**The filesystem row is the one on the customer path**, and it is the clearest case in the register
for why `dated` is a finding rather than a filing. Milestone 55 is a Time Machine target the
family's Macs back up to. A three-times regression in sequential write would show up as a backup
that used to finish overnight and now does not, reported by a person rather than by CI, and nothing
in this tree would have said a word. It is `dated` because taking it needs a boot with a disk
attached, which is not a thing to put on every push; the honest promotion is a scheduled run rather
than a gate, and it wants a lane.

**The arch row is the odd one and it is deliberate.** There is no ceiling on unsafe inside
`kernel/src/arch/`, because driving that number down means either writing assembly wrong or moving
it out of `arch/`, and rule 1 says arch code belongs there. A target would be a gate pushing against
the architecture. But an unmarked number in a note is exactly the snapshot this whole register is
against, so `script/lint` prints it on every run: on screen every build, asserted never. **A number
with a consumer gets a relation; a number with only a reader gets printed.**

## Owed

Twelve measures are defined and cannot be taken here: four that need an experiment nobody has built
(E1 through E4, all runnable on the dev machine today) and eight that need the cycle counters of
milestone 74, the authority question of milestone 75, or silicon with a real PMU.

They are **not duplicated into this table**, because they already have a home that carries each
one's instrument, its prediction, and what its outcome settles:
design/roadmap/134-the-measurements-that-decide.md. Two open kernel decisions are waiting on them,
and the block's own correction is worth knowing before anyone reaches for hardware: §95 and §96 both
recommend waiting for the TX1, and **both over-gated**, because the experiments that produce a
verdict need no silicon.

## Deliberately not in this register

Each of these was considered and each names the half of the test it failed. The list is here so the
next person does not add them back.

| number | why it is out |
|---|---|
| the kernel's image size (290,816 bytes on aarch64) | **no consumer.** notes/benchmarks.md derives it and then says in its own heading that it is "the number that does not matter": `.text` that never runs during an IPC costs nothing in cache |
| `script/verify`'s wall clock (~47 minutes) | **no consumer.** It is a reader's patience, not a constraint anything is sized against, and notes/verification.md dates it honestly |
| lines of Rust, crates, user programs, commits | **no consumer.** AGENTS.md's method figures are rhetoric about scale, and that file says so; a gate on them would be measuring a paragraph |
| `nifefs`'s `NAME_LEN = 32` | **does not move on its own.** It is a decision with a cost per directory block, not a measurement |
| the number of `#[cfg(kani)]` unsafe blocks (14) | **already gated**, by milestone 113's fourteenth clippy configuration, per block rather than in aggregate |
| `unsafe {}` against `// SAFETY:` parity | **measured and refused.** `clippy::undocumented_unsafe_blocks` already enforces it per block as a hard error, and a count comparison disagrees with it in 65 places, every one of them a document that is right. notes/unsafe-obligations.md carries the reading |
| the CoreMark score | **already gated**, as a row in `bench/baseline-*.txt` |

The parity row is the one worth reading before proposing a new gate. A count check that fails
correct documents is not a weak gate, it is a gate that will be deleted, and `script/lint` has
already lost three checks with that signature.

## EXAMPLES

### Adding a measure to the register

Take the unsafe census, from calef's question to a gated row, because every step of it went
differently than expected.

**1. Apply the test out loud.** Does anything depend on the amount of unsafe in this tree? Yes: the
whole demonstrator claim is a verified-Rust capability microkernel, and unsafe is where verification
stops. Does it move without anybody editing it? Yes, 42 non-merge commits changed it in fourteen
days. Both halves pass.

**2. Take the number, and take it more than once.** A single measurement cannot tell a direction
from a level, and here it inverted the answer:

```sh
# blocks outside kernel/src/arch/, at four points in the tree's history
2026-07-15   171 blocks in   7,508 lines    22.8 per 10,000
2026-08-04   723 blocks in  58,805 lines    12.3 per 10,000
2026-08-16   817 blocks in  73,129 lines    11.2 per 10,000
2026-08-18   747 blocks in  80,359 lines     9.3 per 10,000
```

The count more than quadrupled and the density more than halved. A ceiling on the count would have
fired on nearly every lane; a ceiling on the density holds a trend that is already going the right
way.

**3. Choose the relation from the shape of the quantity, not from taste.** Equality for a census
somebody maintains, `count-at-least` where more is better and a deletion is the bad event,
`count-at-most` where less is better and a drift up is. See notes/counted-claims.md.

**4. Watch it fail.** This is not optional and it is where the two real bugs were:

```sh
# add one `unsafe impl Send` anywhere, then:
$ script/lint
lint: a counted claim disagrees with the tree:
  notes/unsafe-obligations.md:461: claims at most 17, the tree has 18
  (unsafe-thread-safety-claims: how many `unsafe impl Send`/`Sync` claims the tree makes, each one
  a hand-written assertion that the compiler is wrong about a type). A ceiling is only wrong when it
  stops being true, so this means the count went UP by 1 past the headroom. Take the addition back
  out, or raise the ceiling in this commit and say beside it why the addition was worth it
```

The density ceiling's first marker **did not fire when it should have**, and the reason is the sort
of thing only a deliberate failure finds. It was written as `at most 91 blocks per 10,000 lines`,
and the convention binds a marker to the **last** number on the line, so the gate was comparing
10,000 against 92 and passing every time. The marker now sits immediately after its own number.

### Re-taking a dated measure

There is no wrapper and there should not be one: each dated row's command is in its table cell
because the commands are genuinely different animals, and a `script/measures` that ran all of them
would take an hour and be run by nobody. Copy the cell.

```sh
# the filesystem row, which needs a disk attached
script/bench --real --smp

# then edit the date in this file's table, in the same commit as the numbers
```

If the number moved, **the finding is the movement**, not the new value. Say what moved and against
what, in notes/benchmarks.md where the series lives, and leave this register holding only the date.

## BUGS

- **A `dated` row goes stale silently, which is the whole point and is also the limitation.** This
  register makes the staleness visible to a reader who opens the file; it makes it visible to
  nobody else. Nothing checks that a date is recent, and a check that did would be asserting a
  policy nobody has set. If a row's staleness starts to matter, the fix is to promote it to
  `gated`, not to add a freshness gate.

- **The register is a ratchet, like the convention it extends.** A measure nobody adds is not
  tracked, and "the register is complete" is never a thing anybody can say. It grows as people
  notice numbers, which is the same honest boundary `notes/counted-claims.md` records.

- **`patches/std-nife/overlay/` is outside the unsafe census, and it is our code.** Thirty-seven
  `unsafe {}` blocks in the `std` platform layer are counted by nothing here. Two separate reasons,
  and only the first is a decision: a ceiling asserts a direction, and that code implements `std`'s
  internal interfaces, so it cannot be restructured to hold fewer unsafe blocks without diverging
  further from the crate we track. The second is worse and is not a decision at all: **that code is
  compiled into `std` by the farm and never by a clippy configuration here**, so
  `undocumented_unsafe_blocks` and `unsafe_op_in_unsafe_fn` do not reach it either. Fifteen of its
  blocks have no `SAFETY:` comment in the form the lint wants, and nothing has ever said so. That
  is a coverage hole in the lint policy rather than a gap in this register, and it wants a lane.

- **Unsafe density can be diluted by writing more safe code, and nothing stops that.** The
  denominator is non-blank lines after comments and string literals are stripped, so prose cannot
  move it, but a verbose safe refactor can. The counter-argument is that the effect is small at
  80,000 lines and that the alternative, a raw count, was measured and is worse. Watch the printed
  numerator, which `script/lint` prints beside the ratio for exactly this reason.

- **The unsafe derivation is a text scanner, not a parser.** It blanks comments and literals with a
  regex before matching keywords, which is what keeps fourteen `unsafe {}` written inside doc
  examples out of the count. Block comments are matched non-greedily and Rust's nest; the tree has
  no nested ones, and a nested one could only make the count too high, which fails loud. Same caveat
  as `script/lint`'s `# Safety`, dead-code and `#[path]` checks, which are built the same way.

- **Nothing here measures the verification argument, and nothing can.** Unsafe density says how much
  code is outside the compiler's guarantees; it says nothing about whether the invariants written in
  the `SAFETY:` comments are true. §61 already records that a lint checks a comment exists and never
  that it is right. A register of numbers is not a substitute for reading them.

- **The gated and dated rows are maintained by hand.** Nothing checks that
  `script/fastpath-footprint` still exists or that `script/bench --real --smp` is still the command,
  which makes this document exactly the class of artifact it was written to complain about, one
  level up. The mitigating fact is that `script/lint` already fails when a script in `script/` has
  no entry in notes/scripts.md, so a renamed instrument cannot vanish quietly from the tree, only
  from this table.

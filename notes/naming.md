# Naming things in nife

What the tree's names mean, which conventions are rules, and which of those a machine checks.

Written at milestone 46, alongside the rename that made four of the names honest. The headline rule
and its argument are [DECISIONS §39](../design/decisions/39-component-names.md); this note is the working reference and covers
the parts §39 does not: crates, scripts, where a document goes, and the two numbering schemes that
look alike and are not.

## The rule everything else is a corollary of

**A name is a claim, and it is made before a reader sees a line of code.** That is the whole
argument. A wrong name is the same defect as a stale comment, except that a comment can be skipped
and a name cannot: every reader of every call site reads it.

Two ways a name can be a false claim, and the tree had both:

- **It claims a model we rejected.** `netd`, `compd`, `gpud`, `termd`. The `-d` suffix says "Unix
  daemon", and a daemon is defined by what it detaches from: no controlling terminal, inherited
  ambient authority, a pid file, started by a privileged init. This OS has none of those. `netd` held
  five explicit capabilities, could not name its own callers, was supervised, and could be reaped by
  something that lacked the authority to build it. About as far from a daemon as a long-running
  process gets.
- **It claims a reader who does not exist.** `linedisc` was the correct Unix term of art. calef did
  not recognise it, and he built the system. That is evidence about the name, not about him. It
  became `lineedit` and then `line_editor`, which someone who has never opened a tty manual
  understands immediately.

So: **name a component for what it is, and prefer a word that parses without prior Unix exposure.**
`blk`, `spawner`, `console`, `input`, `painter`, `window`, `kbd` were always right, and were always
the majority. The four `-d` names were the outliers.

The shell is the one exception, and it is deliberate: it is called **`swish`**, not `shell`, because
shell names are identities rather than descriptions (`bash`, `zsh`, `fish`, `rc`). The argument is in
milestone 63's roadmap block. `capsh` was the obvious candidate and is unavailable: Linux's libcap
ships `capsh(1)`, a capability shell wrapper, so a reader arriving from Linux would assume ours is
that tool.

## Components

A **component** is the shippable unit: one binary in `user/src/`, one `[[bin]]` in `user/Cargo.toml`,
one entry in the initrd archive. A **service** is what a component offers. A **contract** is the wire
protocol it offers it over. "Server" is a fine role word inside a component (`fs_server` serves the FS
service). "Daemon" appears nowhere.

- Lowercase, `snake_case`, no suffix. `net_stack`, `compositor`, `display`, `line_editor`,
  `fs_subtree_caretaker`. One word where one word will do, an underscore where the name is a
  qualifier applied to a thing; the 2026-08-01 rule below retired the older "no separators" wording.
- **Never `-d`.** Not `netd`, not a future `logd` or `authd`. Checked.
- **`c_` means "written in C", and it spans two unrelated milestones.** `c_shim`, `c_seam` and
  `c_confiner` are milestone 36's foreign-language seam (DECISIONS §31); `c_swappable` is milestone
  23's replacement demo, the C half of the `rust_swappable` / `c_swappable` pair. The prefix means
  the same thing in both places and the milestones have nothing to do with each other, so do not
  read the four of them as one family.
- Abbreviate only where the abbreviation is the ordinary name of the thing: `blk`, `kbd`, `pci`. If
  you have to expand it in the doc comment to make the file readable, it was not the ordinary name.
- The binary name, the source file name and the archive entry name are the same string. `xtask`'s
  `mkinitrd` pairs them positionally in a flat array, so a mismatch is a runtime "program not found"
  rather than a compile error, which is exactly the kind of thing to keep boring.
- The one deliberate exception: `builder` is packed as `init`, because `init` is the entry the kernel
  loads by name. The name in the archive is a role; the name in `user/src/` is the program.

Fixtures and benchmarks (`heeder`, `spinner`, `flaky`, `allocator_exerciser`, `worker`, `coremark`,
`os_primitives_benchmarker`) live in `user/` next to the real components and are not components.
Milestone 39's directory-layout work is where that gets separated; the naming rule is the same either
way.

**Two suffixes carry a category, and the distinction between them is real** (milestone 63).
An **`_exerciser`** puts a capability of the system under load and sees whether it holds, with no
contract being probed from outside: `allocator_exerciser` interleaves allocation and free and then
demands a large allocation fit in pages already committed, and `std_exerciser` is an ordinary Rust
program on the native ABI whose three behaviours are chosen by the authority it was granted. A
**`_test_client`** exercises a service contract from outside, with a server on the other end:
`fs_test_client`, `socket_test_client`, `credentialer_test_client`. The `test` is not noise. The
unqualified names (`fs_client`, `socket_client`, `credentialer_client`) belong to the real clients
milestones 54 and 55 will need, and giving them to test programs squats them.

## Crates

### The one rule, and who applies it (2026-08-01)

**`snake_case`, everywhere, with no second tier.** Crates already did this (`fs_proto`, `user_rt`);
programs did not, and **0 of 57** carried an underscore, so multiword names were squished. The three
worst were `fsclient`, `sysinit` and `credcli`; they are `fs_test_client`, `system_initializer` and
`credentialer_test_client` now (milestone 63).

An earlier draft had two tiers: short names for programs a user types, underscores for programs only
the system spawns. It was rejected, and the reason generalises. **The category is not a stable
property of a program.** `wc` was internal plumbing and became a prompt-typed pipeline stage inside a
day, and a convention keyed to something that changes produces renames. It is also not how Unix got
its names: the terseness of `ls` is emergent pressure on words people type constantly, not a rule
anyone wrote down, and codifying an emergent property turns it into a classification chore every
contributor has to get right.

So one rule, no branch. A short name for a typed command is a *choice its author makes*, not a
convention to apply; nobody needs a rule to know `wc` beats `word_count`.

**calef names the crates, the programs, and the shared modules.** Same shape as `DECISIONS.md`
section numbers: global to the tree, so decided by the person who can see the whole tree. A lane
ships a **provisional** name, says so in its report, and expects it to change. Nobody renames on
their own initiative either, because a rename is a naming decision with extra steps. The reason is
that names are what make this OS legible to humans and to LLMs, and in a capability system a name is
often the only thing that says what a program can *do*.

**Standard terms are already right and must not be touched.** `elf`, `pci`, `dtb`, `gpt`, `ipc`,
`paging`, `glob`, `asid`, `socket_proto` are names a reader knows from outside this project, so they
cost nothing to learn. This tenet is a naming authority, not a renaming mandate, and renaming `elf`
would destroy the recognition the whole thing exists to buy.

**One constraint:** `nifefs` caps archive names at `NAME_LEN = 32` bytes, so a program's name is
bounded. Crates are not in the archive and are unbounded.

It was 24 until 2026-08-01, when it had started deciding names rather than bounding them: two settled
names were within four bytes of it and `os_primitives_benchmarker` exceeded it. Raising it costs
directory entries per block, and nothing else now that `Fs` no longer holds an entry array. See
[nifefs.md](nifefs.md) for the numbers. The rule that survives the raise: **do not let the
limit pick a name, and do not spend a format change on bytes nothing needs.** 32 clears the longest
settled name by seven bytes, which is a budget rather than the three bytes that were left before.

## Crates

`crates/` holds four audiences under one directory, and **naming does not distinguish them**, which
is a known gap rather than a decision.

- **Kernel logic**, host-tested and Kani-reachable: `capability`, `paging`, `frames`, `regions`,
  `slots`, `asid`, `intrusive`, `ipc`, `dma_validator`, `measured_boot`, `user_heap`.
- **Wire contracts**, spelled `*_proto` and checked for it by `script/lint`: `fs_proto`,
  `socket_proto`, `sink_proto`, `cred_proto`, `clock_proto`, `entropy_proto`, `gfx_proto`,
  `ntp_proto`, `supervision_proto`, `swap_proto`. Plus `abi`, which is the syscall boundary and
  predates the suffix.
- **Format and hardware parsers**: `elf`, `dtb`, `pci`, `gpt`, `nifefs`.
- **Userspace libraries**: `user_rt`, `grant_plan`, `virtio`, `video_terminal`, `line_editor`,
  `bitfont`, `glob`, `calendar`, `cred`, `compositor`, `coremark`, `c_seam`.

**`compositor` and `line_editor` are the two that look like contracts and are not**, and an earlier
version of this section listed them as such. Both are *logic* crates that happen to contain a
protocol module: `compositor` is the scene, the clipping and damage-rectangle arithmetic, and the
composition itself; `line_editor` is a sans-IO editor with a `line_editor::proto` inside it. Renaming
either to `*_proto` would promise a wire definition and deliver an algorithm, which is exactly the
kind of claim §39 is about. The `*_proto` check is right to leave them alone.

What the names actually do, over the 39 directories under `crates/`:

- **One word where one word will do**, which is 21 of the 39: `abi`, `capability`, `compositor`,
  `elf`, `frames`, `ipc`, `paging`, `regions`, `slots`, `virtio`.
- **Underscore when the two halves are separate concepts** and the name reads as a qualifier applied
  to a thing, which is the other 18: `fs_proto` is the proto *for* fs, `gfx_proto` the proto *for*
  gfx, `dma_validator` the validation *of* DMA, `user_rt` the runtime *for* userspace, `user_heap`
  the heap *for* userspace, `measured_boot` the measurement *of* boot.

**Milestone 63 deleted the third bullet, which used to read "run together when the result is one
word".** It was a real observation (`capsh`, `lineedit`, `uheap`, `crickerfs`, `bitfont`), and it was
the rule that produced every abbreviation a reader had to decode. **Two of the five survive it, not
one**, and this sentence said otherwise until milestone 115 checked the history: `crickerfs` (now `nifefs`, milestone 120) stayed
with a reason, because `procfs` is the shape of a filesystem name outside this project and nobody
writes `proc_fs`, and **`bitfont` stayed with none**, having never been renamed at all. Three moved,
to `grant_plan`, `line_editor` and `user_heap`. The boundary that
remains, between one word and two, is judgement, and the guard rail is that a **standard term keeps
its standard spelling** (see above).

The one place it became a real inconsistency is worth fixing and is checked: **the wire contract was
spelled four ways** (`fs_proto`, `gfx_proto`, `netproto`, `line_editor::proto`) for one concept.
`*_proto` wins for crates, because it is what the actual crates already were, and `socket_proto` has
since graduated from a module inside `net_stack` into a crate under that name.

**A crate that is a component's engine takes the component's name** (`line_editor` the sans-IO
editing crate, `line_editor` the binary that wires it to endpoints; `compositor` and `coremark` are
the same pair). They are the same thing at two layers, and giving the engine a second name is how
`termd`/`linedisc` happened in the first place. Where a note needs to tell them apart it says "the
`line_editor` crate" and "the `line_editor` binary".

**One pair deliberately does not share a name**, and the reason is worth keeping: the crate is
`video_terminal` and the program that wires it is `display_terminal`. The crate is named for the
**protocol** it implements (the VT standard, bytes in and a character grid out) and the program for
its **role** (the terminal on the display, next to `display`, the virtio-gpu driver it is a client
of). Both facts are true and neither name says the other.

## Scripts

Two directories, on purpose, and the split is by audience.

- **`script/`** is the front door: [Scripts to Rule Them All](https://github.com/github/scripts-to-rule-them-all)
  names, one short file each, **no extension**, lowercase, hyphenated if more than one word
  (`qemu-check`, `ci-qemu`, `toolchain-bump`, `vendor-verify`, `supply-chain`). These are what a
  person types. The canonical set (`setup`, `test`, `server`, `console`, ...) keeps its standard
  names even where a different word would be more descriptive: the entire value is that the command
  is the same in every repo that follows the pattern.
- **`scripts/`** is the helper drawer: `.sh` extension, called by other scripts and by `xtask`, not
  by people (`qemu-bounded.sh`, `qemu-runner-aarch64.sh`, `qemu-runner-riscv64.sh`).

Every `script/` entry needs a row in [scripts.md](scripts.md); `script/lint` fails without one, and
fails in the other direction too if `README.md` names a script that does not exist.

## Where a name's provenance lives (milestone 115)

**The refusals are the valuable half, and they used to live nowhere.** A ratified name is visible in
the tree, because it *is* the name. A refused one is visible in no file at all, and the person who
most needs it is the person about to propose it again.

That is not hypothetical. A lane proposed `system_builder` for the crate milestone 96 extracted, the
maintainer endorsed it, and calef overruled it to `system_initializer`. Only afterwards did anyone
find that **milestone 63 had already refused `system_builder`**, for a reason still true:
`user/src/builder.rs` calls itself "a minimal init: the system builder", so two programs would claim
one phrase. The refusal existed, in one table cell inside one milestone block, invisible at the
moment it was needed. A blind rename then swept the old name out of that very row, and the record of
the refusal was nearly destroyed by the rename it should have prevented.

**The record is derived, not maintained.** The first draft of the fix was one ratified-names table,
here in this file. calef rejected it on 2026-08-04 for scaling the way the original `DECISIONS.md`
and `design/roadmap.md` scaled, and size is the smaller half of that argument. The **conflict shape**
is the real one: every lane that adds a name would edit one file, which is exactly what produced
three section-number collisions in a day.

So:

1. **Provenance lives at the name.** A crate's `lib.rs` header, a program's module doc and a
   `script/` entry point's comment block each carry a `Name:` block saying when the name was
   ratified and what was refused. Adding a name touches exactly one file, so two lanes naming two
   things cannot collide. It also puts the refusal where the next proposer is already reading.
2. **`script/lint` checks presence, never content.** 126 names today, and a name with no block
   fails the build.
3. **`script/names` is the table**, computed on demand, so it cannot drift from the tree. Same
   family as `script/roadmap`, `script/decisions` and `script/catch-up`.
4. **The maintainer writes the block at ratification**, in the same commit that applies the name,
   while the alternatives are still in mind.

### The three states

calef works back through the existing names over time, so the record has to hold **which ones still
want him**, not only what is known. The state is the first word of the block:

| State | Means | What it costs to clear |
|---|---|---|
| **unrecorded** | nothing in the tree or its history says why this name was chosen | research, and then a ruling |
| **recorded** | the tree argues the name somewhere (a milestone block, a decision, a header) and calef never ruled | a ruling |
| **ratified** | calef ruled, with the date and what was refused | done |

The first cut of this mechanism had two states, and `unrecorded` was doing both of the first two
jobs. "Nobody in this tree can say why this is called that" and "here is the argument, nobody ever
signed it" are different amounts of calef's time, and a worklist that cannot tell them apart is a
list rather than a plan.

**The criterion, stated so that a reader can disagree with a case rather than with a mystery: a name
is `recorded` when something *outside its own block* argues for the name it has.** Three corollaries
did most of the work in the 2026-08-04 triage:

- **A `Name:` block is never its own evidence.** All 126 were written in one week by this milestone.
  Reading them as history would make the record prove itself and every name `recorded` by
  construction, which is why a `recorded` block must cite somewhere else and the citation is checked
  for being present.
- **"It got here first" is not a reason.** `notes/naming.md` exempts `abi` from the `*_proto` rule
  because it "predates the suffix", which explains why the crate is not called `syscall_proto` and
  says nothing about why it is called `abi`. Counting an exemption as an explanation would let every
  old name in the tree explain itself.
- **An assertion is not an argument.** The BUGS section below says of `virtio` that "the crate keeps
  its name, which is right", with no reason attached, so a reader learns that somebody agreed rather
  than why.

**The gate never keys on `ratified`, and that is deliberate rather than a weakness to tighten
later.** 54 of 126 names are unratified today. A lint that demanded the queue be drained would hold
every unrelated merge behind a review nobody can hurry, which is a wall this milestone was written
specifically not to build. `script/lint` insists only that a name say which of the three it is.

### The three forms

```
Name: ratified <YYYY-MM-DD> (<who>, <where>). Refused `x` (why), `y` (why).
Name: recorded (<where>). <what the tree already argues, and what it does not settle>
Name: unrecorded. <what the history does and does not say>
```

**`recorded` carries a citation for the same reason `ratified` carries a date.** The claim is that
the reasoning lives somewhere else, so a block that will not say where has not made the claim. Both
are checked as form and neither as truth: nothing follows the citation to see whether it says what
the block says it says.

A block runs from its `Name:` line to the next empty comment line, so it may wrap over as many
lines as the reasons need. Two conventions make the refusals machine-readable without a syntax
anybody has to remember: **a reason goes in parentheses**, and **the refusal clause ends at its
sentence**. Both exist because the alternative misfires. `capsh(1)` is cited as the Linux tool that
made `capsh` unavailable, so a parser that read every backtick would record the citation as a
refusal of its own; `grant_plan` explains after its list that it is deliberately not named for
`swish`, and neither `swish` nor the `dwarden` it compares itself to is a refused name.

**`unrecorded` is a first-class answer.** Most of this tree's vocabulary arrived before anyone was
writing naming decisions down. Inventing a ratification to fill a row would put a false claim in the
one record whose entire job is saying who claimed what, so where the history does not say, the entry
says it does not say and cites the commit that introduced the name.

### The numbers, and the one that was not expected

Of 126 names: **72 ratified, 10 recorded, 44 unrecorded.** So 43% of this tree's most reader-facing
vocabulary arrived without a recorded decision, and only a fifth of that backlog is the cheap kind
where the argument exists and wants a signature.

Every name a rename ever touched is ratified, because a rename is an argument and somebody wrote it
down. What is left over is what nobody objected to at the time.

**The distribution is the finding, and it runs opposite to exposure:**

| Surface | ratified | recorded | unrecorded |
|---|---|---|---|
| programs (54) | 36 | **0** | 18 |
| crates (43) | 23 | 8 | 12 |
| `script/` (29) | 13 | 2 | 14 |

**Programs are 0 recorded of 18.** The surface a person types at the prompt, which the worklist puts
first precisely because a wrong name there is read by everyone who uses the system, is the one with
no argued reasoning anywhere in the tree: not in a header, not in a milestone block, not in an
introducing commit. `budgeter` and `heeder` are cited *as* an established agent-noun family when
milestone 63 argues for `benchmarker`, and neither was ever argued for itself. `sink` is used
throughout DECISIONS §51 and defended nowhere in it. The one program whose record says anything
useful is `disk_partitioner`, whose introducing commit calls the name provisional in as many words.

The 10 `recorded` are almost all one rule doing the work: seven `*_proto` crates, where milestone
46's spelling decision plus the service the stem names produces the whole string. The three
outliers are `intrusive` (its own header grounds the term in Linux's `list_head` and seL4's TCB
queues), `script/fmt` (the name was itself the fix, and the header cites §39 for it), and
`script/supply-chain` (the naming tenet cites this name and says not to respell it).

**Two `*_proto` crates are not recorded, and the split is the criterion working.** `gfx_proto` and
`cred_proto` have abbreviated stems, which is the first of the three failure modes the tenet lists
for crate names, and the rule that yields `<service>_proto` does not pick which word goes in front
of the underscore. `cred` is the sharper case: milestone 63 expanded `credcli` and argued
`credentialer` in full, then left two crates spelled `cred` without saying why. `user_rt` fails the
same way twice over, since the only thing establishing `user_` as a prefix is `user_rt` itself.

### EXAMPLES

Has this name been refused before? This is the query the incident above needed and nothing could
answer:

```
$ script/names system_builder
REFUSED for crate system_initializer  (crates/system_initializer/src/lib.rs)
  ratified 2026-08-04 (calef, milestone 96), and it is the ratification that raised milestone 115.
  Refused `system_builder` (milestone 63 had already refused it, for a reason still true:
  `builder.rs` calls itself "a minimal init: the system builder", so two programs would claim one
  phrase) and `system_bootloader` ...
```

Everything that has been turned down, and where the reason lives:

```
$ script/names --refused
REFUSED (85), and what holds each refusal

  allocdemo                    program allocator_exerciser
  allot                        crate grant_plan
  ...
  job_killer                   program job_undertaker
  sanitize                     script undefined-behavior-check
  sheesh                       crate swish
```

**What is left, in the order worth working through.** This is the deliverable, and the ordering is
exposure rather than alphabet or count, because exposure is what makes a wrong name expensive: a
program is typed at the prompt, a crate is what a newcomer greps before opening anything, a
`script/` entry point is typed by whoever works on the tree rather than in it. Within a tier, a name
nobody can justify comes before one whose reasoning merely lacks a signature.

```
$ script/names --unratified
UNRATIFIED (54 of 126), in the order worth working through
...
  programs, unrecorded
    budgeter                     user/src/budgeter.rs
    builder                      user/src/builder.rs
    ...
  crates, unrecorded
    abi                          crates/abi/src/lib.rs
    ...
  crates, recorded
    clock_proto                  crates/clock_proto/src/lib.rs
    ...
  scripts, recorded
    fmt                          script/fmt
    supply-chain                 script/supply-chain

44 unrecorded (research, then a ruling), 10 recorded (a ruling only).
```

**The tier is the kind, and not "programs a person actually types".** That second split is the
two-tier rule calef rejected on 2026-08-01, keyed on a property that is not stable: `wc` went from
internal plumbing to a prompt-typed pipeline stage inside a day. Every program in `user/src/` is in
the initrd and can be typed, so the kind is the honest tier and needs no classification anybody
could get wrong. This is a sort order rather than a naming convention, so the cost of being wrong
about one entry is that it is read in the wrong minute.

Then one name at a time, with what the history does and does not say about it:

```
$ script/names bitfont
crate bitfont  (crates/bitfont/src/lib.rs)
  unrecorded. One of the five run-together names milestone 63 reviewed on 2026-08-01 ...
```

The narrower slice, for the names where the research is still owed:

```
$ script/names --unrecorded
UNRECORDED (44 of 126): nothing outside the block says why
```

The whole table, and the gate:

```
$ script/names | tail -3
total: 126 names, 72 ratified, 10 recorded, 44 unrecorded, 85 refusals
54 still want calef: script/names --unratified
refused but live: video_terminal

$ script/names --check
names: NOTE 'video_terminal' is recorded as refused and is also a live name
names: 126 names carry provenance (43 crates, 54 programs, 29 scripts)
names: 72 ratified, 10 recorded, 44 unrecorded, 85 refusals recorded beside them
names: 54 still want calef (script/names --unratified), which is a worklist and not a failure
```

That `video_terminal` line is the mechanism working rather than a defect. The name was **refused for
the program** (`display_terminal` is named for its role) and is **live as the crate** (named for the
protocol it implements). Both facts are true, the pair is deliberate, and a reader who meets only
one of them would get it wrong. The check reports the contradiction and never fails on it, because a
refused word can legitimately survive as ordinary English and a gate that fires on prose is a gate
people learn to skip.

### BUGS

- **It checks that a name carries a reason, never that the reason is still true.** A block whose
  argument was overtaken looks exactly like one whose argument holds. That is the same limit
  `script/decisions --check` records for `§N` citations, and it is not closeable by a script,
  because a reason is prose and prose is checked by reading. Milestone 97 is the neighbouring case.
- **It cannot tell an honest `unrecorded` from a lazy one.** The 44 above were each researched
  against the git history, and nothing stops the forty-fifth from being a shrug. The only defence is
  that an `unrecorded` entry cites the commit that introduced the name, so the next reader starts
  where this one stopped rather than from nothing.
- **`recorded` is the state easiest to claim and hardest to check**, which is the price of splitting
  it out. The citation is checked for being present and never followed, so `recorded (milestone 46)`
  on a name milestone 46 never mentions passes the gate exactly as well as a true one. Read the
  citation before trusting the state, and note that a wrong one costs less than the alternatives: it
  demotes the entry from "research owed" to "signature owed" in a worklist, rather than putting a
  false claim in the tree.
- **Seven of the ten `recorded` lean on a rule that was derived from the names it now explains.**
  `*_proto` won in milestone 46 partly *because* "it is what the actual crates already were", so
  saying `fs_proto` is recorded by that rule is not fully independent of `fs_proto`. It is not
  circular either, since the decision adjudicated four live spellings and `script/lint` has enforced
  the winner since, but a reader deciding how much weight the state carries should know it leans on
  one decision and that the decision partly ratified the status quo.
- **Three surfaces, and the tree has more than three kinds of name.** Crates, programs and `script/`
  entry points carry blocks. Directories, types, `scripts/` helpers, `kernel`, `xtask`, `fs_server`
  and `tools/redoxfs_host` do not, and at least one ratified name has no home as a result:
  **`design/audit-reports/`** (calef, 2026-08-04), where `audit-trail` was refused because
  `design/decisions/35-scanner-findings.md` already uses that phrase for a chronological record of
  dismissals and it is also what an operating system means by it (`auditd`), and bare `audits` was
  passed over because every file in the directory is a report. Recorded here rather than stretched
  into a schema that does not fit it.

  **That blind spot has a live casualty, found while triaging.** `disk_partitioner`'s introducing
  commit (2026-08-03) named two provisional things: itself, and `fs_maker`. The first is on a
  covered surface, so it is in the worklist with the word "provisional" quoted in its block. The
  second is at `fs_server/src/bin/mkfs.rs`, where nothing looks, so it was resolved to `mkfs` by
  whoever was mid-task and no record anywhere says a decision was owed. That is the exact failure
  this milestone exists to prevent, still happening one directory over.
- **A type's name is a naming decision the mechanism does not see.** `BootEndowment` was ratified on
  2026-08-04 (replacing `Grants`) and is mentioned inside `system_initializer`'s block only because
  its crate happens to export it. `supervision_proto::Endow` is an open naming question (§69) and
  appears nowhere in this record.
- **The `Name:` marker is a string in a comment**, so a header that never had one is caught by the
  gate while a header that loses one to an edit is caught only if the edit removes the whole line.
- **A correctly formatted `ratified` is never checked against calef, and on 2026-08-14 two lanes
  contradicted each other about the same name on the same day.** `crates/cpu_set` was introduced
  twice in one stack of pull requests. #176 carried `Name: ratified (calef, 2026-08-14, the same day
  the first-silicon online-set sweep introduced it)`. #178, one branch later in the same stack,
  carried `Name: unrecorded, provisional (introduced 2026-08-14 by the first-silicon online-set
  sweep; calef has not seen it)`. The second is the true one, and the two sat in the queue together
  asserting opposite things about whether a ruling had happened. (Both headers said `Chris` when
  they were written; they are quoted here in the referent this tree adopted on 2026-08-15, which is
  also how the surviving one now reads in `crates/cpu_set`.)

  **The gate caught the false one, and caught it for the wrong reason.** `script/names --check`
  rejected #176 because the date sits inside the parenthetical where `ratified (\d{4}-\d{2}-\d{2})`
  cannot reach it. Written as `Name: ratified 2026-08-14 (calef, …)`, the identical false claim would
  have passed every check in CI and landed on `main`. What stopped it was punctuation.

  **This follows from the design and is not a defect in it.** The gate checks that a block names one
  of the three states and never that the state is `ratified`, because a gate keyed on ratification
  holds every unrelated merge behind a queue only one person can drain, which is the wall milestone
  115 was written not to build. The cost of that choice is what this entry records: **claiming
  calef's ruling is as cheap as claiming anything else, and nothing downstream disagrees.** The
  `recorded` entry above says the citation is never followed; this is the same hole one state up,
  where the claim is not a citation anybody could follow but an assertion about a person.

  A lane that does not know this will write `ratified` meaning "this name seems settled". Write
  `unrecorded, provisional` instead and say so in the report. It costs nothing, `script/names
  --unratified` is a worklist rather than a wall, and an unratified name has never failed a build.

  **What would help without building the wall**, if this recurs: surface newly added `ratified`
  blocks in a diff for the integrator rather than blocking on them. `ratified` is the one state a
  lane structurally cannot be entitled to assert, so a new one appearing in a pull request is worth a
  human glance even though it must not be worth a red check. That is rung two of AGENTS.md's ladder
  applied to the one state that currently sits on rung zero. Not built, and not obviously worth
  building for a hazard observed once.

## Directories (milestone 63)

The tenet covered crates, programs, modules, shell entry points and markdown, and said nothing about
**directories**, so the tree carried three spellings. Two rules, and neither is a new tier:

- **A directory that holds a Rust package is named exactly as the package**, so `snake_case`. The
  directory and the package are one thing with one name.
- **Any other directory is lowercase, and hyphenated if it needs two words**, the same convention as
  markdown filenames and `script/` entry points, because a directory is a path element and paths are
  hyphenated in the world outside this repository.

Three directories violated the first rule and all three moved in milestone 63: `fs-server/` (package
`fs-server`) is `fs_server/`, `tools/redoxfs-host/` is `tools/redoxfs_host/`, and `user-std/`, whose
package was called `hellostd` and matched neither, is `std_exerciser/` twice over.

A hyphenated package name is not wrong in the wider ecosystem. `wasm-bindgen` and
`tracing-subscriber` are ordinary and Cargo normalises a hyphen to an underscore for `use`, so
nothing was broken. The case was internal consistency, 36 crates against 3, and it should be read
that way rather than as a correctness fix.

**Two things that look alike and are not:** `target/` is gitignored build output and `targets/` is
the tracked custom target JSON (`aarch64-unknown-nife.json`). Nothing enforces the distinction.

## Where a document goes

Three places, and the distinction is what the document is *for*, not what it is about.

| | holds | shape |
|---|---|---|
| `design/` | the option space, before a decision | "here are four answers and three are bad" |
| `DECISIONS.md` | the decision, and the argument that settled it | numbered `§N`, append-only |
| `notes/` | what exists, and what building it taught us | a running glossary, indexed in `notes/README.md` |

`design/roadmap/` is the exception that proves the split: it lives in `design/` because a milestone
block is an argument for doing something, not a record of having done it, even after the milestone
ships and the block gains a "Built" line.

A note is not optional. Every concept and every finding gets one, indexed in
[notes/README.md](README.md), because for a demonstration OS the documentation is part of the
deliverable rather than a courtesy to the author.

## `§N` is not milestone N, and they collide

**DECISIONS section numbers and roadmap milestone numbers are separate schemes over the same small
integers.** There are 41 sections and 39 milestone blocks, so almost every number means two things:

| N | DECISIONS `§N` | roadmap milestone N |
|---|---|---|
| 24 | the two-tier Ctrl-C | a Virtualization.framework board |
| 28 | SMP placement | the line discipline |
| 31 | the foreign-language C seam | the capability shell |
| 39 | this naming rule | components, services, and the directory layout |

This has already produced a wrong citation in the tree, not a hypothetical one: milestone 50's block
cited "§31's FileSpec", which points at the C seam and has nothing to do with file grants. The thing
it meant was milestone 31 phase 2, granting against the §27 filesystem contract. Fixed in c0643bc.

So:

- **Write `§N` only for DECISIONS.** Never for a milestone.
- **Write "milestone N" in full.** Never bare `N`, and never `§N`.
- Prefer number **and** name on first mention in a block ("milestone 31, the capability shell"),
  which is what makes the wrong one visible.

`script/decisions --check` verifies that every cited `§N` resolves to *some* section. It cannot
verify that it resolves to the *right* one: a well-formed wrong citation is indistinguishable from a
correct one from the outside. Worth knowing before trusting that gate for more than it claims.

## Branches

Eight prefixes were in use when this was written, including both `feature/` and `feat/` for the same
idea. One spelling, and `feature/` is the older one:

`milestone/` (a roadmap milestone), `fix/` (a bug with a name), `bench/` (measurement work),
`audit/` (reading rather than writing), `integration/` (joining lanes), `finalize/` (landing them).
Plus `main`, and the tooling's own `worktree-agent-*`, which no person types.

Checked, for the current branch only, and skipped on a detached HEAD.

## What is checked, and what cannot be

`script/lint`'s `naming conventions` block enforces seven things, and the first five are cheap greps
because lint runs constantly. (An earlier version of this sentence said four and then listed five,
which is the ordinary way a hand-kept count drifts; take it from the script.)

1. **No name ending in `-d`**, over `user/src/*.rs`, `user/Cargo.toml`'s `[[bin]]` names, and
   `crates/*`. Four characters or more, so `kbd` is an abbreviation rather than a daemon. Words that
   genuinely end in `d` go in `naming_allow` **with a reason**, the same shape as a per-item
   `#[allow]`; `asid` (Address Space IDentifier) is the one there today.
2. **The word "daemon" appears nowhere**, outside `DECISIONS.md` and `design/`, which are where the
   argument about the word lives and therefore have to be able to name it.
3. **Contract crates spelled `*_proto`.**
4. **The current branch carries a recognised prefix.**
5. **No `#[path]` module is shared by two or more binaries** (CLAUDE.md rule 7). This is the newest
   and the one with teeth: it counts consumers per include target and fires at two. A module with a
   single consumer is an ordinary submodule and is fine, because the rule is about *agreement between
   binaries*, not file layout. The allow-list is **empty**, which is the intended steady state.
   `virtio` was its one entry for about an hour: it could not be a crate while it reached back into
   whichever binary included it for `check`, and the resolution was to **delete `check`** rather than
   pass it in. Rust already has the per-binary "how this program dies" hook, `#[panic_handler]`, and
   both binaries already had one executing the same instruction by two different routes. An entry
   here needs a reason of that calibre.
6. **`notes/` and `design/` filenames are lowercase and hyphenated**, because they are URL slugs the
   moment any of this is published. `README.md` is the only exception, and it is GitHub behaviour
   rather than style.
7. **Every crate, program and `script/` entry point carries a `Name:` block** (milestone 115, and
   the section above). Presence only: it cannot check that the reason is still true. It checks that
   the block names one of the three states, that `ratified` carries a date and that `recorded`
   carries a citation, and it **never checks that the state is `ratified`**, so a name waiting on
   calef does not fail anybody's build. `script/names --unratified` is how that queue gets worked.

Everything else here is prose because it needs judgement and no checker can supply it. In particular
**a checker cannot catch the jargon half of §39**: `linedisc` would have passed all four rules above.
It ends in `c`, contains no daemon, is not a proto crate, and had a perfectly good branch. What
caught it was a person reading the name and not knowing what it meant, and that remains the test.

Two limits worth stating rather than discovering: the checks read the filesystem for names and use
`git grep` for the word, so an **untracked** file with "daemon" in it is invisible until it is added
(the same blind spot the conflict-marker check has), and check 1 sees the *names* of things rather
than the things, so a component whose name is fine and whose behaviour is a daemon is not its
problem.

## BUGS

What milestone 63 did **not** rename, each on purpose, so the next reader does not "fix" one of them
by mistake.

- **The boot mode is still called `shell`, and the program is `swish`.** `cargo xtask shell` and the
  kernel's `--features shell` name a *configuration* (boot straight to a prompt, milestone tour
  compiled out), not the binary. Renaming them would have been a naming decision nobody made. The
  cost is that a reader who greps `shell` in `xtask` and in `kernel/Cargo.toml` meets a word that no
  longer names a program.
- **`caps` is still a shell builtin**, and it no longer shares a name with anything. It was the
  larger half of the 285 occurrences of `caps` in the tree before the rename, and it means "print
  this process's endowment". The crate that used to share the spelling is `capability`.
- **`crates/virtio`'s first line still says "A virtio-blk driver".** It also drives net, serves
  blocks through `run_blk_server`, and carries two deliberate attack roles. The crate keeps its name,
  which is right, but the sentence under it is wrong. Out of scope for 63 and not yet filed anywhere
  else.
- **A rename can reach into the vendored tree, and `script/supply-chain` is what says so.** Our one
  divergence comment in `vendor/redoxfs/Cargo.toml` names `redoxfs_host`, so the rename had to touch
  the vendored file **and** `vendor/redoxfs.divergence.patch` together or
  `script/vendor-verify` fails with "differs from upstream+patches". Milestone 63 changed the patch
  first and the vendored file not at all, and the gate caught it. Edit both, in the same commit.
- **The measured-boot manifest is still `target/init-measure-<arch>.txt`.** It is a build artifact
  name, not a crate reference; `kernel/build.rs` reads it and turns it into `TRUST_ROOT`.
- **Note filenames did not move**, and that is the rule rather than an oversight:
  [fs-server.md](fs-server.md), [shell.md](shell.md), [shell-navigation.md](shell-navigation.md) and
  [line-discipline.md](line-discipline.md) are markdown, so they stay lowercase-hyphenated even
  though the things they describe are now `fs_server`, `swish` and `line_editor`.

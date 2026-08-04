# `time`: measuring a command that holds no clock

The shell's second prefix word (milestone 86), after [`caps`](grant-expression.md). `time wc
report.txt` runs the command exactly as typed and says how long it took. The program is
`user/src/swish.rs`, the arithmetic and every sentence are `crates/swish`, and the guest tests are
`kernel::user::time_tests`.

```text
$ time wc report.txt
       3      12      67
  time: real 4.213 ms
```

## The one thing worth understanding: whose clock it is

**The shell's.** `time` reads a clock capability the shell was granted at boot, times the command
around its own dispatch, and hands the command **nothing**. So `wc`, whose entire endowment is one
endpoint and which has never been able to ask what time it is, gets timed anyway.

That is the Unix behaviour, and it is also the capability-model answer, which is why the two agree
here rather than needing a trade. A program does not consent to being timed and cannot tell that it
is; a duration is observable to anybody who can watch a thing start and stop, and the shell is that
observer by construction, because it is what started the thing and what noticed it finish.

The alternative was delegating a clock **to the child**, and it is rejected rather than deferred. It
would make `time` a grant: `time wc report.txt` would run a differently endowed `wc` from the one
`wc report.txt` runs, which breaks the property the whole command rests on, and it would put a
readable clock in the hands of every program anybody thought to time. The `DECISIONS.md` section for
this is the integrator's to mint at merge (CLAUDE.md: a lane does not claim a number global to the
tree); until it exists the argument is here and in `design/roadmap/86-time-command.md`.

The consequence in the wiring is one bit. The shell holds the clock page with **`READ` and not
`GRANT`**, so it can read the time and cannot hand a clock to anything it spawns. Which processes
can read the time is still decided by the manifests init reads (`Manifest::clock`, today `date` and
nothing else); the shell's own reading authority does not widen that set by one. `caps` prints the
row, with the rights, because a reader who saw "clock" in that table and assumed it could be passed
on would be wrong about the one thing the table exists to answer:

```text
$ caps
  this shell holds, and nothing else:
    cap 0  endpoint  terminal   read lines, write text
    ...
    cap 5  frame     clock      READ only, NOT delegable: 'time' measures with
                                it and no command can be handed it
```

## What you time is what you run

The tail of `time` is a **whole command line**, operators included, and the shell re-dispatches it
through `dispatch`, the same function the prompt calls. So `time date | wc` times the pipeline,
`time echo hello` times a builtin that spawns no process at all, and a line that would have been
refused is refused identically.

This is `caps`'s shape one milestone later, including the ordering bug behind it: `swish::route`
answers both prefix words **before** it splits the line on its operators, or the tail would arrive as
the single word `date` with everything after the pipe silently gone. One function, two arms, one
host test each.

`caps time <command>` previews the command itself, for the same reason: `time` moves no authority, so
the preview must be identical to the untimed one. The host test asserts equality of the whole table,
not a substring, because "identical" is the claim.

## What the number means, and three things it is not

**It is wall clock**, between the shell deciding to run the line and the line being over, at the
resolution of the ambient counter (16 ns on the aarch64 board at 62.5 MHz, 100 ns on the RISC-V one
at 10 MHz). `real` is Unix's word for it and it is the honest one.

**It is not CPU time.** There is no `user` row and no `sys` row, because nothing in this kernel is
asked what a thread spent: that is the scheduler's knowledge and it is unqueried today. If it
arrives it is another row printed by this command, not a rival command. Saying so at the prompt (the
`help` line says "WALL clock, not CPU") is cheaper than a person inferring it from a number.

**It is not a benchmark.** `os_primitives_benchmarker` and `script/bench` measure spawn latency
properly, with repetition and a stated methodology (notes/benchmarks.md). `time` measures one run of
one line and includes the shell's own planning, spawning and draining, because there is no other
vantage point from which the question has an answer.

**It is not a promise that the clock stood still.** A wall clock can be stepped by an authority the
shell does not hold. The shell reads `clock_proto`'s **generation** at both ends, and when it changed
it says so rather than printing a number it cannot stand behind:

```text
  time: real 4.213 ms
  time: the wall clock was stepped while that ran, so this is what the clock
        says elapsed rather than what did
```

The subtraction is saturating for the same reason: a backwards step would otherwise wrap a `u64`
into something enormous.

## The refusals, two of which are `date`'s

A shell that cannot read a clock **does not run the command**. `time` is a request to measure, and
running the line unmeasured while saying nothing would be DECISIONS §42's silent degradation with a
stopwatch on it; running it while complaining would leave a person unable to tell which half
happened. So the refusal is at the prompt, before anything is spawned, like every other refusal in
this shell.

```text
time: name a command to time: time <command>
time: the time is unknown: this shell holds no clock capability
time: the time is unknown: the machine has no clock it believes
```

The last two are `date`'s sentences with the name changed, deliberately (notes/date.md). They are the
same two facts about the same page and they call for the same two fixes: nobody granted this process
a clock, or the machine never learned the time. A person who has met one has met the other.

The probe for the first cannot read the page, which is `date`'s finding applied unchanged: a process
granted no clock has nothing mapped where a clock would be, so a read there would fault instead of
answering. It invokes the capability with a method number no object type defines, and a refusal from
an object is proof one is there.

## The wiring, and the slot that moves

The init grants the shell the clock **last**, after the filesystem pair, so a boot with no disk
attached takes exactly the path it took before this existed. That means the slot is **4 on a boot
with no filesystem and 5 on a boot with one**, and the shell is *told* the number in `x2` at
`_start` rather than assuming it.

This was written twice when milestone 86 landed, once in each of the two inits, because milestone 96
had not merged yet. It is written once now, in `crates/system_initializer`, and the frame it hands
over is `BootEndowment::clock_page`: the same capability the kernel granted init, handed on with
`READ` and no `GRANT`. There is deliberately no second endowment field for the shell's copy, because
the shell's clock is not a separate kernel grant, and a field would ask each board to state the same
slot number twice with nothing checking that the two agree.

Being told is the same shape as `arg1` carrying the directory's rights (notes/shell-navigation.md),
and here the argument is sharper: nothing in this system reports what a process holds, and a shell
that probed the wrong slot would find some *other* object and map a page that is not a clock. Zero
means "no clock", which is unambiguous because slot 0 is the terminal in every wiring.

The page is mapped at `0x00d0_0000`, which is **not** `date`'s `0x00c0_0000`: that address is where a
*child* maps its clock, and the shell already has the terminal's output frame there. Two address
spaces may agree on an address; one may not.

## What the tests prove

**Host** (`crates/swish`, `crates/grant_plan`), in milliseconds and with no machine:

- the tail parses back to the command that would have run, and plans the identical endowment;
- the operators survive the prefix (`time date | wc` is a timed pipeline);
- the unit boundaries of the duration rendering, at each side of each boundary, plus the zero
  padding, because `4.13 ms` and `4.013 ms` are different numbers and only one of them happened;
- `caps time <command>` renders byte for byte what `caps <command>` renders;
- all three refusal sentences, and that each is attributed to `time` rather than reading as the
  command's own complaint.

**Guest** (`kernel::user::time_tests`), one script run three times against three cspaces:

- with a **published** clock page: `worker 3` and `time worker 3` answer the same thing, and the
  duration parses back to a positive number under ten seconds. `worker`'s manifest declares no
  clock, so it holds none, and it is timed anyway. `time echo hello` spawns no process at all and
  still reports a duration.
- with a **blank** page (granted, never published to): "the machine has no clock it believes", and
  the untimed control on the line above still ran, so what stopped the timed line was the clock.
- with **no capability**: "this shell holds no clock capability", answered without touching the
  address a clock would have been at.

**At a real prompt** (`script/shell-check`, both ISAs), because only that gate runs the real inits:
`time wc gate.txt` answers the same three numbers `wc gate.txt` answered, `time date` prints a
`time: real` line, and `caps` shows the clock row with its rights. A boot where init never handed the
shell a clock passes every guest test and fails here.

## BUGS

Named here rather than in a tracker, next to the feature.

- **A duration does not actually need a clock capability, and this command requires one anyway.**
  Wall clock is `offset + counter` (notes/clock.md), the offset is constant across a command, and the
  counter is **ambient**: `user_rt::monotonic_nanos` is two register reads and no syscall, available
  to every process in the system. So `end - start` reduces to a difference of counter readings, and
  a `time` built on the counter alone would need no capability, could not be refused, and would be
  *immune* to the stepped-clock hazard above rather than merely honest about it.

  What requiring the clock buys is that the shell reports **wall-clock** elapsed time, and that it
  can see a step at all. What it costs is the refusal path: on a machine with no believable clock,
  `time` declines to measure something it could have measured. The design was settled in milestone
  86's roadmap block before the code was written and is implemented as recorded; this paragraph is
  the objection, kept next to the feature so the next reader does not have to rediscover it. If it is
  revisited, the change is small and local: read `monotonic_nanos` at both ends, delete `Untimed`'s
  two clock arms, and the wiring below stops being needed.
- **`time` cannot be a stage of a pipeline.** `date | time wc` is refused with "is a builtin that
  produces no stream, so it cannot be a stage of a pipeline". Unix allows it. Nothing here needs it,
  and a prefix word inside a stage would have to decide whose shell was doing the timing.
- **A nested prefix collapses rather than nesting.** `time time date` prints one duration, not two.
  The prefixes are stripped in a loop instead of recursing through `dispatch`, because this shell's
  stack has run out four times already (notes/pipes.md) and timing something twice measures nothing
  twice.
- **The measurement includes the shell.** Planning, spawning, delegating, draining the output and
  rendering it are all inside the interval, because they are all part of running the line. For a
  spawn that is the honest number; for a comparison against another system it is not, and
  notes/benchmarks.md is where that question is answered properly.
- **There is no `user` or `sys`, and their absence is not a `TODO`.** Nothing in the kernel records
  per-thread CPU time, so there is no number to print and no method to call. It would be a scheduler
  change, not a shell one.

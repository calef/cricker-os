# 106. Take the `terminal_sink_caretaker` narrowing: an unredirected tail stage's output goes to the screen, not the shell

**Status: DECIDED.** calef, 2026-08-22, on a milestone 40 lane's write-up
(`notes/tail-output-narrowing.md`, pull request #392): option 1, take it. *"Proceed as you
recommend as long as we have a milestone to address the fix"* (the caretaker-hop display race,
tracked at milestone 151).

**Minted 106 rather than the lane's own citation of 105**: §105 (`std::thread::spawn` declined) was
minted the same day on a concurrent branch (pull request #394) and had not merged when this was
written.

## The question

Milestone 40's remaining fork, open since notes/pipes.md first named it (2026-08-04, milestone 50)
and restated as this milestone's own blocker on 2026-08-18: `doc page.md` cannot render at the
prompt, because `doc` both writes and reads (`InputSpec::Required { writes_while_reading: true }`),
and this kernel gives a process exactly one blocking wait point. The shell cannot be both the feeder
of `doc`'s input and the reader of its output at once; `grant_plan::check_chain` refuses the line
before anything spawns, correctly and non-deadlockingly, but no line has ever rendered a page.

DECISIONS §101 (2026-08-20) already endorsed the *direction* ("the right short-term move," "the
right permanent shape for a tail stage whose output goes to the screen") and sequenced it as step 1,
ahead of the notification object, but explicitly declined to take milestone 40's own fork: *"That is
milestone 40's fork, and this decision does not take it."* This decision takes it.

## The decision

**Take it.** An unredirected tail stage's primary output (no `>`, no `|` on the line, the same
condition `Wiring::sink == false` already tests) is delegated to `terminal_sink_caretaker` by
default instead of to the shell's own synchronous read loop, the same adapter a program's *declared
second* stream already reaches by default under §67.

**This is cheaper than the fork's own write-up estimated, and that mattered to the decision.**
`SINK_BIT`'s existing contract (`crates/grant_plan/src/spawnproto.rs`) already makes the child's
output slot opaque to the program: *"the shell delegates an endpoint and init puts it where the
result endpoint would have gone, so the child writes to a pipe or a file sink without knowing
which."* Nothing about *what a program declares* changes. No program's manifest is touched, because
the primary slot isn't something a program opts into: every program has exactly one, unconditionally.
The work is confined to the shell (default the delegated capability to a caretaker grant when
`sink == false`) and init (build that capability by default in that case, the same way it already
builds one for a `DIR_BIT` grant). That is shell-and-init coordination, not the wire-format-two-
programs-must-agree-to category the fork's own write-up filed this under; the note's six-questions
analysis (question 6, reversibility) did not check `SINK_BIT`'s own design closely enough to catch
this.

**What ships alongside it, reusing proven mechanism rather than inventing new:**

- **The completion signal.** The shell loses its "read the child's `OP_EOF`" signal for a
  caretaker-routed child, and gains DECISIONS §26's already-built, already-proven fault/exit
  endpoint instead (built at milestone 22, today wired only for supervised/interruptible foreground
  jobs). Wiring it for ordinary sink-declaring children is shell-side plumbing comparable in size to
  the existing `spawn_interruptible` job-watching path, not a new kernel primitive.
- **The narrowing rule.** Applies only when the line has no `>` and no `|`, decidable from the plan
  before anything spawns, so redirected or piped output is completely unaffected.

## The known cost, and why it is carried rather than blocking

**The caretaker-hop display race**, named for the first time in this fork's write-up: kernel
exit-delivery (§26) tells the shell a child is dead-until-reaped, which is a stronger signal than
"the child painted its own last line," but `terminal_sink_caretaker` is a separate long-lived
process, and its own trailing `CALL` to `line_editor` can still be in flight when the shell prints
its next prompt. Two independent `CALL`s to one server, no ordering primitive between them today: a
page's last line and the next `$ ` can interleave under contention. A display glitch, not a
confinement or correctness failure: no capability changes hands, no byte reaches the wrong reader.

**This is carried as a documented `BUGS` entry, not a blocker, on the condition that its fix has a
tracked home**: milestone 151 (`design/roadmap/151-notification-objects.md`), which builds the
notification object §101 already specified. Once it lands, the shell binds a notification to its
own TCB and `WAIT`s on "the caretaker's queue for this client has drained" instead of racing it.

## What this does not decide

**The notification object itself** is milestone 151's build, not this decision's; §101 already
specified its shape in full. **Phase 3, the graphical viewer**, still waits on the display ladder
(milestone 29's font rendering, milestone 33's compositor) regardless of this fork.

## What it unblocks

Milestone 40 phase 3's caretaker-narrowing increment is now buildable to a concrete spec: the
narrowing rule, the §26 fault-endpoint reuse, and the caretaker-hop race recorded as a known,
accepted interim carried until milestone 151 lands.

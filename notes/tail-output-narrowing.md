# The tail-stage output fork: milestone 40's last piece, decided the six-questions way

*Written 2026-08-22, by the lane milestone 40's roadmap block handed this to
(`design/roadmap/40-documentation-service.md`). The question itself is not new: notes/pipes.md has
carried it open since milestone 50 (2026-08-04), notes/manual.md restated it as milestone 40's
remaining fork on 2026-08-18, and DECISIONS §101 (notification objects), decided 2026-08-20,
ratified the *direction* without taking milestone 40's specific fork. This note is the six-questions writeup CLAUDE.md's "A fork
reaches calef with its questions already answered" asks for, so the remaining decision can be made
in a sentence. See the PR body for the one-paragraph ask.*

## The premise, checked against the code rather than trusted

**Claim (the roadmap block's):** "a tail stage's output has nowhere to go but the shell, which is
why no line renders a page at the prompt."

**True, and it is a structural fact rather than a missing feature.** `doc page.md` (a bare
positional, no `<`, no `|`, no `>`) plans as a one-stage pipeline whose input is a file the shell
itself feeds (`grant_plan::plan_against_with` turns a trailing positional into `Source::File`,
exactly as it does for `wc report.txt`; see notes/pipes.md's "the file behind a `<` is this shell").
`doc` also writes while it reads (`InputSpec::Required { writes_while_reading: true }`, the only
declarer today), so the shell would have to be both the feeder of its input and the reader of its
output, and this kernel gives a process exactly one blocking wait point (`SEND`/`RECV`, no select,
no receive-on-a-set, no timed wait). `grant_plan::check_chain` refuses the line before anything is
spawned:

```text
$ doc motd
  doc: writes while it reads, and this shell can only wait on one thing at a time: give it a
  reader that is not this shell, as in '| wc'
```

This is verified at a real prompt, both ISAs, in notes/pipes.md's "One wait point" section. It is
not a bug in `doc`: `wc` is the only barrier in the tree, and only a chain that ends in a barrier
runs. So the premise holds, and no shell-side fix closes it: notes/pipes.md states outright that no
interleaving schedule can, because the shell cannot know which of `SEND`/`RECV` the far end wants
next, and guessing wrong deadlocks either way.

## 1. What else was considered, and why did each lose?

Three prior answers exist in the tree already, all recorded before this note, none chosen:

**(a) A pull-based source.** Collapse input and output onto one endpoint the child `CALL`s for
bytes on and `SEND`s output over, so the shell has one wait point per child. notes/pipes.md names
this "the exact answer to the constraint" and DECISIONS §101 confirms it is still available.
**Refused, twice, on the same grounds both times**: it destroys the property milestone 50 calls
load-bearing, that a pipe's read and write ends are separate capabilities with separate rights (a
program on the right of a `|` cannot write back up its own input, because it never holds a
capability that could). notes/pipes.md calls this "a design fork, and calef's"; §101 declines it a
second time for the identical reason and does not reopen it.

**(b) A buffering stage.** A component that speaks the sink contract on both sides and absorbs
what it is given, inserted as a barrier. This is the shape the original milestone 50 roadmap block
predicted buffering would arrive in if it earned its place. **Measured and refused, 2026-08-03**
(notes/pipes.md, "Buffering: measured"): a buffering hop costs roughly a second rendezvous
(`relay_rtt` vs `ipc_rtt`, about double), buys decoupling rather than bandwidth, and every pipeline
in this tree today is pure byte-moving with no producer-side work to overlap, so it would make
throughput worse for no correctness gain. It also does not remove the wall for a document larger
than its grant, which would deadlock again. §101 restates the refusal rather than reopening it.

**(c) Do nothing.** The refusal above is honest, non-deadlocking, and costs nothing further. This
is the status quo. It loses only in the sense that it is not an answer: `doc page.md` still cannot
render at a prompt, which is the one thing phase 1 and 2 of this milestone were building toward
(notes/manual.md's own "In brief": rendered for display, not shown raw).

**(d) `terminal_sink_caretaker` takes the tail stage's primary output** (the option the roadmap
block names). Not previously refused; DECISIONS §101 explicitly calls it "the right short-term
move" and reserves the specific wiring as milestone 40's own fork, undecided. This is the option
this note works out in full below, because it is the one live candidate.

## 2. What does this tree already do in the analogous case?

**Exactly this, for the second stream.** DECISIONS §67 (2026-08-03) already answers a materially
identical question for diagnostics: a program that declares a second output has it delivered by
default to `terminal_sink_caretaker`, a dedicated adapter process that holds the terminal endpoint
and hands out a sink, bypassing the shell entirely. `date`'s clockless complaint reaches the screen
"without passing through the shell at all" today. The refused alternatives at that decision (a
numbered-slot convention, a tagged opcode on one endpoint) were refused for reasons that generalize:
this system does not want ambient numbered conventions (Unix's fd 2), and it does not want to
conflate two kinds of thing on one channel again.

The precedent buys three things for free if extended to the primary slot:

- **The mechanism already exists and is proven.** `terminal_sink_caretaker` is built, gated
  (`script/shell-check` runs a `2>` case on both ISAs), and its header already states the general
  shape: "a program whose output slot holds an endpoint to this process is writing to the screen
  and cannot tell." Nothing about that sentence is specific to diagnostics.
- **Zero incremental authority for a program that already declares.** `date` already gets this
  adapter as its default second-stream destination; handing its *first* stream the same adapter by
  default adds no new capability the program did not already have a sibling of.
- **It is the same decision the pager and the colour bit need**, not three unrelated asks. Paging
  needs the terminal's `OP_READLINE`, colour needs to know a stage ends at a real screen rather than
  a file, and both are the same "does this child's output/input touch the terminal component
  directly" question notes/manual.md's "Where this goes next" already unifies.

## 3. What is the prior art outside the tree?

**Unix does not have this problem, because fd inheritance answers it for free.** Every process
inherits fd 1 from its parent at fork; a program's output already IS "the terminal" unless the
shell explicitly redirects it, because fd 1 was the terminal's fd before the fork ever happened. The
shell never reads a child's stdout to print it; the kernel's own tty layer does the printing, and
the shell is not in the data path for an unredirected command at all. This system inverted that on
purpose (§67's whole point: nothing here is ambient, a capability names a destination rather than a
number everyone agrees on), and the cost of the inversion is exactly the wall this note is about:
a nife shell is *in* the data path by construction, which is what makes it also the reader, which is
what creates the one-wait-point conflict Unix's design never has to face.

**seL4 has no shell-adjacent analogue** (seL4 systems are typically single-purpose, not
interactive), but DECISIONS §101 already did the relevant literature comparison for the general
multiplexing question: Mach port sets (the ancestor), Fuchsia `zx_port` (a queued variant), Redox
event queues (assumes a scheme/fd model this system does not have), and seL4's own notification
object (adopted, in `design/decisions/101-notification-objects.md`). None of those is about
*routing output away from a shell*; they are about a receiver waiting on more than one source. This
note's question is narrower and orthogonal: it is asking whether the shell should be a party to the
wait at all for a tail stage's primary output, not how it should wait on several things once it is.

**`man`/`apropos`/`mandb`, already this design's stated prior art for the whole milestone**, is
silent on this specific question because a Unix pager runs as a normal Unix process with an
inherited tty, so the equivalent question (how does a pager get the screen) never arises there
either, for the same fd-inheritance reason above.

## 4. Is the premise true?

Answered above, first section: yes, verified against `grant_plan::check_chain` and the real-prompt
transcripts in notes/pipes.md and notes/manual.md, on both architectures. Nothing here rests on the
roadmap block's own framing without independent confirmation.

**A second premise is worth checking too, because it changes the cost estimate below**: does the
tree already have a way for the shell to learn a child has exited, independent of reading its
output stream? **Yes, mostly unused today.** DECISIONS §26 built a kernel-delivered fault/exit
endpoint in milestone 22: "when a thread faults or exits, the kernel delivers a message to the
supervision endpoint its spawner designated," with a reserved cspace slot
(`abi::fault::FAULT_EP_SLOT`) and a kernel-stamped `(event code, tid, ...)` message, already proved
and already in the tree. Today `user/src/swish.rs` wires this **only for supervised (interruptible)
foreground jobs** (`spawn_interruptible`, watching a cooperative job-frame `DONE` flag, which is a
*different*, userspace-cooperative mechanism, not §26's kernel path). Ordinary sink-declaring
children (`date`, `wc`, `doc`) are spawned with no fault endpoint at all; the shell's only
completion signal for them today is draining their output to `OP_EOF`. **If a tail stage's primary
output moves to `terminal_sink_caretaker`, the shell needs a different completion signal, and §26's
already-built fault endpoint is sitting there unused for exactly this purpose.** That materially
lowers the cost of option (d): it is a wiring change (grant an already-existing kernel object at
spawn time and `RECV` on it instead of on the child's output), not a new kernel primitive.

## 5. What does each option cost, measured rather than asserted?

| Option | Kernel change | Wire/protocol change | What it costs | What it's already proven to cost (measured) |
|---|---|---|---|---|
| (a) Pull-based source | None | Collapses input+output onto one endpoint; touches `spawnproto`, every sink-contract reader/writer in the tree | Destroys the separate-rights property (§51's indifference, the pipe's read/write asymmetry) | Not benchmarked; rejected on the property loss before it reached measurement, twice |
| (b) Buffering stage | None | New program, `Prog` id, init entry | A second rendezvous per message | Measured: ~2x per-message latency (`relay_rtt` 1187ns vs `ipc_rtt` 2313ns is the userspace-hop cost this pays); does not raise the 16-byte-per-message ceiling that is the actual bottleneck (13.3 MiB/s vs Unix's 44 MiB/s at the same granularity) |
| (c) Do nothing | None | None | Milestone 40 stays PARTIAL; no line renders a page at a prompt, ever, on this branch of the design | N/A |
| (d) `terminal_sink_caretaker` takes primary output | None | One `spawnproto` bit (or a repurposed `DIAG_BIT`-shaped convention) for "this stage's output goes to the terminal by default"; the shell must additionally wire §26's fault endpoint for the spawn, where today it wires none | Narrows what the shell can observe about that child (it no longer reads its bytes); a completion-race caveat, below | §26's fault delivery is already built and proved (milestone 22); the incremental piece is shell wiring, comparable in size to `spawn_interruptible`'s existing job-watching path, not a new kernel primitive |

**The caveat option (d) owes, named exactly where notes/manual.md already named it and not
resolved there:** using kernel exit-delivery (§26) as the shell's "child is done, print the next
prompt" signal is *stronger* than the vaguer "wait for the child to exit" notes/manual.md worried
about, because §26's message is only sent after the thread is dead-until-reaped (DECISIONS §26.4):
a dead thread cannot enqueue any further `SEND`. So there is no race in which the *child itself*
paints the screen after the shell has moved on. **The race that remains is one hop further out**:
`terminal_sink_caretaker` is a separate long-lived process, and a child's `SEND` to it completing
(which is what could happen just before the child exits) only means the caretaker has *received*
the bytes into its own address space, not that it has finished its own `CALL` to `line_editor`
delivering them to the screen. If the shell prints its next prompt (a second, concurrent `CALL` to
`line_editor`) before the caretaker's trailing `CALL` lands, the two interleave at the terminal
server, which serializes them but not in a guaranteed order. This is a real, previously-unnamed
finding of this note (notes/manual.md flagged the shape of the question but not this specific
mechanism): the fix, if wanted, is not part of this decision and is deferred to the same list
DECISIONS §101 already carries (a bound notification the shell could `WAIT` on for "the caretaker's
queue for this client has drained," which needs the notification object §101 already decided to
build, later). **Absent that, the honest interim is that a page's last line and the next `$ `
prompt can interleave under contention**, which is a display glitch rather than a correctness or
confinement failure: no capability changes hands, no byte is misdelivered to the wrong reader, and
the caretaker's own BUGS section already documents that it serializes clients with no guarantee
about *which* pending message the terminal shows first when more than one is in flight.

## 6. How reversible is it, and who has already acted on it?

**Not cheap, and not because the wiring is large.** Per the *move fast on what can be undone* tenet,
this falls on the expensive side for two independent reasons:

- **It is a spawn-protocol decision**, which `crates/grant_plan/src/spawnproto.rs` calls out as
  "anything two binaries must agree on": both inits (`user/src/hello.rs` for aarch64,
  `crates/system_initializer` for riscv64, unified since milestone 96) and every program's manifest
  would carry the new convention. §67 set the precedent for how cheaply this class of change lands
  when it follows the existing shape (a bit, a manifest declaration, no new syscall), but it is still
  a wire format, which the tenet lists explicitly as one of the few genuinely expensive categories.
- **It changes what a spawned program's authority *implies* at this prompt**, which is exactly the
  kind of thing DECISIONS §101 itself flags as syscall-adjacent: "what a spawned program holds at
  this prompt is calef's."

**Who has already acted on it:** nobody, yet, in code. But three records already treat the
*direction* as settled and only the *specifics* as open, which narrows what calef actually has to
decide:

- DECISIONS §101 (notification objects), decided 2026-08-20 by calef, calls the narrowing "the right short-term move" and "the right
  permanent shape for a tail stage whose output goes to the screen," sequences it as step 1 ahead of
  the notification object, and explicitly declines to take it: "That is milestone 40's fork, and
  this decision does not take it."
- notes/manual.md (2026-08-18) already proposes the narrowing rule (apply only when the line has no
  `>` and no `|`, decidable from the plan before anything spawns, so nothing loses its ability to be
  redirected) and calls it "a proposal and not a decision."
- notes/pipes.md (2026-08-04) has carried the underlying trade as an open BUGS entry since milestone
  50: "a shell that wanted a program to print straight to the screen rather than through its own
  result endpoint could hand it over, and would lose the ability to redirect that program at all."

So this is not a fork with unstated options; it is a fork whose options and their costs are already
distributed across three files, none of which closes it. This note's contribution is collecting them
against the six questions, adding the fault-endpoint reuse finding (question 4) and the
caretaker-hop race finding (question 5), neither of which existed in the tree before.

## What is NOT part of this decision

**The notification object** (DECISIONS §101) is decided, specified, and sequenced as its own,
separate, later kernel milestone. Nothing here depends on it landing first; §101 says so explicitly.

**Phase 3, the graphical viewer**, waits on the display ladder (milestone 29's font rendering,
milestone 33's compositor) regardless of how this fork resolves.

## Recommendation, and the correction to this note's own question 6

**No recommendation was offered here originally**, on the reasoning that this is a syscall-adjacent,
two-programs-must-agree wire decision on the expensive side of *move fast on what can be undone*.
**That framing was checked against the code during the decision discussion and did not hold.**
`SINK_BIT`'s own contract (`crates/grant_plan/src/spawnproto.rs`) already makes the child's output
slot opaque to the program: "the shell delegates an endpoint and init puts it where the result
endpoint would have gone, so the child writes to a pipe or a file sink without knowing which."
Nothing about what a program declares changes; no manifest is touched, because the primary output
slot isn't something a program opts into. The actual work is shell-and-init default-routing logic
(when `sink == false`, delegate a `terminal_sink_caretaker` capability instead of the shell's own
read endpoint), the same shape init already uses for a `DIR_BIT` grant. That is cheaper than §67
itself, which had a real manifest-declaration axis this fork does not.

**Decided 2026-08-22 (DECISIONS §106): take it.** See that decision for the full record. This note's
contribution stands: the fault-endpoint reuse (question 4) and the caretaker-hop race (question 5)
are real findings, carried forward as the concrete spec for the build. Milestone 151 (notification
objects) is minted to track the race's fix, per §106's condition for taking this fork.

## BUGS

- **This note is itself a "where this goes next" for notes/manual.md's own section of the same
  name**, and the two will drift if only one is updated after the decision lands. Whichever lane
  builds the decided option should fold this note's finding (the fault-endpoint reuse, the
  caretaker-hop race) back into notes/manual.md and notes/pipes.md rather than leaving three files
  telling three overlapping stories.
- **The caretaker-hop race (question 5) is named, not measured.** No benchmark exists for how often
  a child's exit races its own trailing `terminal_sink_caretaker` delivery under real scheduling;
  the argument above is structural (two independent `CALL`s to one server, no ordering primitive
  between them) rather than a measured incidence rate.

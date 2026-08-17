# Interleavings, model-checked (loom)

The fourth leg of the analysis surface, after Kani ([verification.md](verification.md)), the fuzzers
([fuzzing.md](fuzzing.md)) and Miri ([undefined-behavior.md](undefined-behavior.md)). Milestone 80.

CLAUDE.md's fourth rule is *assume weak memory ordering*, and before this milestone **nothing in the
tree could falsify a violation of it**. That is the gap, stated plainly: we had a rule, a lot of
careful comments about acquire and release, and no instrument. The instrument found a real bug in
the first protocol it was pointed at that had not been designed with it in mind.

## Why the other three tools cannot see this

| Tool | What it checks | Why it misses an ordering bug |
|---|---|---|
| Kani | properties for every *input* | every harness in the tree is single-threaded, and the note that carries them says so: "concurrency is the sharpest edge of this limit" |
| The fuzzers | crashes on hostile bytes | one thread, one input at a time |
| Miri | aliasing, provenance, uninitialized reads | it runs *one* interleaving of a program, not the space of them |
| `script/test` under QEMU TCG | that the kernel boots and passes | TCG executes guest atomics conservatively and round-robins vCPUs; it explores almost none of the orderings aarch64 and riscv64 permit |
| `script/test --hvf` (milestone 81) | the real ISA on four physical cores | genuine orderings, **unsearched**: it samples what the silicon happened to do on that run |

Loom is the one that searches. It runs a concurrent test on the host and enumerates every thread
interleaving *and* every reordering the C11 memory model permits, including relaxed-ordering
surprises that no machine you own would produce today and some machine will produce tomorrow.

**And loom models C11, not ARM and not RISC-V.** That is the caveat to repeat rather than bury: it
narrows the gap, it does not close it. Litmus-level confidence about either ISA's own model would
need herd7-style tooling and is not this milestone. A tear loom finds is real; a clean loom run is
not a proof about the silicon.

## The survey: where the hand-rolled protocols actually are

The roadmap named three candidates (the per-CPU run-queue handoff, the reaper handoff, the IPC
sender queue) and the brief added two more (`crates/intrusive`, `crates/slots`). **Four of those five
have no atomic protocol at all**, and finding that out is most of what the survey was for.

| Candidate | What it actually is | Reachable by loom |
|---|---|---|
| The IPC sender queue (`crates/ipc`) | zero atomics. `Endpoint` is plain data under the `SCHED` `IrqSafeMutex` | nothing to explore |
| `crates/intrusive` (the run queues) | zero atomics. Single-owner with interrupts masked, plus an `UnsafeCell` | nothing to explore |
| `crates/slots` (the thread table) | zero atomics. Under `SCHED` | nothing to explore |
| The reaper handoff (`PerCpu::switched_from`) | one `AtomicU64`, both accesses `Relaxed`, written and read **by the same core** with interrupts masked. The atomic is interior mutability, not synchronisation | nothing to explore |
| The run-queue handoff | the migration inbox is an `IrqSafeMutex`; the *steal request slot* is the lock-free part | **yes**, and it is the pilot |

So the population is smaller than it looked, and that is a fact about the design rather than a gap in
the search: this kernel puts almost everything behind a ranked interrupt-safe lock on purpose
(DECISIONS §9, [locking.md](locking.md)). What is left, from a grep for every compare-exchange, swap
and fetch-op outside test code:

- **`crates/steal_request`** (new, milestone 80): the work-steal request slot. The pilot.
- **`crates/clock_proto`**: the clock page's **seqlock**. Cross-*address-space*, hand-rolled, with an
  explicit fence in the reader. This one the roadmap did not name, and it is where the bug was.
- **`kernel/src/smp.rs`**: the boot roster. `HWID`/`STARTABLE` written relaxed, then `ROSTER` stored
  with a release; readers acquire `ROSTER` and then read the arrays. A textbook array publication,
  correct as written, and single-shot at boot.
- **`kernel/src/arch/*/irq.rs`**: the interrupt-routing lottery, a compare-exchange per IRQ line.
  Rule 1 keeps it under `arch/`, so lifting it is a bigger question than this milestone.
- **`crates/user_rt/src/heap.rs`**: a hand-rolled userspace spin lock. `user_rt` is aarch64 inline
  `asm!` and does not compile for the host at all, so reaching it needs the lock lifted out first.
- Everything else is a **counter**: `fetch_add` on a statistic that a reader compares against zero or
  against its own earlier reading. Relaxed is right and there is no protocol.

## What was modelled

Nineteen harnesses <!--count:loom-harnesses--> across four crates <!--count:loom-crates-->, run by
`script/interleaving-check`.

Both numbers said sixteen and three until the 2026-08-17 documentation sweep, and this line is the
reason the sweep's gate work happened: `notes/counted-claims.md` cited it as the case a
`<!--count:NAME-->` marker could never help, because milestone 125's marker read digits and nothing
else. It reads a cardinal spelled as a word now, so the sentence is re-derived on every lint. The
count was wrong in both halves, and the note contradicted itself a few hundred lines down, where the
timing table already said "all four crates".

### `crates/steal_request`, the pilot

An idle core cannot reach into a loaded core's run queue, deliberately (DECISIONS §11), so stealing
is a message: the thief claims a one-slot mailbox in the victim with a compare-exchange from zero,
pokes it with a reschedule interrupt, and the victim swaps the slot back to zero at its next
scheduler entry and hands one thread into the thief's inbox. The slot was an `AtomicU32` field on
`PerCpu` with the compare-exchange written inline in `sched.rs`; it is now a crate, and the kernel
calls it rather than keeping a copy of the protocol. Same Phase-2 move `regions`, `ipc` and
`dma_validator` made for Kani.

| Harness | Property |
|---|---|
| `two_thieves_race_and_exactly_one_claim_is_granted` | "a thundering herd of idle cores collapses to one steal per victim per round", which is a claim about a compare-exchange under concurrency and therefore a sentence no single-threaded test could check |
| `a_granted_claim_is_served_exactly_once` | conservation: a granted request is in the victim's hand or still in the slot, never both and never neither, with the victim polling while the claim is in flight |
| `a_second_victim_cannot_serve_the_same_request` | the read and the clear are one step, so one request cannot be handed to two cores |
| `a_take_sees_everything_the_thief_wrote_before_claiming` | the release/acquire pairing publishes what preceded the claim (loom's `UnsafeCell` turns "visible" into a checked fact rather than an argument) |
| `a_relaxed_pairing_publishes_nothing` | **the falsification**, `#[should_panic]`: the same handshake with relaxed orderings must fail, and if it ever stops failing we want to hear about it |
| `a_stale_load_reading_costs_a_round_and_nothing_more` | §28's gossip claim, that a thief reads its victim's load relaxed and possibly stale on purpose: the interleaving where the victim drains between the load and the claim costs a wasted round and nothing else |

### `crates/clock_proto`, the second protocol

A seqlock over a shared page: the clock service writes, and every process holding a read mapping
reads, with no lock available between them because they are in different address spaces. Its own
documentation says the memory ordering is the point rather than decoration.

| Harness | Property |
|---|---|
| `a_reader_never_sees_half_a_publish` | the state and the offset are a matched pair; a reader that catches the writer mid-publish retries rather than blending |
| `the_generation_a_reader_sees_matches_the_pair_it_read` | `Reading::generation` is a value callers depend on (did the clock step under me), not a diagnostic, so it must agree with the pair it arrived with |
| `two_writers_serialise_rather_than_corrupt_the_page` | the crate says several processes may hold the page read/write and the compare-exchange serialises them; "would corrupt silently" is a claim about interleavings |
| `a_racing_reader_sees_an_unrecognised_page_or_a_whole_one` | `init` writes the magic last with a release, so a reader racing the first publish gets `UNKNOWN` or a whole page, never a recognised page with garbage in it |

### `crates/wake_handshake`, the block/wake protocol (2026-08-14)

The retrofit the fourth bench stop's audit asked for (notes/visionfive2.md said plainly that the
block/wake protocol had no loom coverage and that covering it meant an extraction). The protocol
behind `Thread`'s `on_cpu`/`wake_pending`/`wait_on`/`ipc_served`/`ipc_aborted` fields is now a
crate, the kernel calls its transitions at every wake, park, switch and finish-switch site, and
loom searches it on the host.

**This one extends the method, and the extension is worth naming.** The survey above counts atomic
protocols, and by that count the block/wake path had nothing to explore: every field is written
under `SCHED`. What the fourth bench stop demonstrated is that a lock-based protocol still has an
interleaving space, in the *gaps between critical sections*: a thread that parks itself releases
`SCHED` and keeps running until its core saves its context, and a waker can take the lock inside
that window. All three of this protocol's recorded races live in that gap. So the model puts
`SCHED` behind a `loom::sync::Mutex`, the thread's saved context in a `loom::cell::UnsafeCell`
written outside the lock (where `switch_to` writes the real one), each core's critical sections in
a loom thread in the kernel's program order, and lets loom order the sections. What it checks is
therefore interleaving logic plus the context handoff's ordering through the lock, not memory
orderings; there are none to check, and saying otherwise would be claiming coverage the model does
not have.

| Harness | Property |
|---|---|
| `a_wake_racing_a_switch_out_resumes_on_a_saved_context` | the wake-before-switch-out race (notes/intrusive-queues.md): whichever side of the race queues the thread (a direct wake after the switch-out, or a deferred wake completed by `finish_switch`), exactly one does, and the resume reads the context the victim's core saved |
| `a_wake_that_ignores_on_cpu_switches_into_an_unsaved_context` | **the reconstruction**, `#[should_panic]`: the pre-fix wake with the deferral removed, and loom must find the interleaving where the resumer takes a thread whose core is still saving its context |
| `a_stolen_thread_resumes_on_its_saved_context` | the steal edge of the same window: a preempted thread sits `Ready` with `on_cpu` still set, and the single-owner queue discipline (serve only on the owning core, after `finish_switch`) is what orders the thief's resume after the context save |
| `a_thief_that_pops_a_foreign_queue_steals_an_unsaved_context` | **the reconstruction**, `#[should_panic]`: break the single-owner rule and loom must find the thief taking a mid-switch-out thread |
| `an_undelivered_wake_racing_a_park_strands_nobody` | boot 8's gate under race: a spurious wake with nothing delivered is `Refused` in every interleaving, before or after the switch-out completes; the receiver stays parked and waiting, the real sender still completes the rendezvous, and the resume sees a delivery |
| `without_the_gate_a_spurious_wake_completes_an_empty_rendezvous` | **the reconstruction**, `#[should_panic("resumed with nothing delivered")]`: the pre-boot-8 wake (deferral kept, gate absent) strands the receiver in every interleaving, which is exactly what the bench recorded |

Two mechanics worth copying. The model's invariants are **real `assert!`s, not `debug_assert!`s**,
because this script compiles `--release` and a should-panic reconstruction with its tripwire
compiled out reports success while checking nothing. And each of the three reconstructions is the
*historical* semantics rebuilt locally in the harness (the fields are public), so the shipped
methods never carry a broken variant; reconstructing all three was cheap because each was a
deletion.

**Bounds, honestly.** Every harness runs to exhaustion: no `LOOM_MAX_PREEMPTIONS`, no branch
bound. That is affordable (the whole crate's search is ~10 ms) because the models are three or
four threads with two or three critical sections each, and that size is not modesty: one waker,
one victim core, one thief is the entire cast of every recorded race in this protocol. What the
small model cannot represent is stated in the crate's BUGS: the model checks the protocol *under*
the kernel's locking discipline (every site holds `SCHED`, run queues single-owner,
`finish_switch` before the next scheduler entry), and that the kernel keeps the discipline is
established by reading `sched.rs`, not by loom.

**What it found: nothing in the current protocol**, on the first run and after falsifying every
harness (deleting the deferral fails the first harness; deleting the gate fails the fifth; the
three reconstructions fail by construction). Like the pilot, the negative result has a reading:
all three of this protocol's races were found by flakes and bench boots first and fixed before
this model existed, and the model now holds the fixes in place where the next edit to `wake` or
`finish_switch` cannot silently undo them.

### `crates/canary_gate`, the corruption canary's serialization (2026-08-15)

The fourth extraction, and the first whose bug was in the extracted protocol itself rather than
held in place around it. The registry canary (first-silicon diagnostics, `kernel/src/sched.rs`)
serialized arm/check/disarm with two hand-written flags, `ARMED` and `IN_CHECK`, and the pair had
two holes. The observed one: a `check()` that lost the single-flight compare-exchange returned
silently, so the kernel test's decisive check could be swallowed by a timer tick's pass that had
read the watched byte before the flip; that is the `thead-c906` flake in
[cpu-models.md](cpu-models.md)'s BUGS, two failures in four runs of one tree. The latent one: a
checker between its `ARMED` load and its `IN_CHECK` win was invisible to `arm()`'s drain loop, so
a re-arm could rewrite the watch table and the shadow while a pass read them, a data race whose
torn `(base, len)` is a wild read in kernel space. No failure was ever traced to the second hole
(every boot arms at most once today, over ranges that never change), but a corruption instrument
that can itself read wild is not an instrument.

The replacement is one state word (`DISARMED`/`ARMED`/`ARMING`/`CHECKING`, every transition a
single compare-exchange) behind RAII guards; the kernel touches the plan and the shadow only
while holding one. Three harnesses: armer/checker exclusion over a shared `UnsafeCell` (loom's
access tracking makes a torn plan an error in itself, not a value someone must notice), disarm's
quiescence promise (after `disarm()` returns the caller writes the cell bare-handed), and
single-flight for two checkers. A fourth, scratch-only harness encoded the OLD two-flag spelling
against the exclusion model and loom falsified it on the first run, which is the falsification
discipline the wake_handshake section above describes, applied to a protocol that was actually
broken.

## What loom found

**A real weak-memory bug in the clock page's seqlock, on the first run.**

The writer claimed the sequence (a compare-exchange to an odd value) and then wrote the state and
the offset, with **nothing ordering the claim ahead of them**. Three of the four harnesses failed
immediately, all with the same shape: a reader observing the *new* offset beside the *old* state,
revalidating the sequence successfully because the odd value had not reached it either, and
returning the pair. A wrong wall clock, silently, from an API whose whole job is to make a torn read
impossible.

```
a torn reading: (1, 2000) is neither publish
a reader saw a recognised page with garbage in it: (0, 1000)
the generation disagrees with the reading it came with: Reading { state: 1, offset_nanos: 0, generation: 0 }
```

The fix is one line, a `fence(Release)` between the claim and the data stores, which is exactly the
`smp_wmb()` Linux puts in `write_seqcount_begin`.

**The part worth keeping is which fixes do not work.** The obvious reflex is to strengthen the
compare-exchange, and it was already `Acquire` on success with a comment saying that is what stops
the stores being hoisted above the claim. That comment is true and irrelevant to this bug:

| Attempt | Result |
|---|---|
| claim as `Acquire` (as shipped) | 3 of 4 harnesses fail |
| claim as `AcqRel` | 3 of 4 fail |
| claim as `SeqCst` | 3 of 4 fail |
| `fence(Release)` after the claim, claim left `Acquire` | all pass |

An acquire or release RMW orders accesses around *itself*. What a seqlock writer needs is its own
store ordered **ahead of the plain stores that follow**, and that is a store-store barrier between
the two, which no ordering on the RMW expresses. This is the kind of thing that is obvious once
stated and was not obvious to anyone who read the code, including the person who wrote the comment.

The reader's existing `fence(Acquire)` was checked the same way: removing it fails the same three
harnesses, with the writer's fence in place. So both halves of the pair are now checked rather than
argued.

**Why nothing else caught it.** It is unreachable on x86 (total store order gives the missing
barrier for free). QEMU's TCG explores almost none of the orderings that produce it. The ten host
tests in `clock_proto`, the kernel's clock tests on both ISAs, `script/verify`, `script/fuzz` and
`script/undefined-behavior-check` all passed before the fix and all pass after it: none of them asks
a question this could answer. The failure mode it would have produced on the VisionFive 2 is a
timestamp that is wrong by however far the clock last stepped, at a rate too low to reproduce and
with no instrument pointed at it. That is precisely the class of bug milestone 80 exists to retire
before the board lands.

### And the pilot found nothing, which is its own result

All six `steal_request` harnesses passed on the first run, and that is worth having for three
reasons rather than being a disappointment.

It converts three comments into checked facts (the herd collapsing to one claim, the release/acquire
pairing, the accepted staleness of the load reading). It leaves a **regression test** on a protocol
that is about to matter more: milestone 17's scheduler partitioning is explicitly sequenced behind
this one, and [sched-lock-inventory.md](sched-lock-inventory.md) says any design that replaces the
`SCHED` lock with messages wants its protocol born loom-checked. And the negative result itself is
informative: the steal slot is simple *because* the design pushed everything else behind a lock, so
"loom found nothing here" is evidence for DECISIONS §9's discipline rather than evidence against
loom.

The sharpest thing the pilot did produce came from breaking it on purpose:

| Break | Result |
|---|---|
| `claim` as a load then a store instead of a compare-exchange | `two_thieves_race_and_exactly_one_claim_is_granted` fails: "both thieves were granted the slot" |
| `take` as a load then a store instead of a swap | **all six still pass.** The atomicity of the swap is not load-bearing under the kernel's single-victim discipline: `serve_steal_request` runs on the owning core from a masked-interrupt handler and cannot re-enter itself, so there is never a second taker |
| ... and the same break, against a two-victim harness | fails: "one request was served to two victims: Some(4) and Some(4)" |

That is why `a_second_victim_cannot_serve_the_same_request` is in the suite: it is the one harness
guarding a property the running system does not currently need. The swap stays because it is one
instruction instead of two and because the discipline that makes the weaker version safe is written
nowhere the compiler can see it.

## EXAMPLES

Run everything:

```
$ script/interleaving-check
==> loom: the hand-rolled atomic protocols, every interleaving
running 6 tests
test interleavings::two_thieves_race_and_exactly_one_claim_is_granted ... ok
...
test result: ok. 6 passed; 0 failed
running 4 tests
test interleavings::a_reader_never_sees_half_a_publish ... ok
...
test result: ok. 4 passed; 0 failed
running 6 tests
test interleavings::a_wake_racing_a_switch_out_resumes_on_a_saved_context ... ok
...
test result: ok. 6 passed; 0 failed
```

Run one harness, which is what you do while iterating on a counterexample:

```
$ script/interleaving-check a_reader_never_sees_half_a_publish
```

Reproduce the clock bug, to see what a loom failure looks like before trusting a green run. Delete
the `fence(Ordering::Release)` from `ClockPage::publish` and:

```
$ script/interleaving-check -p clock_proto
thread '...a_reader_never_sees_half_a_publish' panicked at crates/clock_proto/src/lib.rs:738:26:
a torn reading: (1, 2000) is neither publish
```

Add a protocol of your own. Four steps, and the third is the one that is easy to get wrong:

1. Put it in a crate that compiles for the host. A protocol inside a `no_std` binary is unreachable,
   which is rule 7 pushing in the same direction it already pushes for Kani.
2. Swap the atomics behind the cfg:
   ```rust
   #[cfg(loom)]
   use loom::sync::atomic::{AtomicU32, Ordering};
   #[cfg(not(loom))]
   use core::sync::atomic::{AtomicU32, Ordering};
   ```
   and add `[target.'cfg(loom)'.dependencies] loom = "0.7"` to the crate's manifest.
3. **Give every spin loop a yield.** Loom's scheduler is cooperative, so a thread spinning on
   `core::hint::spin_loop()` can starve the writer whose progress it is waiting for and the model
   never terminates. `clock_proto` has a `spin_hint()` helper that is `loom::thread::yield_now()`
   under the cfg and the hint otherwise; copy that shape.
4. Add the crate to `script/interleaving-check`'s package list, write the harnesses, and **falsify
   each one** before believing it.

## The cost, measured

| | |
|---|---|
| Runtime, all four crates, warm | **under a second** wall on an M-series laptop (0.2 s measured 2026-08-14, with `wake_handshake`'s six harnesses adding ~10 ms of search; `canary_gate`'s three, measured 2026-08-15, add under 10 ms) |
| Runtime, cold (compiling loom and its 28 transitive crates) | ~6.5 s |
| Crates added to `Cargo.lock` | **28** (loom plus `generator`, `scoped-tls`, `tracing`, `tracing-subscriber` and their trees) |
| Crates compiled by an ordinary `cargo build`, `cargo test`, `cargo clippy` or `script/test` | **zero** |

That runtime is why it could be a gate and is not one yet; see BUGS.

### Where the dependency is gated, exactly

`loom` is declared under `[target.'cfg(loom)'.dependencies]`, which is tokio's own pattern. Cargo
evaluates `cfg(loom)` as false for every real target, so:

- `cargo build`, `cargo test`, `cargo clippy`, `script/test`, `script/lint` and every CI job never
  resolve it, never download it and never compile a line of it.
- `cargo tree` does not show it. `cargo deny` does not see it either: `deny.toml` narrows the graph
  to the five targets this project builds for, and `cfg(loom)` is true for none of them.
- The one place it *is* visible is `Cargo.lock`, which records every possible dependency regardless
  of activation. Twenty-eight lines' worth. That is the honest cost of the decision, and it is the
  cheap end of DECISIONS §46: nothing ships it, nothing links it, and removing it is deleting two
  manifest stanzas and two cfg blocks.

`--cfg loom` is set by `script/interleaving-check` and by nothing else in the tree.

## BUGS

- **Loom models C11, not aarch64 and not riscv64.** Said three times in this note on purpose. A
  failure it reports is real; a clean run is not a proof about the silicon. Milestone 81's HVF leg
  is the complementary evidence, and it is a sample rather than a search.
- **Not a gate, and not in `script/test` or `script/gates`.** The runtime would allow it today (under
  a second) and the reason it is out is different: the search cost of a loom model is exponential in
  the number of threads and the length of the protocol, so a harness added six months from now can
  take minutes without anyone intending it to. A gate whose cost is a step function is a gate that
  gets skipped. Revisit when there is a CI job for it.
- **It cannot see the reschedule interrupt.** `steal_request`'s liveness claim is that a poked victim
  eventually reaches a scheduler entry and clears its slot, and until it does, every other idle core
  is locked out of that victim. That is outside the model in both directions: loom does not know
  about the SGI, and it does not know about the timer tick that makes the thief retry.
- **The harnesses are small on purpose, and small is a bound.** Two thieves, one victim, two polls;
  two writers, one reader, one publish each; one waker, one victim core, one thief in the
  block/wake models. The protocols are symmetric enough that a third
  participant explores no new state *in these cases*, and that is an argument, not a proof. Every
  harness carries reachability flags (the `Reached` type) so a bound that quietly empties the
  interesting branch fails loudly, which is `kani::cover!`'s job done by hand.
- **`crates/user_rt`'s spin lock and the interrupt-routing lottery are unmodelled.** Both are named
  in the survey above with the reason: one does not compile for the host, and the other lives under
  `arch/` where rule 1 keeps it. Neither is a small retrofit.
- **The `#[cfg(loom)]` code is invisible to `script/lint`**, exactly as the Kani harnesses were before
  milestone 113 built a shim for them. Here it costs one flag instead of a shim:
  `script/interleaving-check` compiles the harnesses with `-D warnings`, so it lints them itself. If
  a third tool ever gets its own cfg, the shim question comes back.

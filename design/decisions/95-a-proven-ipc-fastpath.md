# 95. A hand-written IPC fastpath, and whether it can stay proven

**Status: PROPOSED.** Raised 2026-08-18 by calef, in one question: *"Can we do the fast path and
still make it proven?"* It follows milestone 132, whose gate measured the gap and deliberately did
not close it.

**What is blocked until this is answered:** nothing is blocked. The gate holds the number still, and
`ipc_fastpath` at 5.6 KiB against a 4 KiB target is a gap that is not widening. This is a decision
about whether to spend, not a decision something is waiting on.

## The short answer

**Yes, and the precedent is the strongest one available: seL4's C fastpath is inside its
functional-correctness proof rather than being the unverified escape hatch.** That inverts the usual
expectation about high-assurance software, and it is worth stating precisely because the detail
changes what we would have to do:

- The fastpath is machine-checked to **refine an executable detailed specification of the fastpath**.
  There is no abstract-level specification of it; the tie to the abstract spec runs through the
  slowpath.
- **An assembly variant was written and is neither verified nor used.** That is a direct instruction
  for us: a fastpath written in Rust can be reasoned about, and one written in assembly buys the last
  few percent and leaves the proof behind.

## What "proven" means in this tree, which is narrower than seL4's and is the whole design constraint

`script/verify` says it in its own header: the proofs are a function of the harness crates and their
transitive dependency closure, and **`cargo kani` never compiles the kernel.** So nothing in
`kernel/src/` is directly proved, and no fastpath living there could be.

What we have instead is better than it sounds, and notes/verification.md names the property: the
decision core is a pure crate and **the kernel calls it rather than keeping a copy.**
`crates/ipc::Endpoint` is the kernel's real endpoint state, not a model kept in sync, which is why
that note calls it "the first place a proof reaches all the way into the running kernel". Six
harnesses cover the rendezvous decisions, including `send_rendezvous_iff_a_receiver_waited`.

So the question is not "can Kani prove the fastpath". It is **"can the fastpath be built so that the
part which decides is still the proved part".**

## The obligation, and it is small and the right shape

The hazard a fastpath introduces is not slowness or unsafety. It is **divergence**: two paths that
disagree about when a rendezvous happens, where the fast one is the one that runs and the proved one
is the one nobody executes. That failure is invisible to every instrument this tree owns, because
both paths individually look correct.

It is also exactly what a pure predicate kills. The shape:

1. Add an eligibility predicate to `crates/ipc`, beside the state machine it is about.
2. Prove, in the same inductive-step style as the six existing harnesses, that **eligible implies the
   general path would have returned `Rendezvous` with the same partner**.
3. Require the kernel's fastpath to call that predicate rather than reimplement it, which is the
   Phase-2 rewire's argument applied one layer down.

**The proof is one-directional, and that is what makes it cheap.** The predicate must be
conservative: any doubt bails to the slowpath. So we owe "eligible implies same outcome" and we owe
nothing about the ineligible case, because being wrong there costs performance and never correctness.
seL4's eligibility test has the same character, a conjunction of simple state tests.

## What stays unproven, stated plainly

The mechanism. Register save and restore, the direct switch, the address-space swap. None of that is
Kani-reachable today (`switch_to` is not), so **the fastpath does not move the proof boundary; it
puts more code on the unproved side of it.** That is the honest cost and it should not be dressed up:
a fastpath grows the unverified mechanism while keeping the verified decision.

The mitigation this tree can afford is differential rather than deductive. The slowpath survives by
construction, since the fastpath must be able to bail to it, so a build that forces the slowpath and
compares outcomes against a build that does not is a real gate and is rung two.

### Amended 2026-08-18: the boundary is further away than the paragraph above implies

calef read the original as trading provability for specialisation and asked whether there is not
already an unproven subset, and therefore a judgement rather than a line. **He is right, and the
correction is larger than the question assumed.**

`script/verify` states it: the proofs are a function of the harness crates and their dependency
closure, and `cargo kani` never compiles the kernel. So the tiers are:

| tier | size | what defends it |
|---|---|---|
| pure-logic crates | the proved set | Kani, over every input |
| Rust in `kernel/src` | 47,525 lines, **none of it proved** | types, the borrow checker, the QEMU suite |
| `unsafe` within it | 394 blocks, functions and impls | review and `// SAFETY:` comments |
| hand-written assembly | 1,152 lines, 2.4% of the kernel | read once, notes/arch-audit.md |

**The assembly is not the unproven subset. It is the least-defended layer inside an already-unproven
kernel.** `ipc_send` is unproved today; what is proved is the decision core it calls. A Rust fastpath
therefore crosses no proof boundary at all, and the sentence above, while true, puts the line nearer
than it is.

The quantity actually being spent is **unverified TCB**: how much unproved code sits on a
security-critical path, and what compensating control covers it. That is a trade this tree has
already priced once. Milestone 20 accepted a new architecture knowing it "enlarges the unverified TCB
(one hand-written boot/MMU/trap/syscall layer per arch, the least-verifiable code)" and paid for it
with sequencing rather than with a refusal. §14 concedes the frame outright: not a seL4-scale proof of
the whole kernel, because that is person-decades.

### The judgement, as seL4 wrote it, and the part that changes the recommendation below

§4.7 of the retrospective sets the price explicitly: **"For seL4 we were willing to tolerate no more
than a 10% degradation in IPC performance"** as the cost of verifiability. They then beat it, at 188
cycles one-way on ARM11, roughly 10% *better* than the fastest IPC they had measured on any kernel on
that hardware, with the verdict "Abandoned: Assembler code for performance" and the flat statement
that assembler implementations are no longer justified by performance arguments. OKL4 had already
dropped its assembler fastpath commercially, on maintenance cost alone and with no verification
motive.

**How they got there is the fact that matters here**, and the original text of this decision missed
it. The fastpath was hand-crafted in C by "manually re-ordering statements, making use of **(verified)
invariants that the compiler is unable to determine by static analysis**". The proof was not a tax on
the optimisation. It was an **input** to it: a verified invariant licenses a reordering the compiler
cannot justify by itself.

That is a stronger argument for option 2 than the one this file originally gave, which was only that
the predicate cannot be retrofitted honestly. It is also why the three decisions here are separable
and should not be taken as one:

- **Assembly** is refused on two grounds that are independent of proof: maintenance cost, and the
  evidence that it no longer buys anything.
- **A Rust fastpath** is an unverified-TCB increase, the same class of decision milestone 20 made.
- **The predicate** strictly increases proof coverage, and is what makes aggressive optimisation of
  the mechanism defensible rather than merely tested.

## The risk is not where it looks, and this is the part worth arguing about

The assembly is not the dangerous part. **The scheduling semantics are.**

`ipc_send` today does not switch to the receiver on a rendezvous. It fills the mailbox, calls
`handshake.serve()`, wakes the receiver onto the run queue and returns, so the sender keeps running.
seL4's fastpath switches **directly** to the receiver and donates the remaining timeslice, and most
of its win is that, not the byte count.

So a real fastpath here is a change to when threads run, not a trimmed copy of an existing path, and
it lands on the machinery with the subtlest invariants in the tree: the handshake states, the boot-8
gate that `serve()` exists to pass, the reaper's interaction with a thread that is queued and
Blocked, and §26's dead-sender corpse case that `ipc_recv` handles inline. **Those are the
invariants that would need to hold under a second path, and none of them is about speed.**

## The other thing it touches, which makes it calef's rather than a lane's

`syscall::dispatch` at 2,024 bytes is the largest single item in the measured footprint, and skipping
it means decoding the operation before the general decoder runs. **That is the syscall surface**
(§10, §16), which this project treats as a boundary rather than a habit, so the shape of the check
is a design fork even though no new syscall number appears.

## Options

1. **Do nothing.** The gate holds 5.6 KiB. We are within the right order of magnitude of the target
   and the cost of being over it is unmeasured, because nothing here models a cache. Cheapest, and it
   keeps the claim "the IPC decision the kernel runs is the proved one" completely unqualified.
2. **The predicate and the proof first, with no fastpath.** Land the eligibility predicate and its
   harness in `crates/ipc`, and have the existing slowpath call it as a no-op assertion. This buys
   nothing in bytes and makes the later fastpath a mechanical change against a proved precondition.
   It is also the honest way to find out whether the obligation is as small as this file claims.
3. **A Rust fastpath behind the proved predicate**, direct-switching to the receiver, bailing to the
   slowpath on anything unusual, with a differential gate. This is seL4's shape and the only option
   that closes the gap.
4. **An assembly fastpath.** Explicitly refused here rather than listed as a live option: seL4 wrote
   one, did not verify it, and does not use it. Recorded so the next person does not rediscover it.

## Recommendation

**Option 2 now, option 3 only behind a measurement.**

The predicate and its proof are worth having on their own, because they turn "the fastpath would be
correct" from an argument into a harness, and because they cost little and are the part that cannot
be retrofitted honestly. That work is startable today and touches no syscall surface.

The fastpath itself should wait for milestone 127's Jetson TX1 and milestone 74's cycle counters.
The reason is this project's own standard rather than caution: **the entire case for a fastpath is a
measurement we cannot currently take.** We do not know what the 5.6 KiB costs, because nothing in the
tree models a cache, and building a second IPC path on an estimate is the argument-instead-of-measure
move that the benchmark discipline exists to refuse.

**What happens if calef says no to all of it:** nothing breaks. The gate keeps the number from
growing, milestone 132 records the gap, and the trigger stated there still stands.

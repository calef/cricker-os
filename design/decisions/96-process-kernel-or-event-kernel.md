# 96. Process kernel or event kernel, and how to decide it

**Status: PROPOSED.** Raised 2026-08-18 by calef, asking how to *think about* this as a decision
rather than asking for the answer. It exists because notes/l4-lessons.md found that this tree has a
process kernel, that seL4, OKL4 and NOVA all replaced theirs with event kernels, and that **nothing
anywhere in this repository records the model as a choice.** It is what the code grew into.

**What is blocked: nothing.** The system runs. This is a decision about whether to spend, and the
useful output of writing it down is that the question stops being invisible.

## What the two things are

A **process kernel** gives every thread its own kernel stack, so a thread that blocks inside the
kernel simply stops on that stack and resumes there. That is what we do: `STACK_PAGES` is 6, plus a
guard page, and the word "continuation" appears nowhere in `kernel/src`.

An **event kernel** has one kernel stack per core. A thread that must block stores a **continuation**,
a small explicit record of what to do when it resumes, and the kernel stack unwinds. seL4, OKL4 and
NOVA are all this.

## What class of decision it is, which is the part calef actually asked

Run this tree's own test from CLAUDE.md, which is **not** "can I revert the commit" but **"who else
has already acted on this"**:

- **No user program can tell.** The syscall ABI does not change. Nothing about `SEND`, `RECV` or
  `CALL` is different under either model.
- **No wire format, no name, no published fact, no stored secret.** None of the irreversible
  categories is touched.
- So by the stated test this is **reversible**, and it is expensive in exactly the currency agents
  made cheap: kernel code.

**But it has a property the reversible category does not usually have, and this is the thing worth
naming.** Every blocking operation written *after* the decision is written in the chosen idiom. The
cost of switching therefore **grows with every feature added**, and what grows is not only code but a
convention in the next contributor's head, which CLAUDE.md lists among the things that did **not** get
cheaper. So it is neither of the two categories that file describes. It is a third: **reversible, but
appreciating.**

The practical consequence is the opposite of the usual advice for reversible decisions. Deferring is
not free, so the thing to avoid is not a hasty choice, it is an *unmade* choice that keeps getting
more expensive while nobody is looking. Which is precisely the state this tree was in until the audit.

## The four inputs, and three of them are already settled

**1. Memory. Dead as an argument here, and it was the paper's headline.**

Warton measured the event kernel's per-thread memory at a quarter of the process kernel's. Our
exposure is bounded by construction: `MAX_THREADS` is 128, so kernel stacks total **3.00 MiB, static**.
An event kernel would spend `MAX_CPUS` (8) stacks, 192 KiB, plus larger TCBs for the continuations.
The saving is **2.81 MiB, or 0.069% of a 4 GiB board.** The original result came from resource-starved
embedded systems. We are not one, and this argument should not be recycled as though we were.

**2. Shrinking the stacks instead. Closed off, and checked rather than assumed.**

The obvious cheap alternative is to keep the model and take part of the memory win by making stacks
smaller. It is not available. Milestone 84's high-water instrument measured thread stacks at **11,352
bytes on aarch64 and 11,672 on riscv64**, across roughly 420 stacks over the full suite. Stacks were
*raised* from 4 pages to 6 on 2026-08-15, because milestone 124 found `spawn_on` carrying a 4,592-byte
frame that could step past a one-page guard. At 16 KiB, measured usage was already 69 to 71%. There
is no slack to reclaim.

**3. Performance. The live argument, and we cannot currently measure it.**

Warton found the event kernel **generally within 1% on micro-benchmarks** and **20% better on a
multi-tasking workload** (AIM7). Read that pairing carefully, because it is the uncomfortable part:
**every instrument this project owns is a micro-benchmark.** `ipc_rtt`, `ipc_rtt_el0`, the icount
tripwire and milestone 132's footprint gate would all show approximately nothing. The one number that
would move is the one we have no way to produce.

**4. Verification. The paper's reason does not transfer; a weaker version might.**

Their stated argument is specific: an event kernel avoiding in-kernel page-fault exceptions
"preserves the semantics of the C language", and staying inside C's semantics reduces verification
complexity. **We are not verifying C and Kani does not reach `kernel/src` at all** (§95), so that
argument is simply not ours.

The version that may transfer is an inference and is marked as one: a continuation is **explicit,
data-shaped state**, where a kernel stack is **implicit, control-shaped state**, and explicit
data-shaped state is what a model checker can reason about. If the verification frontier ever moves
into the kernel, the event kernel is the shape that would let it. Nobody has demonstrated that here,
and it should not be quoted as though somebody had.

**On dismissing the whole thing as an embedded-era artifact**, which input 1 invites: the authors
close that door themselves. The choice "was driven initially by the realities of resource-starved
embedded systems and later the needs of verification", but "the approach's benefits are not
restricted to those contexts, and we believe it is **generally the best approach on modern
hardware**."

## The decoupling that makes this decision less urgent than it looks

**Direct process switch does not require an event kernel.** Traditional L4 had direct process switch
*and* per-thread kernel stacks; the event kernel arrived years later with L4-embedded. So the cluster
notes/l4-lessons.md identified is separable after all:

- **Row 14, direct process switch**, is available to us *as a process kernel*, and it is the change
  §95 is already deciding about and milestone 132 already measured the ground for.
- **Row 11, the kernel model**, is the expensive commitment underneath it.

That matters because it means the cheap win is not gated on the expensive decision. We can take the
fastpath work without settling this, and we should not let the larger question hold the smaller one.

## The question that actually settles it

CLAUDE.md's test for an argument that sounds like design: **would I still choose this if both options
were the same amount of work?**

Answered honestly: **no, I would not choose the process kernel at equal cost.** The paper recommends
the event kernel for modern hardware in its own voice, the direct-switch and Benno-scheduling wins
fall out of it rather than being bolted on, and the explicit-state property points the right way for a
verification project. **So the case for staying as we are is an effort argument**, and per §92 it has
to say so in those words rather than dressing itself as architecture.

What makes the effort argument *legitimate* here rather than merely convenient is input 3. We would
be spending a pervasive kernel rewrite against a benefit we cannot measure, on the word of a
twenty-year-old result from a different kernel on ARMv5. This project's standard is measure, do not
argue, and adopting an event kernel today would be arguing.

## Recommendation

**Do not decide it yet, and make the thing that blocks the decision explicit so it stops being
invisible.** Three steps, in order, none of which is the rewrite:

1. **Take the cheap half separately.** Pursue direct process switch under §95 as a process kernel.
   It is independently justified, and it retires the part of the gap that is not expensive.
2. **Build the instrument that could decide it.** The blocker is that a multi-tasking workload is the
   only place the difference appears, and we have none. That wants a roadmap block of its own, and it
   is useful well beyond this question: milestone 25's cross-OS comparison has the same hole, and
   milestone 127's TX1 is where such a number would finally mean something.
3. **Re-open this section when that number exists**, or sooner if a customer-path workload starts
   creating threads in the hundreds, which is the condition under which input 1 stops being dead.

**If calef says no to all of it**, the outcome is that the tree keeps a process kernel and now says so
on purpose, with the reason and the trigger written down. That alone is worth more than the status quo
it replaces, which was an unexamined default.

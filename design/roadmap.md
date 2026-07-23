# Post-v1 milestone roadmap (proposed)

The eleven milestones in DECISIONS.md were the plan, and they are done. This is the proposed
roadmap past them, drawn from the architecture discussion comparing Windows NT, macOS/XNU, and
Linux, and from the gaps the code already flags. Nothing here is committed. It is a `design/`
proposal like the others in this directory: a place to argue the cut and the order before any of it
becomes a numbered milestone in DECISIONS.md.

Two facts shape the whole list.

**cricker-os already _is_ most of the clean-slate recommendation.** No fork (explicit `Spawn`
endowment: reading one literal tells you a process's whole authority). Share-not-move frames with
rights narrowing at send. Endpoint-only naming, no way to name a receiver. Memory safety as a
language property. So this roadmap is not "adopt the principles." It is "close the specific gaps
between the principles and this code," and the gaps are few.

**The goal is understanding, not reach** (CLAUDE.md). A milestone earns its place by what it
*teaches*, not by what it lets the OS run. That is why the compatibility question below is framed as
a study rather than a viability constraint, and why a perf item with no payoff on QEMU can still be
worth building for the mechanism it teaches.

## The milestones

| #  | Milestone | What it teaches / delivers |
|----|-----------|----------------------------|
| 12 | Call/Reply IPC: a one-shot reply capability | Reply-to-caller as a kernel guarantee; retires the per-client reply endpoint |
| 13 | Capability revocation + untyped reclamation | A derivation tree and recursive revoke; reclaim a page from a live peer |
| 14 | Kernel objects from untyped: remove the kernel heap | Retype TCBs, endpoints, page tables; §10's deferred axis, finished |
| 15 | Tagged address spaces (ASIDs) | Stop flushing the whole EL1 TLB on every user switch |
| 16 | Real hardware + SMMU-backed driver isolation | The IOMMU makes driver isolation real; the shadow ring becomes belt-and-suspenders |
| 17 | Multikernel-leaning scheduler (research) | Partition the shared thread table and endpoints; message-passing where one lock now sits |

The order is capability-core first (12-14, the project's thesis), then the road to real machines
(15-16), then optional research (17). Several of these already have their design worked out; the
blocks below point at it.

### 12. Call/Reply IPC: a one-shot reply capability

**Deliverable.** A kernel-minted, single-use reply capability handed to a server on a `Call`, so it
can answer *whoever* called without being individually wired to them, and can answer exactly once.

**Why first.** Small, self-contained, and it retires a real wart: request/reply currently burns two
endpoints, and the console server is correct only *by convention* (it is single-threaded and IPC is
synchronous rendezvous), not by construction. The moment a server serves clients it was not wired
to, or a thread pool shares a reply path, the convention breaks.

**Prior art.** Mach's `send-once` right (it had this in the 1980s); seL4's `Reply` cap minted on
`Call`, with a call chain that also enables priority donation.

**Detail.** DECISIONS.md "Open design ideas" (Call/Reply) and notes/ipc-naming.md already work the
functional and safety triggers. It widens the §4 syscall surface (a `Call` method, a `Reply`
object), so it is a real decision, not a speculative add. This milestone turns that entry into code
and gives it its own numbered §.

### 13. Capability revocation + untyped reclamation

**Deliverable.** A capability-derivation tree and a recursive `revoke` that unmaps an object from
every holder, so authority can be retracted from a live peer and a page can finally be reclaimed.

**Why.** The deepest thing left in the capability model, and it unblocks everything about
reclamation. `untyped::destroy` already exists, dead, as a tripwire: today frames are spend-only and
never reused, which is the *only* reason teardown's dangling mappings are safe rather than a
use-after-free.

**Prior art.** seL4's CDT plus recursive revoke, a first-class kernel object there.

**Blocking precondition.** DECISIONS.md "Open design ideas" (revocation) and
notes/capability-lifecycle.md state the invariant this must not break: **no reclamation of any kind
until revocation lands.** This milestone is that work, and the precondition is why it comes before
14.

### 14. Kernel objects from untyped: remove the kernel heap

**Deliverable.** Retype TCBs, endpoints, and page tables out of untyped memory, the way milestone 11
already does for user pages, and delete the kernel heap and slab.

**Why.** This finishes §10's deferred axis. Milestone 11 stopped the kernel allocating for *user*
memory; the kernel's own objects still come from its heap. It is also the real prerequisite for the
"small enough to verify" endgame: seL4's proof leans on a kernel that never allocates. Biggest item
here, and the seL4 long tail by reputation.

**Gated on a decision** (below): whether verifiability is actually the goal. If Rust-safety is the
stopping point, this is a purity exercise with a real but smaller payoff (the kernel-heap-exhaustion
class disappears entirely). If formal proof is the goal, it is on the critical path.

### 15. Tagged address spaces (ASIDs)

**Deliverable.** Give each address space an ASID so a context switch stops doing `tlbi vmalle1is`
(discard every EL1 translation, machine-wide) and instead flushes nothing.

**Why.** `mmu::set_ttbr0` does the sledgehammer flush today and says so: "no ASIDs yet ... every
address space uses ASID 0 ... ASIDs are the fix." A self-contained exercise in ASID allocation and,
more interestingly, ASID *reuse* (there are only so many; a real system recycles them and must flush
exactly the reclaimed one). It has no measurable payoff on QEMU, which does not model TLB cost, so it
is here for the mechanism, and as the honest prerequisite for reasoning about the
Spectre/address-space-switch cost the discussion raised. You cannot measure that cost while every
switch already flushes the world.

**Detail.** Standard aarch64 (ASID in TTBRx, `TCR_EL1.A1`); kernel/src/arch/aarch64/mmu.rs carries
the deferral.

### 16. Real hardware + SMMU-backed driver isolation

**Deliverable.** Port to hardware with an IOMMU in front of the device (Raspberry Pi 4 class, or
virtio-pci behind QEMU's SMMU) and confine driver DMA with the SMMU's stage-2, behind or instead of
the software shadow ring.

**Why.** This is where the discussion's strongest pro-microkernel argument finally becomes true for
us. On QEMU `virt` there is no IOMMU over virtio-mmio, so driver isolation is real only because of
the shadow descriptor ring we wrote (notes/dma.md). Real hardware makes it real in silicon, and the
shadow ring becomes belt-and-suspenders rather than the sole defense. Keep the `Virtio` capability
shaped so it can sit behind either.

**Prior art.** design/driver-domains.md already works the principled version (a driver per VM,
cricker-os as an EL2 hypervisor, SMMU stage-2). Hardware-gated, and impossible under HVF.

### 17. Multikernel-leaning scheduler (research, optional)

**Deliverable.** Partition or replicate the two structures still shared under one `SCHED` lock (the
thread table and the endpoint array), toward per-core state with message-passing where a lock now
sits.

**Why.** The SMP work (§11) already went most of the way: per-CPU run queues, per-CPU current and
held-rank, cross-core placement by inbox-plus-SGI with no shared run-queue lock. What remains shared
is the thread table and endpoints. Barrelfish's multikernel (treat the machine as a distributed
system, message-passing between cores) is the honest research answer for NUMA and P/E asymmetry.
This is a direction, not a commitment: keeping the one lock is a perfectly honest choice at the
current scale, and worth saying so rather than feeling the machine is owed a message-passing thread
table.

## Two decisions this roadmap forces

Not milestones. Forks to settle, each gating work above.

- **The verification endgame.** Is Rust's memory safety the stopping point, or is formal proof the
  goal? Rust already removes the ~70% memory-safety CVE class with no proof and near-zero cost, which
  is most of what verification buys in practice. Formal proof additionally buys functional
  correctness and information-flow, and its prerequisite is milestone 14 (no kernel heap). Decide this
  *before* 14, because it decides whether 14 is on the critical path or an optional purity win. For a
  learning kernel, "Rust-safety is the floor, formal proof is out of scope" is a defensible answer,
  and it is better stated out loud than drifted past.

- **POSIX posture.** None, or a small personality built to *understand* why every clean-slate system
  (Fuchsia's Starnix, Redox's relibc) keeps re-growing the old ABI. The discussion is right that
  compatibility decides a *product's* reach, but this is not a product and reach does not bind us. So
  the question is not "how much POSIX for adoption" but "is a minimal POSIX personality worth building
  as a teaching exercise." Decide the intent before any compatibility code, so it stays a study and
  not a slow slide toward a second kernel.

## The rival worth understanding, not building

eBPF is the strongest competing answer to the question this whole architecture asks: safe kernel
extension through *verification* rather than *isolation*, with no IPC cost. Worth reading as the
other fork. It does not undercut the thesis so much as relocate the cost: the eBPF verifier is itself
a large, subtle, repeatedly-CVE'd component, so "the verifier is the TCB" is its version of the
problem, not an escape from it. No milestone; a reading item.

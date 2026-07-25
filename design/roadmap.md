# Post-v1 milestone roadmap

The eleven milestones in DECISIONS.md were the plan, and they are done. This is the roadmap past
them. It began (see the git history of this file) as an uncommitted `design/` proposal drawn from the
architecture discussion comparing Windows NT, macOS/XNU, and Linux. It now has a **committed
destination**: DECISIONS §14, a verified-Rust capability microkernel that runs real workloads. That
commitment re-ordered this list and resolved two of the forks it used to end with.

Three facts shape the whole list.

**cricker-os already _is_ most of the clean-slate recommendation.** No fork (explicit `Spawn`
endowment: reading one literal tells you a process's whole authority). Share-not-move frames with
rights narrowing at send. Endpoint-only naming, no way to name a receiver. Memory safety as a
language property. So this roadmap is not "adopt the principles." It is "close the specific gaps
between the principles and this code," and the gaps are few.

**Understanding is the method, not a cap on ambition** (CLAUDE.md). The way we work is unchanged:
write it together, explain the hardware, write the notes. What changed with §14 is that the work now
serves a destination (the demonstrator), so a milestone earns its place by moving toward a *verified
core running real confined workloads*, not only by what it teaches in isolation.

**Verify inward from the capability core.** §14 makes verification the goal, and the frontier is the
pure-logic §7 crates. The `caps` model is proved already (`script/verify`, notes/verification.md);
IPC and the MMU invariants are next. This threads through the list rather than being one milestone.

## The milestones

| #  | Milestone | What it delivers | Serves §14 by |
|----|-----------|------------------|---------------|
| 12 | Call/Reply IPC: a one-shot reply capability | Reply-to-caller as a kernel guarantee. **Built, §12.** | the IPC the TCB must get right |
| 13 | Capability revocation + untyped reclamation | Unmap a page from every holder; reclaim a region safely. **Built (frame scope), §13.** | safe teardown, a TCB property |
| 18 | Verify the capability core, then spread inward | Machine-checked proofs of `caps`, then IPC, then MMU isolation | **the verification itself.** **Built:** `caps`, IPC (rendezvous + one-shot Reply), and the MMU isolation invariants are all proved |
| 14 | Kernel objects from untyped: remove the kernel heap | Retype TCBs, endpoints, page tables; delete the kernel heap | **critical path:** a verifiable kernel cannot allocate. **Built:** the kernel has no allocator; see design/kernel-objects-from-untyped.md |
| 15 | Tagged address spaces (ASIDs) | 16-bit ASIDs, generation/rollover; stop flushing the whole EL1 TLB per switch | perf the real-workload path needs on real silicon. **Built** (8-bit fixed bitmap, no rollover: milestone 14's bounds made generations unnecessary; notes/asids.md) |
| 21 | Performance measurement: benchmarks with teeth | icount microbenchmarks + committed baseline that fails on regression; HVF-native runs for real magnitudes | perf claims become measurements; regressions surface next to their cause. **Built**; notes/benchmarks.md |
| 16 | Real hardware + SMMU-backed driver isolation | Port to an IOMMU-backed machine; confine driver DMA in silicon | isolation in hardware, under real workloads |
| 19 | Run a real workload | A native-ABI workload first; Linux-compat or VM hosting later | **the "runs real workloads" half** of the thesis. **Built:** granular verbs and userspace init (19d), init as the real boot path (19d.2c), dedicated binaries delivered as a crickerfs archive with a shared `user_rt` runtime (19f.1-6), the native ABI written down (19e/Decision 2, notes/abi.md, DECISIONS §15), and the first real workload, a CoreMark-derived compute program spawned against that ABI (19e). design/init-and-granular-spawn.md |
| 17 | Multikernel-leaning scheduler (research, optional) | Partition the shared thread table and endpoints | optional; not on the thesis path |
| 20 | A portable HAL, proven on a second architecture | Make `arch/` a real HAL; bring up RISC-V then x86_64 | the "portable verified core" claim; reach the demonstrator earns |
| 24 | A second aarch64 *board*: Virtualization.framework (optional) | Boot under Apple's Virtualization.framework, not QEMU's `virt`: a virtio-console driver (VZ has no PL011), VZ's interrupt/memory layout and boot handoff, device discovery through the machine VZ presents | proves the `arch/` **board** boundary on a second machine of the *same* ISA (cheaper than 16's silicon, distinct from 20's second ISA), and lets cricker-os run under the same VMM as macOS/Linux guests. Optional; portability exercise, **not** a benchmarking prerequisite (guest-internal microbenchmarks are VMM-independent) |
| 25 | Cross-OS performance comparison (extends 21) | EL0-measured primitive benchmarks (syscall, context switch, IPC, map, spawn) the lmbench way, so the numbers include the trap the kernel-side benchmarks skip; then line them up against lmbench (Linux, macOS guests) and `sel4bench` (seL4), at a matched virtualization tier, with release builds. Fold in the icount codegen-sensitivity fix. | **turns perf claims into cross-OS numbers**: where does a Rust capability microkernel stand next to Linux, macOS's XNU, and seL4 on the primitives that define an OS. **Started**: EL0 self-timing (19e), null-syscall EL0 (~43 ns HVF); context switch, IPC, and the host side remain. notes/benchmarks.md |
| 22 | Trusted init: verify it, and shrink what a broken one can do | Measured/secure boot that checks init before running it; reduce init's authority so a compromise is bounded | **closes the thesis's own soft spot:** init is the privileged *unverified* component the whole system is built by |
| 23 | A capability-routed component OS with live replacement | Every userspace component (driver, server, app) is a swappable, vendor-shippable unit behind a stable contract; operators replace them live, no reboot. The console hot-swap is instance one; a durable queue-broker decouples component lifecycles (opt-in per channel, for latency) | **the flagship payoff and a product ambition:** competing vendor components, confined by the kernel and swapped live; the verified core is the one fixed thing |

The order §14 sets: **verify the core and make it verifiable first** (18 and 14, the thesis), then the
road to running real workloads on real machines (15, 21, 16, 19; 25 extends 21 into cross-OS
comparison), with the multikernel work (17) as
optional research, the second-architecture port (20) as the reach the demonstrator earns, and the
second-*board* port (24, Virtualization.framework) as an optional same-ISA portability exercise, all
late and only after the core is proven. **Trusted init (22) follows 19**, because it only has teeth once there
*is* an init to verify and once real hardware (16) closes the in-RAM tampering window. **The capability-routed
component OS (23) is the late destination**: the console hot-swap is instance one, built on
revocation (13/the CDT), supervision (22), and dedicated binaries (19f); the general version (a
component contract, state handoff, vendor confinement) is a product ambition the demonstrator
earns, and it re-touches the parked competitor story below. The broad competitor ambition stays parked (see the
end of this file).
Several milestones already have their design worked out; the blocks below point at it.

### 12. Call/Reply IPC: a one-shot reply capability

**Built (milestone 12); see DECISIONS §12 and notes/ipc-naming.md.** The rest of this block is the
proposal it was built from.

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

**Built (milestone 13), scoped to frame revocation; see DECISIONS §13.** The full derivation tree is
deferred, the way the argument earlier in this file predicted: revoke-all-derivatives serves the
reclamation triggers, and subtree granularity waits for a driver. The rest of this block is the
proposal it was built from.

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

**On the critical path (§14).** The gate this used to sit behind ("is verifiability actually the
goal?") is resolved: it is. So this is no longer an optional purity win. A verifiable kernel cannot
allocate dynamically, so removing the heap is a prerequisite for verifying the kernel at scale rather
than only its pure-logic crates. It still also buys the smaller payoff on its own terms: the
kernel-heap-exhaustion class disappears entirely.

### 21. Performance measurement: benchmarks with teeth

**Added 2026-07-23, prompted by milestone 15 shipping a performance win nothing measures.** The
requirement, stated by Chris: identify performance issues, and identify the *introduction* of
performance problems proximate to the changes that introduce them.

**Deliverable.** In-kernel microbenchmarks over the paths a microkernel lives on (IPC round-trip,
call/reply, context switch, spawn-to-reap, untyped map, null syscall), run under QEMU `-icount`
so virtual time is a deterministic function of instructions executed; a `script/bench` entry
point separate from `script/test`; and a **committed baseline** that `script/bench --check`
diffs against, failing loudly on regression. Updating the baseline is a deliberate act in the
same commit that changes performance, so the baseline file's git history *is* the performance
record, each delta next to its cause.

**Two instruments, because one cannot do both jobs.**

1. **icount (TCG): the regression teeth.** Deterministic instruction counts, tight thresholds,
   the committed baseline, commit-gating. Catches path-length regressions (an extra lock, an
   accidental O(n), a flush creeping back). Models no caches and no TLB, so magnitudes are
   fiction; the counts are the point.
2. **HVF: the real magnitudes.** On this host (Apple Silicon), `-accel hvf` runs the kernel
   natively under Hypervisor.framework: real caches, real TLBs, `CNTVCT_EL0` at the hardware's
   24 MHz. `script/bench --real` reports medians over repeated runs with loose bounds, not
   gates: it is a real machine shared with a desktop OS, so the numbers are statistical.
   This is where milestone 15's flush removal finally gets measured (an A/B flag restoring the
   old `vmalle1is` quantifies it), and it is the aarch64-on-aarch64 coincidence paying off.

Known limits: device-touching paths carry virtualization overhead under HVF (MMIO traps to the
VMM), the PMU is not virtualized (cycle-exact counters wait for milestone 16's silicon, which
inherits this harness and swaps the clock), and the first thing to validate is that QEMU's
semihosting test-exit works under HVF at all; if not, the bench build reports over virtio
instead.

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

**Also closes an integrity window (milestone 22's precondition).** Before DMA is confined in
silicon, a malicious device can DMA over any RAM the kernel has not walled off, *including the
initrd holding init before the kernel has loaded and measured it*. Software confinement (the shadow
ring) governs a driver the kernel already trusts to run; it does nothing about a device corrupting
init's bytes at rest. So verifying init (22) is only airtight once 16 removes the way to tamper with
it underneath the check.

### 22. Trusted init: verify it, and shrink what a broken one can do

**The soft spot this closes.** §14 promises "a verified core that confines unverified workloads."
init is unverified, but it is not a *typical* workload: it holds the process-construction authority
and builds every other process. At runtime the kernel confines it as well as anything (MMU
isolation is proved, its code is W^X, capabilities are unforgeable), and a compromised init
**cannot break the kernel or escape confinement**. But its *bytes* are currently loaded unsigned and
unchecked, and its *authority* is broad, so within that authority a corrupted init can do real harm
(endow malicious children, deny the system it was meant to start).

**Deliverable, three halves.**

1. **Verify init before it runs.** A measured or secure boot step: the kernel (or a boot stage
   ahead of it) checks init's hash, or a signature over it, before dropping to EL0 at its entry.
   Today `spawn_init` loads whatever initrd it is handed. seL4's high-assurance deployments do
   exactly this for the root task; it is the single biggest gap between "verified kernel" and
   "trustworthy system."
2. **Shrink the blast radius.** Reduce what a compromised init can do: hand most
   process-construction to smaller, less-privileged sub-servers, so init's own authority is
   minimal and short-lived (build the first servers, then drop the untyped). The less init holds,
   the less a broken init costs.
3. **Supervise, don't relaunch-in-kernel.** What happens when init (or any server) *fails*, as
   distinct from being corrupted. The failure of init degrades to a **halt, never a breach**
   (the kernel's guarantees hold regardless), so the only open question is availability: halt, or
   recover? The answer is neither a bare halt nor a kernel that relaunches init.

   - **Not kernel-relaunch.** Relaunching init from the kernel re-imports the loader we just
     evicted (milestone 19) plus *restart policy* (retries, backoff, escalation) into the trusted
     core, and it crash-loops on a deterministic fault (init panics on a bad ELF; relaunch hits
     the same bug). Restart is policy, and policy does not belong in the kernel.
   - **The mechanism/policy split, as everywhere else.** Add one small *mechanism* to the kernel:
     a **fault/death notification** — when a thread faults or exits, the kernel delivers a message
     to an endpoint held by whoever holds the capability to supervise it. Capability-gated (you
     can supervise a thread only if you were granted its fault endpoint), mechanism-only. This is
     seL4's fault endpoint.
   - **Policy lives in a userspace supervision tree.** init builds the system, wires supervisors,
     and either becomes a *minimal* root supervisor (so small it essentially cannot fail) or steps
     back. A sub-server that dies is restarted by *its* supervisor with whatever policy it wants
     (bounded retries, fall-back, give-up), in userspace. Failures below the root are contained
     and restartable; only the death of the irreducible root supervisor halts, which is the
     fail-closed floor, pushed as high and as small as possible.
   - **This also dissolves the SPOF.** init-during-boot stays a single point of failure (if it
     cannot build the system, halt is correct — nothing to recover to). init-*after*-boot stops
     being one: it is either a trivial root or gone, and failures below it are supervised.

   The one kernel primitive this adds (the fault endpoint) is worth its own numbered decision when
   19d.2/22 make it concrete; recorded here so the design (halt is the floor, supervision is the
   answer, the kernel never runs restart policy) is on the record rather than in a conversation.

**The reach tail.** Beyond verifying init's *bytes*, verifying init's *behaviour* is the natural
next layer inward for the §14 thesis: init is small and privileged enough to be worth proving, once
the kernel's proofs are done. Recorded as the direction, not committed. (Distinct from supervision
above: proof buys *safety*, supervision buys *availability*; init's failure mode is availability, so
supervision is the load-bearing answer and proof is the optional reach.)

**Prior art.** seL4 + a verified boot chain (measured boot, or CapDL-driven system initialisers
whose output is checkable); the general secure/measured-boot literature (TPM/PCR measurement,
signed boot images). For the supervision half: seL4 fault endpoints (the kernel turns a fault into
a message a supervisor holds); MINIX 3's reincarnation server (a userspace process that restarts
dead drivers, not the kernel); Erlang/OTP supervision trees and "let it crash" (decades of evidence
that restart policy wants to be a rich userspace thing, not a kernel reflex).

### 23. A capability-routed component OS with live replacement

**The destination the design points at, and a product ambition.** A client names an *endpoint*,
never a peer (the milestone 7-8 decision), so a component's identity is invisible to the code that
uses it: any program that speaks the protocol and holds the right capabilities *is* the component.
That decoupling is what makes running components replaceable at all, and it generalizes: the aim is
a system where **every userspace component (driver, server, app) is a swappable, vendor-shippable
unit behind a stable contract, and operators replace them live, no reboot** -- with the verified
kernel as the one fixed thing underneath an entirely swappable userland. This is Fuchsia's shape
(capability-routed components, stable protocol interfaces) on a verified core.

**Instance one: hot-swap the console server (the mechanism).** Replace a running server with a new
version, no reboot, with a client that never notices. Four steps, each on earlier machinery:

1. **Start the new server** (a supervisor builds it via the granular verbs, endows it fresh).
2. **Revoke the old server's device capability** so there are never two owners of one device's
   registers (the interleaving hazard): milestone 13's revocation extended from frames to *device*
   capabilities, where the deferred CDT (capability-derivation tree) finally earns its keep.
3. **Redirect clients through a broker.** Clients hold a cap to a stable *broker* endpoint, not to
   the server; the broker re-points on a swap, so substitution is invisible. A userspace naming
   service.
4. **Drain in-flight requests and tear the old server down** (the reaper plus revocation).

**The broker as a queue, and its latency (the concern that governs where this is used).** The
instance-one broker just re-points; the general form *buffers* -- a **durable queue server** that
holds messages in its own budget while a backend is down (crashed, restarting under supervision, or
being swapped), so a producer never blocks on an absent consumer and the new consumer drains the
backlog. This is the OS analogue of a distributed message queue (Kafka/RabbitMQ): a stable, always-up
broker decouples the *lifecycles* of the two ends, which is what makes crash-restart and live swap
seamless rather than merely possible. The kernel does not change -- it keeps synchronous rendezvous
(tiny, verified, no allocation); the queue is userspace policy, its buffer bounded by the server's
own untyped, so a runaway producer hits backpressure or a drop policy, never unbounded kernel memory.

Latency is the price, and it dictates where the queue is wired. Interposing a queue server turns one
rendezvous (one IPC, one switch, register transfer) into **two IPCs, two switches, and a copy**
through the server's buffer -- roughly a 2x IPC tax plus a scheduling hop. On a microkernel where
IPC is the hot path, that is not paid everywhere:

- **Opt-in per channel, never the default.** Direct synchronous rendezvous stays the fast path;
  queuing is chosen only for channels that cross a lifecycle boundary (components that restart or
  swap), where the decoupling is worth the tax.
- **Pass-through when both ends are up.** The broker buffers only during the down window; in steady
  state, with a live consumer waiting, it forwards directly, keeping the common case near direct IPC.
- **A latency ladder, not one point.** Fastest: a shared-memory ring buffer + async notification
  (the io_uring / virtio shape cricker-os *already runs* for device I/O; the notification primitive
  is a generalisation of the endpoint's async-signal count) -- no middleman process, decouples in
  rate. Middle: a queue-server process -- decouples lifecycle, one extra hop. Slowest: a durable
  queue server that writes to storage -- survives its own crash. The rung is a per-channel choice.
- **Measure it, do not argue it.** Milestone 21's benchmark harness is the instrument: add a
  queued-IPC round trip beside the direct one, so the tax is a committed baseline number and a
  regression in it surfaces proximate to its cause.

Prior art for the queue itself: Mach ports (kernel message queues, macOS's foundation), Unix pipes,
POSIX/SysV message queues, and every distributed broker (Kafka, RabbitMQ, SQS); the shared-memory
ring variant is io_uring, DPDK, and virtio.

**Generalising to all components: what the console case does not yet need.**

- **A uniform component contract + manifest.** Each component implements a stable protocol and
  *declares the capabilities it needs* (this device, these endpoints), so any vendor's build is a
  drop-in the supervisor wires from the manifest. This is seL4 CapDL / Fuchsia component-manifest
  territory.
- **State handoff (the crux).** The console is easy because it is near-stateless. A filesystem
  server (open handles, caches, in-flight writes) or a network stack (live connections) cannot be
  kill-and-restarted without losing state; live-swapping them needs a serialise-old / absorb-new
  protocol over a supervisor-brokered channel. Prior art: Erlang/OTP `code_change`, VM live
  migration, CRIU checkpoint/restore. This is where the real engineering is.
- **Dependency-aware orchestration.** If B is a client of A, swapping A means quiesce B, swap,
  resume; the supervisor (22) needs the dependency graph and a quiescence protocol.

**The fixed core, stated honestly.** Two things are deliberately *not* hot-swapped this way, and
that boundary is a feature. The **kernel** is the verified TCB enforcing everything; you do not
live-swap it (changing it is a reboot; seamless kernel update is a separate, heavier problem). A
**minimal init / root supervisor / broker** is the fixed point that makes swapping everything else
possible -- pushed as tiny and stable as it can be, but you cannot swap the swapper infinitely.

**Why this is the selling point, and safe.** Because the kernel confines every component to exactly
the capabilities it was granted, **untrusted, competing vendor components run safely**: a Linux
vendor kernel module is ring-0 and can do anything; a cricker-os vendor component is a confined
process that can touch only what the operator handed it. A malicious console driver scribbles on the
UART it was given and nothing else -- it cannot read another component's memory, forge authority, or
reach the kernel. That is what makes "different vendors ship competing components, operators swap
them live" not merely possible but *safe*, and it is the payoff of the capability model plus
milestone 22's authority-minimisation. It also connects directly to the parked competitor ambition
at the end of this file: this component model *is* a general-purpose product story, on the verified
core the demonstrator earns first.

**Prior art.** Fuchsia (the closest match: capability-routed, manifest-declared, swappable
components); MINIX 3's reincarnation server (live driver replacement in userspace); QNX
(hot-swappable drivers); Erlang/OTP hot code loading and supervision. The common thread is ours:
components are isolated processes, named through indirection and confined by capability, so one can
be swapped under the others.

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

### 18. Verify the capability core, then spread inward

**Green-lit and started; see DECISIONS §14 and notes/verification.md.** This is the verification
thesis as an actual work item rather than an aspiration.

**Deliverable.** Machine-checked proofs (Kani) of the security-critical logic, spreading inward from
the capability core. `crates/caps` is proved already: five harnesses covering "`derive` never widens
rights," "userspace cannot forge a right," and the subset order's reflexivity and transitivity, each
for *every* input rather than sampled cases (`script/verify`). Next, in order, IPC (the rendezvous
and the one-shot reply) and the MMU isolation invariants.

**Why here.** It is the differentiator (§14), and it is cheap to start: the §7 pure-logic crates
already compile for the host, and proofs live behind `#[cfg(kani)]` so they never touch an ordinary
build. It also interlocks with 14: proving properties *of the kernel* (not just its logic crates) at
scale wants a kernel that does not allocate.

**Prior art.** seL4 (Isabelle/HOL refinement, verified C) is the mountain; we took the tractable path
(bounded model checking, Rust). Verus is the deeper Rust option to revisit if a property needs
unbounded proof.

### 19. Run a real workload

**Deliverable.** The "runs real workloads" half of §14: a real, unverified program running in
confined userspace on the verified core. A **native-ABI** workload first (the leanest thing that
proves the point), with a Linux-compat personality or VM hosting as later, larger options.

**Why.** The thesis is not "a verified kernel" but "a verified kernel *that runs real workloads*."
This is the milestone that makes the second half true, and it is what a demonstrator ultimately shows.

**The sub-decision it carries.** What counts as the first "real workload," and by which ABI. Native
first keeps the kernel pure and the surface small. A Linux-compat personality (Starnix / gVisor /
WSL1 shape, a userspace server translating syscalls) is how a demonstrator eventually reaches
existing software, and it is where the parked competitor ambition would begin. VM hosting (seL4's
route) needs the EL2 work in design/driver-domains.md. Decide the first target before writing
compat code, so it stays scoped.

### 20. A portable HAL, proven on a second architecture

**Reach the demonstrator earns (§14), with a thesis-relevant core.** A second ISA is reach work, and
§14 parks reach. What pulls part of it back in-scope is one demonstrator claim: **the verified
capability core is architecture-independent**, the same machine-checked confinement running S/U on
RISC-V, ring-3 on x86, and EL0 on ARM. seL4 (verified on both ARM and RISC-V) is the precedent.

**Deliverable, in two parts.**

1. **Make `arch/` a real HAL.** Today it is a `#[cfg(target_arch)]` re-export whose contract is
   "fails to compile if something is missing." Turn it into a genuine machine-dependent layer: split
   the aarch64 descriptor format out of the `paging` crate (a generic level-walk plus a per-arch entry
   codec, the way Linux folds page-table levels), put device discovery behind a "here is the hardware"
   interface (device tree today, ACPI/PCI later), and make the arch surface explicit. This is the
   reusable half and most of the value; a second ISA is what proves the split is honest. The
   seam-*naming* subset that needs no second architecture is broken out as **20a** and can start now;
   the abstraction *shapes* (the codec and discovery interfaces) wait for RISC-V, because deriving
   them from one ISA is the wrong-abstraction trap DECISIONS warns against.
2. **Bring up a second ISA, then a third: RISC-V first, x86_64 second.**

**Why RISC-V first.** It is structurally close to aarch64, so it reuses the most and needs the
smallest new `arch/` subtree: device tree and virtio-mmio port unchanged, the weak-memory discipline
keeps paying off (RVWMO, like ARM), and Sv39/Sv48 is the same MMU shape. What is new is small and
clean (SBI boot, one trap vector, PLIC/CLINT, `ecall`), with no GDT/TSS, ACPI, PCI, or real-mode SMP
trampoline. It de-risks the HAL split cheaply and stays in the verification ecosystem (a formal Sail
ISA spec, seL4's verified RISC-V port).

**Why x86_64 second.** The hard proof: the HAL must survive a genuinely different model (CISC, strong
TSO memory, GDT/TSS, ACPI + PCI, port I/O, the `syscall` + swapgs trampoline, INIT-SIPI-SIPI SMP). If
the abstraction survives x86, it is real rather than an accident of two similar RISC ISAs. It is also
the reach: x86_64 is what most machines are. The file-by-file map is worked out (see the chat where
this milestone was proposed).

**Scope and the honest cost.** In scope: the HAL, and enough of each ISA to boot, confine a ring-3/U
process, and run the test suite. Out of scope and still parked: hardware breadth (every driver on
every board). It buys no proof coverage, the proofs live in the machine-independent crates, which
already do not care about the ISA, and it enlarges the unverified TCB (one hand-written
boot/MMU/trap/syscall layer per arch, the least-verifiable code). That is why it sits late, after the
core is verified (18, 14) and a workload runs (19). Not a new architecture: real-hardware aarch64
(Raspberry Pi) is the cheapest portability proof of all, same ISA on real silicon, and it lives in
milestone 16, not here.

**Prior art.** notes/portability.md: Linux's `arch/` with folded page-table levels, NetBSD's MI/MD
split, NT's HAL from day one. seL4's dual-arch verified port is the "portable verified core"
precedent.

### 20a. Name the seams (HAL-prep without the HAL)

**The part of milestone 20 that is safe to do before a second architecture exists, and can start any
time.** DECISIONS warns against speculatively trait-ifying subsystems, because you build the wrong
abstraction before the requirements are known. That is squarely this: the generic/arch boundary in
`paging`, a device-discovery interface, and any HAL trait can only be shaped once RISC-V shows where
aarch64 was accidentally load-bearing. So this step does the subset that needs no guessing. It
*names and isolates* the seams; it does not *abstract across* them.

**Deliverable.**

1. **A concrete arch-boundary audit.** Make notes/portability.md cricker-os-specific: the exact
   files (`arch/aarch64/*`), the crates that are secretly machine-dependent (`paging` carries the
   aarch64 descriptor format; `dtb` is the device-tree discovery path), and the driver assumptions
   (`pl011`, `gic`, virtio-mmio are MMIO; semihosting is the test-exit). This is the map milestone 20
   executes against, and it is useful on its own as "what a port actually touches."
2. **The arch contract, written down.** `arch/mod.rs` enforces its surface only by failing to
   compile. Document the required surface as a doc comment: the functions and types every arch module
   must provide. A list, not a trait, naming the seam without shaping the abstraction across it.
3. **Isolate the aarch64 format inside `paging`.** Group the descriptor-bit encoding and the `Flags`
   constructors into one clearly-labeled module ("this is the aarch64 format; a second arch replaces
   this file"), beside the table/index/walk code. One crate, one arch, no generic interface yet: a
   clean, visible line for the eventual split, not the split itself.

**Explicitly deferred to arch #2 (RISC-V):** the generic-level-walk / per-arch-codec interface, a
device-discovery abstraction, and any HAL trait. Each needs the second implementation to avoid
encoding aarch64's accidents as "generic."

**Worth it now?** Modestly, and honestly. It is mostly documentation plus one clarity refactor, so it
will not feel like much. What it buys: the port map is written down, the arch surface is explicit
rather than discovered by compile error, and the `paging` split becomes mechanical when RISC-V lands.
It also makes the aarch64 code clearer today, which is its own small return even if no port ever
happens.

## One decision this roadmap still forces

§14 resolved the verification-endgame fork (verification *is* the goal) and converted the old "POSIX
posture" question into milestone 19's real-workload sub-decision (reach binds now that "real
workloads" is committed). What remains open:

- **When the demonstrator becomes a competitor, if ever.** §14 keeps a general-purpose competitor as
  an explicit *later optionality*, parked until the demonstrator earns it. The trigger to reopen it is
  concrete: a verified core that actually runs a real workload (milestone 19), plus a reason the
  world needs another OS that the demonstrator has by then proved. Until both hold, competitor-shaped
  work (broad driver coverage, a full Linux ABI, a package ecosystem) is out of scope, and saying so
  keeps the demonstrator from sliding into a second, unfinished Linux.

## The rival worth understanding, not building

eBPF is the strongest competing answer to the question this whole architecture asks: safe kernel
extension through *verification* rather than *isolation*, with no IPC cost. Worth reading as the
other fork. It does not undercut the thesis so much as relocate the cost: the eBPF verifier is itself
a large, subtle, repeatedly-CVE'd component, so "the verifier is the TCB" is its version of the
problem, not an escape from it. No milestone; a reading item.

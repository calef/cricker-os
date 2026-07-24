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
| 15 | Tagged address spaces (ASIDs) | 16-bit ASIDs, generation/rollover; stop flushing the whole EL1 TLB per switch | perf the real-workload path needs on real silicon |
| 16 | Real hardware + SMMU-backed driver isolation | Port to an IOMMU-backed machine; confine driver DMA in silicon | isolation in hardware, under real workloads |
| 19 | Run a real workload | A native-ABI workload first; Linux-compat or VM hosting later | **the "runs real workloads" half** of the thesis |
| 17 | Multikernel-leaning scheduler (research, optional) | Partition the shared thread table and endpoints | optional; not on the thesis path |
| 20 | A portable HAL, proven on a second architecture | Make `arch/` a real HAL; bring up RISC-V then x86_64 | the "portable verified core" claim; reach the demonstrator earns |

The order §14 sets: **verify the core and make it verifiable first** (18 and 14, the thesis), then the
road to running real workloads on real machines (15, 16, 19), with the multikernel work (17) as
optional research and the second-architecture port (20) as the reach the demonstrator earns, late and
only after the core is proven. The broad competitor ambition stays parked (see the end of this file).
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

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
pure-logic §7 crates. The `capability` model is proved already (`script/verify`, notes/verification.md);
IPC and the MMU invariants are next. This threads through the list rather than being one milestone.

## The milestones

**Status vocabulary.** The `Status` column is the *one* canonical place a milestone's state lives, and
it uses exactly these tokens. This exists because status used to be prose scattered through the detail
blocks, phrased a dozen different ways ("Built 2026-07-28", "DONE", "Phase 1 built", "Largely done"),
which is unreadable at a glance and, worse, unparseable: a status sweep on 2026-07-30 mis-reported eight
milestones. `script/roadmap` validates this column and fails on anything outside the vocabulary.

| Token | Means |
|---|---|
| `BUILT` | Complete, and proven by the gate on every supported ISA. |
| `PARTIAL` | Some phases shipped and more remains, with nobody currently on it. The block says which phases. |
| `IN-PROGRESS` | Active work on a branch right now. |
| `NOT-STARTED` | Specified, nothing built. |
| `OPTIONAL` | Deliberately off the thesis path; not a backlog item. |
| `RECORDED` | Analysis captured and the decision deliberately *not* taken. |

A detail block may narrate its state in prose (that is where the evidence and the dates belong), but the
column is what answers "where do we stand". If the two disagree, the column is wrong and the block is
right, because the block is where the work was written down; fix the column.

**Effort, calibrated from git history (2026-07-30), not guessed.** Blocks below give effort in
**lanes**: one lane is one agent session end to end. Measured across the fourteen milestone branches
merged so far, a lane is **31 to 57 minutes of wall clock, 1 to 9 commits, 7 to 30 files, and 694 to
4,351 inserted lines**: a much narrower band than the work's apparent ambition suggests, since
proving the DMA boundary, building a compositor, and confining a C component all cost about the same.
Milestones that took more than one lane took them as *phases that landed separately* (27 and 30 took
three each, 22, 29, 31 and 35 two each), not as one long push.

This replaces an S/M/L scale that was written before any of it was built and was systematically
pessimistic where it can now be checked: **27, 29, 30 and 32 were each labelled "Effort L"**, and each
came in at roughly a lane per phase. Anything still labelled by feel rather than by history says so.
Re-derive with `git log --first-parent` over the merge commits rather than trusting these numbers as
they age.

**Why this is a markdown table and not GitHub Issues (decided 2026-07-30, Chris).** Issues would buy PR
linkage, a home for discussion, and a board view. They would cost the things this table is actually for:
the roadmap stops being version-controlled alongside the code, so a status change is no longer a diff in
the commit that caused it; `script/roadmap --check` has nothing to validate, and that gate is what caught
milestone 34 having no row; and the cross-references to `DECISIONS §N` and `notes/*.md` decay from one
grep into URLs. The deciding argument is that **a second place where status lives is a second source of
truth**, which is the exact failure this project spent 2026-07-30 cleaning up: `bench.rs` contradicting
notes/benchmarks.md about what `fs_read` measures, status prose phrased a dozen ways that made a sweep
mis-report eight milestones, and §27 corrected four times. The linkage only starts paying when more than
one person files work, so revisit if that changes: with external contributors, the shape would be
markdown canonical and issues **generated one-way** from it, never synced back.

Note also that GitHub's own *Milestones* feature is a name collision with this list and a poor fit
besides, being built for dated release grouping; these are capability-shaped and deliberately undated.

| #  | Status | Milestone | Why it matters (§14) |
|----|--------|-----------|----------------------|
| 12 | BUILT | Call/Reply IPC: a one-shot reply capability | the IPC the TCB must get right |
| 13 | BUILT | Capability revocation + untyped reclamation | safe teardown, a TCB property |
| 26 | BUILT | Object revocation: tear a process back down | the teardown half of "run real workloads": a process can be reaped, not just built |
| 18 | BUILT | Verify the capability core, then spread inward | the verification itself |
| 14 | BUILT | Kernel objects from untyped: remove the kernel heap | removes the kernel heap: the prerequisite for "small enough to verify" |
| 15 | BUILT | Tagged address spaces (ASIDs) | a context switch stops flushing every translation |
| 21 | BUILT | Performance measurement: benchmarks with teeth | perf claims become measurements, and regressions surface next to their cause |
| 16 | PARTIAL | Real hardware + IOMMU-backed driver isolation, **RISC-V first** | isolation in hardware, under real workloads |
| 19 | BUILT | Run a real workload | the "runs real workloads" half of the thesis |
| 17 | OPTIONAL | Multikernel-leaning scheduler (research, optional) | optional; not on the thesis path |
| 20 | BUILT | A portable HAL, proven on a second architecture | the "portable verified core" claim |
| 24 | OPTIONAL | A second aarch64 *board*: Virtualization.framework (optional) | proves the `arch/` **board** boundary on a second machine of the same ISA; optional |
| 27 | BUILT | Rust `std` on the native ABI | widens "runs real workloads" by orders of magnitude |
| 28 | BUILT | A solid terminal: the line discipline as a component | a terminal with real behaviour, which 27's stdio semantics need |
| 29 | BUILT | A display terminal (framebuffer, virtio-gpu) | the first pixels the demonstrator ever puts on a screen, and then the first letters |
| 30 | BUILT | The network stack as a confined component | the canonical microkernel component, and the one people ask about first |
| 31 | PARTIAL | A capability shell: designation is authorization | no-ambient-authority made user-visible, at the one interface a human touches |
| 32 | BUILT | A real filesystem: RedoxFS behind a capability FS server | the flagship userspace-reuse story: a real filesystem we did not write, confined |
| 33 | BUILT | A compositor: one screen, mutually distrusting clients | the canonical multiplexer of one device among mutually distrusting clients |
| 34 | NOT-STARTED | GPU acceleration via virtio-gpu 3D (the display ladder's rung four) | how every VM gets a GPU without a hardware driver |
| 25 | PARTIAL | Cross-OS performance comparison (extends 21) | turns perf claims into cross-OS numbers |
| 22 | PARTIAL | Trusted init: verify it, and shrink what a broken one can do | closes the thesis's own soft spot: init is the privileged *unverified* component |
| 23 | PARTIAL | A capability-routed component OS with live replacement | the flagship payoff, and a product ambition |
| 35 | BUILT | Prove the DMA-confinement boundary (extends 18) | closes the one isolation boundary we test instead of prove |
| 36 | BUILT | A foreign-language component, seam first (spike; feeds 29 and 23) | the thesis in one assertion: unverified foreign code, confined and restarted |
| 37 | BUILT | Prove RedoxFS's crash consistency (DECISIONS §34, condition 1) | decides whether §34's "primary filesystem" label is earned |
| 38 | NOT-STARTED | Filesystem throughput, and the comparison (DECISIONS §34, condition 2; extends 21/25) | "primary filesystem" invites a comparison we cannot currently make |
| 39 | RECORDED | Repository structure for a loosely-coupled OS, and the road to a distribution | the structure has to serve the thesis, and one constraint dominates |
| 40 | NOT-STARTED | Documentation as a system service: searchable, rendered, and installed by packages | the OS explains itself, on itself |
| 41 | BUILT | Dead code: triage the suppressions, and un-blindfold the gate | a `-D warnings` gate with holes in a third of the kernel is not a gate |
| 42 | BUILT | Supply chain and fuzzing in CI (extends the 2026-07-30 CI audit) | we confine code we did not write, and the parsers that read what firmware and disks hand us are where a bound is a lie |
| 43 | NOT-STARTED | A second security audit, with a different lens | the attack surface roughly doubled after the first audit was written |
| 44 | PARTIAL | GitHub repository hardening: policy, private reporting, code scanning, pull requests | a repository with a security thesis should be able to receive a report privately |
| 45 | BUILT | Triage the CodeQL code-scanning alerts, and decide what the tool is for | the alerts land on this project's most-used unsafe abstraction |
| 46 | BUILT | Rename the components for what they are, and write down the naming rules | a name is a claim, and `-d` claims something we rejected; conventions that matter get a checker, not a paragraph |
| 47 | IN-PROGRESS | Navigation and naming: cd, pwd, ls, mkdir, rm, paths, and environment | **divergence from Unix must be earned, never stylistic.** Keep the commands; change only what the capability model actually forces, and get one missing primitive right |
| 48 | NOT-STARTED | Job control: jobs, wait, kill, fg, bg, and a stopped state | **most of it needs no new kernel surface**, and the tty's most tangled feature turns out to be a capability transfer |
| 49 | NOT-STARTED | Users, login, and attribution: what identity is for once it stops being authority | three of Unix's four uses for a uid are already answered structurally; the fourth, **attribution, has no mechanism at all** |
| 50 | PARTIAL | Pipes and redirection: one sink protocol, and `\|` turns out to be an endpoint | the sink contract is **built** (`crates/sink_proto`, notes/sink-protocol.md) and a program is proven indifferent to what its output slot holds; `>`, `<`, `\|` and stdin remain |
| 51 | PARTIAL | Wall-clock time, the `date` command, and an NTP service | the machine knows what time it is: two RTC drivers, the clock service (§43), `crates/calendar`, `crates/ntp_proto`, `date`, and an NTP client holding **propose and not set**. `date` is **not yet reachable from the shell** |
| 52 | RECORDED | Subshells without `fork`, and what copying an endowment means | `( ... )` is fork, we deliberately have no fork, and **capability duplication is not a total function** |
| 53 | NOT-STARTED | The board's own peripherals: network and storage on real silicon | 16a boots the board; this is what makes it able to *do* anything, and it is where virtio stops carrying us |
| 54 | NOT-STARTED | A network file service a Mac can actually mount | the first real workload with a real user, and the security claim backup servers deserve |
| 55 | NOT-STARTED | Time Machine: SMB3 with Apple's extensions, and mDNS | **likely the largest single piece of work in the project**, and the one that must be scoped before it is started |
| 56 | BUILT | Secrets, credentials, and the entropy to make them safe | **built 2026-08-01**: entropy (§44), the Argon2id crypto taken as a dependency per §46, and the credentialer, a store with no getter that verifies and never reads back (§54). The thesis-level gap it named, that *a secret is still a bearer token where a capability is an unforgeable reference*, is **milestone 65's** subject: hold the key, expose the operation |
| 57 | PARTIAL | Partitioning and formatting a real drive, and extended attributes | you cannot find a partition without reading the table, and **we have no partition-table code at all**; all of it is testable in QEMU before the board lands. The host recovery tool (`ls`/`cat`/`extract`), `crates/gpt` and the **extended-attribute layer** are built; on-target `mkfs` and block-device enumeration are not |
| 58 | NOT-STARTED | RISC-V TLB shootdown, and the flush that makes ASIDs pointless | every riscv context switch discards the whole TLB; the fix needs a **software** shootdown protocol, because `sfence.vma` does not broadcast |
| 59 | BUILT | The CPU-model matrix: stop testing against one generous emulator | `-cpu rv64` enables nearly every ratified extension; the board is an RV64GC U74. `script/cpu-matrix` runs the riscv64 suite across five models and all 211 tests pass on every one, so we are already portable to the board's ISA. The ASID test written *for* the board is the gap no model can exercise |
| 60 | NOT-STARTED | ISA discovery: read the machine instead of assuming it | nothing reads `riscv,isa-extensions`; RISC-V has no `CPUID`, so the device tree plus targeted probes are the architected answer. One `Isa` record, built at boot, printed at boot |
| 61 | BUILT | The caretakers: one verb table, and names that say what you get | **built, both ISAs.** The rename landed first (532 tokens, not four filenames); `fs_proto::verb` is one row per opcode and a verb with no row is a compile error; all three caretakers forward the four extended-attribute verbs, proven by three witnesses each with a control that must fail |
| 62 | NOT-STARTED | Tests that assert on time: make a red run mean something | ~19 bounded spins (`for _ in 0..N { yield_now() }`) and wall-clock assertions flake under load. Four separate lanes and the integrator hit them on 2026-08-01; the CPU matrix multiplies the exposure fivefold |
| 63 | BUILT | Directory and package names: one spelling per thing | **built, both ISAs.** Eight crates, fourteen programs and modules, and the three violating directories renamed to the spellings settled in review; `fs-server` is `fs_server`, `user-std`/`hellostd` is `std_exerciser` twice, and the shell has a name (`swish`). The tables below keep the old spellings on purpose, because they are the record of the decision |
| 64 | NOT-STARTED | Enough `std` to run somebody else's crate | milestone 27 shipped the PAL; `fs` answers `Unsupported` in 32 of 54 functions and `thread` in 4 of 6. Measured against real crates.io dependencies rather than guessed at, because the gap that matters is the one a chosen crate actually hits |
| 65 | NOT-STARTED | A secrets service: hold the key, expose the operation, never the key | NTLMv2 does not verify a presented secret, it **computes with a key**, so §54's verifier shape does not fit it. Generalises the credentialer into a software HSM. Blocks milestone 55 |
| 66 | NOT-STARTED | Vaultwarden: somebody else's real application, running here | the north star for "runs real workloads". Names the gaps concretely rather than aspirationally: no TCP **listen or accept** in the socket contract, threads mostly stubs, most of `std::fs` unsupported, no async runtime, no TLS, and SQLite is a C library. Largest single item on this roadmap |
| 67 | NOT-STARTED | `swish` the language: quoting, sequencing, and exit status | `swish` is an interactive shell without control flow. Quoting is the one that is a correctness gap rather than a convenience: **a filename with a space is currently unnameable** |
| 68 | PARTIAL | Code-quality gates: one lint policy, and the lints that lost | Import order, `[workspace.lints]`, dependency direction, unused dependencies, spelling. Three lints were adopted, measured and **removed** on the evidence. `undocumented_unsafe_blocks` is now a GATE: all 205 undocumented blocks were read and commented. Doc examples went 5 -> 23 across nine crates, which is a start and not the standard; `missing_docs` is still not adoptable |
| 69 | BUILT | Split `kernel/src/user.rs` by service | 15,499 lines and **46 top-level modules** in one file: a dozen `*_service` modules and ~34 test modules. The split is nearly free because the boundaries are already `mod` blocks, so moving one to its own file changes no visibility and no API |
| 70 | BUILT | `swish`'s remaining logic in a crate, host-testable like its siblings | `coremark`, `line_editor` and `compositor` are each a crate holding the logic plus a program holding the IO. `swish` is the largest program that is not, so its dispatch, endowment preview and outcome handling are reachable only through QEMU |
| 71 | BUILT | The thread-start fault: a user thread dispatched with `sepc` = 0 | Frame placement, as this entry guessed. RISC-V put the frame 16 bytes under where `trap.s` builds an S-mode frame, so any interrupt in the window rewrote it and the user `sp` read the trap frame's hardwired-zero slot. Reproduced deterministically by widening the window; fixed by placing the frame at the stack top on both ISAs |

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

**The Prior-art sections below cover reuse too.** Before building, each milestone design answers
three questions against the ecosystems in notes/prior-art.md (Redox, rCore, Tock, Hubris, seL4,
Fuchsia): is there code to use, a design to copy, or a mistake to avoid? The build-vs-reuse call
gets recorded with its reason. The rule that decides it: the reuse boundary is the TCB boundary
(inside it, always build; userspace, actively prefer porting), and no reuse may widen the syscall
surface or smuggle in POSIX assumptions. notes/prior-art.md has the full argument.

### 12. Call/Reply IPC: a one-shot reply capability

**In brief.** Reply-to-caller as a kernel guarantee. **Built, §12.**

**Why it matters.** the IPC the TCB must get right

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

**In brief.** Unmap a page from every holder; reclaim a region safely. **Built (frame scope), §13.**

**Why it matters.** safe teardown, a TCB property

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

**In brief.** Retype TCBs, endpoints, page tables; delete the kernel heap

**Why it matters.** **critical path:** a verifiable kernel cannot allocate. **Built:** the kernel has no allocator; see design/kernel-objects-from-untyped.md

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

**In brief.** icount microbenchmarks + committed baseline that fails on regression; HVF-native runs for real magnitudes

**Why it matters.** perf claims become measurements; regressions surface next to their cause. **Built**; notes/benchmarks.md

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

**In brief.** 16-bit ASIDs, generation/rollover; stop flushing the whole EL1 TLB per switch

**Why it matters.** perf the real-workload path needs on real silicon. **Built** (8-bit fixed bitmap, no rollover: milestone 14's bounds made generations unnecessary; notes/asids.md)

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

### 16. Real hardware + IOMMU-backed driver isolation (recast 2026-07-27: RISC-V first)

**In brief.** **16a:** first silicon on a VisionFive 2-class board, whose firmware contract (OpenSBI, SBI HSM, NS16550, PLIC, Sv39) is exactly what the kernel already speaks. **16b:** IOMMU-backed DMA isolation against QEMU's emulation of the **ratified RISC-V IOMMU** (v1.0.1) first, over the §18 PCIe transport; silicon when a board ships it

**Why it matters.** isolation in hardware, under real workloads; the second ISA becomes the first silicon, and the IOMMU work stops waiting on a purchase

The milestone was always two things bundled, first silicon and DMA isolation in hardware, and
the recast splits them, because each is better served on the RISC-V side now.

**16a: first silicon, on a VisionFive 2-class RISC-V board.** The riscv port's firmware contract
on real boards is IDENTICAL to what the kernel runs today: OpenSBI, SBI HSM bring-up (the hart
lottery is already survived, on the record), NS16550, PLIC, Sv39. A ~$60-100 board boots the
exact contract we speak; the aarch64 side fits real boards worse (a Pi wants TF-A for PSCI, its
default is spin-table, and its IOMMU story is the weak spot notes/target-hardware.md already
flags). Deliverable: boot, UART, SMP, the test suite where semihosting allows, and the benches
on real cycles via the SBI PMU extension. Caveat, stated now: sel4bench's platform coverage is
thinner on RISC-V than ARM, so the milestone-25 seL4 comparison may still eventually want an ARM
board; that purchase moves to "when 25's leftover justifies it".

**16b: IOMMU-backed DMA isolation, in emulation, on BOTH boards** (parity, Chris's direction
2026-07-27). Each `virt` board emulates its architecture's native IOMMU: SMMUv3 on aarch64
(`-machine virt,iommu=smmuv3`, mature) and the ratified RISC-V IOMMU (v1.0.1) on riscv (newer;
its bugs may be QEMU's, and the record should say which is which). Both sit in front of PCIe,
which §18 drives on both boards; both need `iommu_platform=on` per virtio device, and a device
without it silently bypasses translation, the same manufactured-fact hazard the runners now
fail loudly on. The two IOMMUs are structural siblings, and the deep symmetry is the payoff:
each translates with its own CPU's page-table format (VMSAv8-64; Sv39), so the format-generic
`paging` crate, the seam that was HAL leak #2, builds IOMMU domains with the same proved code
that builds process address spaces. Shape: one portable DMA-domain seam, two arch IOMMU
drivers under `arch/` (device table, command queue, fault queue each), the `Virtio` capability
unchanged above, the disk and attacker suites running behind the IOMMU on both ISAs, and the
shadow ring demoted to defence in depth everywhere. Silicon carries 16b's riscv code over when
a board ships the ratified spec; that is the emulate-then-carry pattern the kernel was built
on. Parity is claimed at the QEMU tier; 16a's silicon is one board first, honestly.

**Built 2026-07-28** (16b, both ISAs in emulation; DECISIONS §20, notes/iommu.md). The portable
DMA-domain seam (`crate::iommu` over `paging::domain`), the two arch drivers (SMMUv3, RISC-V IOMMU
v1.0.1), boot bring-up (SMMU from the device tree, RISC-V IOMMU enumerated as a PCI function), the
`iommu_platform=on` enablement with the confinement test as the loud-on-bypass guard, and the disk
and both attacker suites passing behind the IOMMU on both boards (aarch64 118 kernel tests, riscv
60). Both emulations behaved to spec, no QEMU-vs-ours bug surfaced. Shadow ring kept as defence in
depth. Remaining under 16: **16a** (first silicon on a RISC-V board) is still the hardware step;
16b's riscv driver carries over when a board ships the ratified spec.

**Why.** This is where the discussion's strongest pro-microkernel argument finally becomes true
for us. Today driver isolation is real only because of the shadow descriptor ring we wrote
(notes/dma.md); an IOMMU makes it real in hardware, with the software ring demoted to defence in
depth.

**Prior art.** design/driver-domains.md already works the principled version (a driver per VM,
stage-2 behind a hypervisor). Hardware-gated there; 16b's emulation-first path is not.

**Also closes an integrity window (milestone 22's precondition).** Before DMA is confined in
silicon, a malicious device can DMA over any RAM the kernel has not walled off, *including the
initrd holding init before the kernel has loaded and measured it*. Software confinement (the shadow
ring) governs a driver the kernel already trusts to run; it does nothing about a device corrupting
init's bytes at rest. So verifying init (22) is only airtight once 16 removes the way to tamper with
it underneath the check.

### 22. Trusted init: verify it, and shrink what a broken one can do

**In brief.** Measured/secure boot that checks init before running it; reduce init's authority so a compromise is bounded

**Why it matters.** **closes the thesis's own soft spot:** init is the privileged *unverified* component the whole system is built by

**The soft spot this closes.** §14 promises "a verified core that confines unverified workloads."
init is unverified, but it is not a *typical* workload: it holds the process-construction authority
and builds every other process. At runtime the kernel confines it as well as anything (MMU
isolation is proved, its code is W^X, capabilities are unforgeable), and a compromised init
**cannot break the kernel or escape confinement**. But its *bytes* are currently loaded unsigned and
unchecked, and its *authority* is broad, so within that authority a corrupted init can do real harm
(endow malicious children, deny the system it was meant to start).

**Deliverable, three halves.**

1. **Verify init before it runs. (Phase B.1, BUILT 2026-07-29.)** A measured boot step: the kernel
   checks init's hash before dropping to EL0/U-mode at its entry. seL4's high-assurance deployments do
   exactly this for the root task; it was the single biggest gap between "verified kernel" and
   "trustworthy system." Built as the **measured** variant: the build hashes the archive entry it
   packed and `kernel/build.rs` compiles the digest into the kernel image, so the check means "this
   kernel image runs exactly this init" with no keys and no signature code in the TCB. SHA-256,
   hand-written in `crates/measured_boot`, one implementation shared by the build and the kernel. Fails
   closed both ways: wrong bytes halt, and an *unmeasured* program halts too (an empty trust root
   vouches for nothing). Both ISAs. The **signature** variant (update init without rebuilding the
   kernel, at the cost of Ed25519 in the TCB and a key-custody question) is recorded in DECISIONS §26's
   phase B block as a follow-up, not built. See notes/trusted-init.md.
2. **Shrink the blast radius. (Phase B.2, BUILT 2026-07-29; the interactive boot's migration is the
   remaining increment.)** Reduce what a compromised init can do: hand most process-construction to
   smaller, less-privileged sub-servers, so init's own authority is minimal and short-lived (build the
   first servers, then drop the untyped). The less init holds, the less a broken init costs. Built as a
   four-program tree (`root_supervisor`, `spawner`, `sub_server_supervisor`, `flaky`): the spawner holds one program image and
   a `WRITE`-only budget (not the archive, so it can build exactly one program), the supervisor holds
   no memory at all and can only *ask*, and the root deletes its untyped once both are running. Proven
   on both ISAs by authority rather than timing: after the handoff, retyping a page or a kernel object
   from init fails with `NoSuchSlot`, and a faulting sub-server is reaped and restarted by its own
   supervisor. `system_initializer` and `hello`'s init role still hold their budgets for life (they remain the
   shell's spawn service); migrating that hand-validated boot path is the next increment. Two design
   forks found and reported rather than built through (a reap-only right, and turning a tid into a
   handle). See DECISIONS §26's phase B.2 block and notes/trusted-init.md.

   **Both of those forks are now closed (DECISIONS §32, BUILT 2026-07-29, both ISAs).** Reaping
   moved off `Untyped::DESTROY`, which needs `WRITE` on the region and therefore the same right that
   *builds* a process from it, onto `Endpoint::REAP` on the supervision endpoint. Authorization
   needed no new bookkeeping: §26 already records `Thread::fault_ep` and the kernel already stamps
   the tid, so the check is that the named thread's recorded endpoint *is* the one being invoked.
   The tid-to-handle fork is closed for this case by the same move, because the tid is authorized
   relative to the endpoint it arrived on rather than being a global handle. The measured payoff:
   **`sub_server_supervisor` now holds nothing but endpoints**, since the phase B.2 proxy that had to ask `spawner`
   to reap is no longer needed. The measured limit: milestone 36's `c_confiner` still holds a
   construction budget because it is *also* the builder, which shows the bundling was two things and
   only one of them was the reap. `REAP` refuses a live thread on purpose, so a **hung** child still
   cannot be restarted; that is the watchdog case and it belongs to 23. Two Kani harnesses in
   `crates/capability` cover the authorization invariant. See notes/supervision.md.
3. **Supervise, don't relaunch-in-kernel.** What happens when init (or any server) *fails*, as
   distinct from being corrupted. The failure of init degrades to a **halt, never a breach**
   (the kernel's guarantees hold regardless), so the only open question is availability: halt, or
   recover? The answer is neither a bare halt nor a kernel that relaunches init.

   - **Not kernel-relaunch.** Relaunching init from the kernel re-imports the loader we just
     evicted (milestone 19) plus *restart policy* (retries, backoff, escalation) into the trusted
     core, and it crash-loops on a deterministic fault (init panics on a bad ELF; relaunch hits
     the same bug). Restart is policy, and policy does not belong in the kernel.
   - **The mechanism/policy split, as everywhere else.** Add one small *mechanism* to the kernel:
     a **fault/death notification**, when a thread faults or exits, the kernel delivers a message
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
     cannot build the system, halt is correct: nothing to recover to). init-*after*-boot stops
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

**In brief.** Every userspace component (driver, server, app) is a swappable, vendor-shippable unit behind a stable contract; operators replace them live, no reboot. The console hot-swap is instance one; a durable queue-broker decouples component lifecycles (opt-in per channel, for latency)

**Why it matters.** **the flagship payoff and a product ambition:** competing vendor components, confined by the kernel and swapped live; the verified core is the one fixed thing

**Status (2026-07-30): the mechanism is built and proven on both ISAs; the generalisations below are
not.** DECISIONS §41, notes/live-replacement.md. What landed: the four steps, an unprivileged
operator (`swapper`) that runs them, a client (`chatty`) that talks across the swap and is its own
witness, an attacker holding the client's exact capabilities that cannot become the server, a control
that must fail (the outgoing instance reads a UART register after the revoke and faults, at the
device's own page, with the kernel as the witness), and a replacement written in **C** over §31's
seam, so what held across the swap is the contract rather than a recompile. Both rungs of the ladder
that this milestone specified are built (`broker` is the opt-in one), and priced: `broker_rtt`.

**Three things the build settled, all in §41.** The block imagined a forwarding *process* as the
broker; it does not need one, because §12's endpoint-only naming already makes the endpoint object
the stable name, so the swap costs **zero** in steady state and the kernel's sender queue buffers the
down window. The block's step order (start the new server, then revoke) does not survive contact:
revocation is by physical page, so the endowment has to move to the far side of the revoke, though
the *build* does stay first. And revoking a **device** had to mean take-back rather than destroy,
which is the "deferred CDT finally earns its keep" this block predicted, at one level of the tree.

**What remains, and it is the part the block itself calls the real engineering:** state handoff
(the component here is near-stateless, which is what makes kill-and-replace sufficient), a component
manifest (endowments are literals in the operator's source), dependency-aware orchestration, and the
hung-component case (§32's watchdog). Also the console proper: the component swapped owns the real
UART and is shaped like a console server, but `line_editor`/`display_terminal`/`compositor` are not themselves swapped,
because the interactive stack is not running under the test harness.

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

**In brief.** Partition the shared thread table and endpoints

**Why it matters.** optional; not on the thesis path

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

**In brief.** Machine-checked proofs of `capability`, then IPC, then MMU isolation

**Why it matters.** **the verification itself.** **Built:** `capability`, IPC (rendezvous + one-shot Reply), and the MMU isolation invariants are all proved

**Green-lit and started; see DECISIONS §14 and notes/verification.md.** This is the verification
thesis as an actual work item rather than an aspiration.

**Deliverable.** Machine-checked proofs (Kani) of the security-critical logic, spreading inward from
the capability core. `crates/capability` is proved already: five harnesses covering "`derive` never widens
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

**Status (2026-07-29), with milestone 35 done.** The proved set is now broad: 13 crates, ~60
harnesses, covering `capability`, `ipc` (rendezvous, one-shot reply, the collected-sender path), the MMU
codec on *both* formats (`paging`: VMSAv8-64 and Sv39, level-walk and leaf permission separation),
generational names (`slots`: a removed name never resolves again), frame allocation, region
split/destroy arithmetic, ELF parsing, the device-tree reader, ASID allocation, PCI decode, and now
the DMA-confinement validator (`dma_validator`) and the IOMMU domain's page set (`paging::domain`), both
milestone 35. An audit against the TCB (prompted by asking "what should we prove that we haven't")
found the boundaries proved with **one glaring exception: the DMA-confinement validator was
attacker-tested, never proved.** Milestone 35 closed it: the validator is extracted and proved for every
input, the `Untyped::SPLIT` mint site is proved to hand a child *exactly* its parent's rights, and the
IOMMU domain's *maps-exactly* property is proved too (its page set, in both directions,
format-independently, so one proof covers both IOMMUs; the build-and-translate round trip stays on the
declined BMC wall and on tests). What milestone 35 explicitly does **not** prove, and says so in three
places rather than leaving it to be inferred: addresses that reach a device inside a **command payload**
instead of a descriptor, which the validator structurally cannot see and only an IOMMU stops, so on a
board without one they are unconfined. See DECISIONS §30 and notes/verification.md.

### 35. Prove the DMA-confinement boundary (extends 18)

**In brief.** Extract the shadow-ring validator (`validate_and_shadow`) out of `kernel/src/virtio.rs` into a host-testable logic crate and machine-check it: no validated descriptor chain, in either direction and including indirect descriptors and multi-queue, can reference memory outside the driver's granted DMA region. Add the `Untyped::SPLIT` "never widens rights" harness (the one fresh-mint site the caps proof doesn't reach) and confirm the IOMMU domain builder's *maps-exactly-the-grant* property is proved, not just tested.

**Why it matters.** **closes the one isolation boundary we test instead of prove.** Every other confinement seam (caps, MMU, IPC, generational names) is Kani-proved for all inputs; DMA is attacker-tested only. It is also the boundary that makes "don't trust the driver" true, so the proof belongs here, not on the confined component. **Load-bearing for 16a:** the VisionFive 2 has no IOMMU, so on first silicon this validator is the *sole* DMA confinement, not defence in depth

**The gap, stated precisely.** `validate_and_shadow` (`kernel/src/virtio.rs`) is the shadow-ring
logic that stops a malicious userspace driver from pointing a device's DMA at memory it was not
granted. It is the boundary that makes "the kernel confines the driver, so you need not trust the
driver" *true*. Every other isolation boundary in the system is Kani-proved for all inputs; this
one is covered by attacker tests that hit specific cases. It is pure bounds-checking over
descriptor structures, exactly what bounded model checking is good at. The only reason it is not
already proved is *where it lives*: the proved things are host-compilable pure-logic crates, and
the validator sits inside the kernel crate.

**Deliverable.**

1. **Extract and prove the validator.** Lift the validation logic into a `crates/`-style
   host-testable crate (the way `capability`, `ipc`, and `paging` were carved out), then prove the core
   property: no validated descriptor chain can reference memory outside the driver's granted
   region. Cover **both directions** (TX device-reads and RX device-writes-into-driver-memory,
   the milestone 30 addition), **indirect descriptors** (the escape the attacker suite already
   probes), and **multi-queue** (per-queue block isolation, also milestone 30). The kernel keeps
   calling the proved logic; the extraction must not change behaviour, held against the green
   attacker suite.
2. **The `Untyped::SPLIT` rights harness.** SPLIT mints a child budget at `untyped_cap_rights`, a
   fresh-mint site *outside* `capability::derive`, so the existing "derive never widens rights" proof
   does not reach it. It is currently pinned by one kernel test (added with milestone 31's
   rights-inheritance fix). Add the companion harness, "split never widens rights", beside the
   existing one, so the "authority never widens" story is proved at *every* mint site.
3. **Confirm the IOMMU domain property.** `paging`'s codec is proved; verify that the domain
   builder (`build_identity_domain`, milestone 16b) has a harness for the *maps-exactly-the-grant*
   property (the device domain maps precisely the granted frames and nothing else), not just a
   test. It is the sibling of the validator property, on the hardware side.

**Done (2026-07-29).** DECISIONS §30 is the decision record; notes/verification.md has the harness
tables, the bounds with their justifications, and the boundary statement; notes/dma.md leads with the
what-is-proved-and-what-is-not map.

1. **The validator** is `crates/dma_validator`, host-testable pure logic the kernel's
   `validate_and_shadow` calls; **seven** Kani harnesses prove no descriptor the walk shadows escapes
   the granted region or is indirect, covering both directions (symbolic flags include the RX
   device-writable bit), indirect descriptors, chains including cycles, **ring-index wraparound through
   `u16` and outer-loop termination**, overflowing address arithmetic, multi-queue block isolation, the
   oversized-batch bound, and the mutated-after-validation (TOCTOU) case. The QEMU attacker suite
   (DMA-escape and indirect-escape, both ISAs, both transports) is unchanged and green, so the
   extraction is faithful. The ring layout constants moved *into* the crate with the kernel aliasing
   them, because a proof about a copy of the layout proves nothing about the layout that runs.
2. **`split_never_widens_rights`** in `crates/capability` proves the `Untyped::SPLIT` mint (routed through
   `Cap::mint_child`) gives the child **exactly** the parent's rights. §16's amendment (SPLIT grants
   `GRANT` so a budget is delegable) makes the loose phrasing wrong, so the property is stated and
   proved as equality: `mint_child` takes no rights argument, delegability is a property of the *root's*
   mint, and rights down a budget tree are monotonically non-increasing.
3. **The IOMMU domain's *maps-exactly* property is proved**, reversing this milestone's own first
   answer. That answer declined it as the build-and-translate BMC wall, which was the right diagnosis of
   the wrong target: the wall is a symbolic IOVA walking a *built* table, and the *page set* the builder
   feeds the mapper is loopless arithmetic needing no tables. Factored out (`paging::domain::grant_pages`,
   `grant_page`, which `build_identity_domain` now calls instead of `map_range`'s unchecked
   `va + i * PAGE_SIZE`) and proved by six harnesses in both directions: soundness (no page outside the
   grant) and completeness (no whole page of the grant unmapped, because a domain mapping *nothing*
   would satisfy soundness perfectly and confine by starvation). Format-independent, so one proof covers
   SMMUv3 and the RISC-V IOMMU with no parity gap. The residual link, "`Mapper::map` writes exactly one
   leaf and touches nothing else", stays on the proved walk arithmetic plus `domain.rs`'s
   build-and-translate tests on both formats and 16b's hardware attacker test.

**Every new property was falsified before it was believed**, and one falsification corrected the code's
own comment: soundness rests on `grant_pages` flooring, not on `grant_page`'s partial-page guard, which
cannot fire for any index the builder passes. Proving the domain also hardened it against a region whose
`base + size` wraps `u64` (recorded as a proof obligation closed, not a reachable bug: the frame
allocator would run dry long before the multiply could wrap).

**The residual gap this milestone does NOT close, stated because a proof that reads as broader than it
is does damage.** The proof is about *descriptor chains*. Per §29, a virtio-gpu's backing addresses ride
in a `RESOURCE_ATTACH_BACKING` **command payload**, which the validator **structurally cannot see** (the
addresses are not in its input), and teaching the transport to parse device commands would breach §18.
So: descriptor-borne addresses are provably confined; payload-borne addresses are confined by the
**IOMMU alone**, whose allow-list item 3 now proves exact (a narrowing, not a closing: the hardware
honouring that allow-list stays an attacker test, and the transport still cannot see the addresses); and
**on a board with no IOMMU nothing confines them at all.**
That inverts this milestone's own load-bearing argument for the payload path: "prove the validator
because on the VisionFive 2 it is all there is" holds only where the validator can look. On that board a
display driver is either trusted with all of physical memory or the transport grows a device-aware
check. Whoever sequences 16a decides; it is a decision, not an oversight.

**Why it is load-bearing now, not later.** Milestone 16a's board, the VisionFive 2, **has no
IOMMU** (notes/target-hardware.md). We demoted the software validator to "defence in depth" when
16b landed the emulated IOMMU, but on first real silicon there is no hardware behind it: the
shadow-ring validator is the *sole* DMA confinement. So this proof should precede or accompany
16a, not trail the optional reach work. It is the §18 thesis ("spread inward from the capability
core") reaching the last unproved isolation boundary, and it is the one place the "verified core"
claim currently rests on testing.

**What stays unproved, on purpose.** The confined components themselves (`smoltcp`, RedoxFS, the
drivers) are *not* proof targets: the whole point of the capability core is that a confined
component need not be trusted. Proof effort belongs at the confinement boundary, not on the code
it confines. Likewise the userspace-only crates (`user_heap`, `grant_plan`, `line_editor`) and scheduler
placement policy stay host-tested; a bad placement is a performance bug, not a safety hole.

### 19. Run a real workload

**In brief.** A native-ABI workload first; Linux-compat or VM hosting later

**Why it matters.** **the "runs real workloads" half** of the thesis. **Built:** granular verbs and userspace init (19d), init as the real boot path (19d.2c), dedicated binaries delivered as a crickerfs archive with a shared `user_rt` runtime (19f.1-6), the native ABI written down (19e/Decision 2, notes/abi.md, DECISIONS §15), and the first real workload, a CoreMark-derived compute program spawned against that ABI (19e). design/init-and-granular-spawn.md

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

**In brief.** Make `arch/` a real HAL; bring up RISC-V then x86_64

**Why it matters.** the "portable verified core" claim; reach the demonstrator earns

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

### 27. Rust `std` on the native ABI

**In brief.** A custom target whose `std` builds: `Vec`, `String`, `println!`, `Instant`, allocation from the process's own untyped, stdio over the console endpoint, `fs`/`net` honestly `Unsupported` until capability-granted servers back them

**Why it matters.** **widens "runs real workloads" by orders of magnitude**: the pool of programs that build for cricker-os becomes "most Rust code that doesn't touch fs/net", and milestone 23's components become writable by people who are not kernel people. Grows toward general purpose (notes/why-not-general-purpose.md) without smuggling POSIX: the `sys` layer maps to capabilities directly, no fork, no open-by-path

**Built 2026-07-28, both ISAs green; phase two complete 2026-07-29.** std's platform layer runs
directly on the capability ABI (Hermit's shape); a real std program (`Vec`, `String`, `HashMap`,
`println!`, `Instant`) is spawned and checked byte for byte on aarch64 and riscv64. Phase two bound
**`std::net`** to net_stack's socket contract and **`std::fs`** to the §27 FS service, so the same binary
now has three behaviours chosen by its grants alone: a filesystem if it holds a directory capability,
a network if it holds a `Stack` endpoint, and honest `Unsupported` for whichever it was not given.
`std::fs`'s interesting half is what a path *means* with no global namespace: "under the directory I
hold", so an absolute path or a `..` is refused as un-nameable rather than served. `thread::spawn`
remains `Unsupported`, as do the operations no contract verb backs (creating or truncating a file,
directory iteration, permissions, symlinks). See notes/std.md and DECISIONS §22.

**Deliverable.** A custom rustc target (`aarch64-unknown-cricker` / `riscv64-unknown-cricker`,
`-Zbuild-std` against a target spec first, a real target later if ever warranted) whose `std`
compiles and links against the capability ABI (notes/abi.md). Concretely: implement std's
Platform Abstraction Layer (PAL, `library/std/src/sys/pal/*`), a **native** cricker-os backend
over what a process already has, not a libc shim under the Unix one. Allocation draws from the process's own untyped
(the `user_rt` heap growing into a real `GlobalAlloc`); `stdout`/`stderr` SEND to the console
endpoint by slot convention; `Instant`/`SystemTime` read the virtual counter; `panic!` aborts (a
fault the kernel reports) before unwinding is ever attempted; `thread::spawn` retypes a TCB, or
returns `Unsupported` in phase one; `fs` and `net` return `Unsupported`, honestly, until
capability-granted servers exist to back them.

**Why.** The first wall an application hits on cricker-os is "no std" (the note
why-not-general-purpose.md names it), and milestone 23's vendor-component ambition needs
components writable by people who are not kernel people. `std` on the native ABI widens "runs
real workloads" from hand-built `no_std` binaries to most of crates.io that stays off fs and
net, without smuggling the POSIX assumptions the ABI deliberately excludes: no fork, no
open-by-path, no ambient anything. Paths, when they come, name capabilities.

**Prior art and reuse.** Hermit is the closest shape (std's pal implemented directly over a
non-POSIX unikernel ABI) and the model to follow; Fuchsia did the same at scale. Redox took the
other road, std via relibc (a POSIX shim first), which is exactly the "later, if ever"
DECISIONS §15 already prices at nothing. Code to use: rustc's own `build-std` machinery and
target-spec JSON; there is no crate to adopt, because the deliverable IS the pal. Mistake to
avoid: an errno-shaped `sys` layer that makes `std` work by pretending the OS is Unix.

**Sequencing.** After 19 (the ABI, done) and object revocation (done); independent of 16 and 22;
feeds 23 directly. **Effort: unpriced** (it depends on another project's toolchain and API, which
the history here cannot bound). Off the thesis path, like 20 was: a reach the demonstrator earns.

### 28. A solid terminal: the line discipline as a component

**In brief.** Line editing, history, ANSI in/out, control characters, and a written terminal contract, as a **swappable userspace component** between the input/console drivers and applications; Ctrl-C as a capability-routed interrupt to the foreground process, not a Unix signal. **Built, §21**: `line_editor` on both ISAs, a sans-IO engine (20 host tests), the contract in notes/terminal-contract.md, `shell_service` retired for userspace init; Ctrl-C routing **built** (two-tier, DECISIONS §24 amendment): a shared-flag cooperative tier and an `Untyped::DESTROY` forcible tier, shell-held, proven on both ISAs with `heeder`/`spinner`; the shell learns of `^C` through `line_editor`'s `OP_INTRCOUNT`

**Why it matters.** a terminal with real behavior is a far better "instance one" for milestone 23's live component replacement than the raw echo loop, and 27's stdio semantics need a terminal that has semantics. Serial, deliberately; the display terminal is 29, and they must not be confused

**Built 2026-07-28; see DECISIONS §21, notes/line-discipline.md, notes/terminal-contract.md, and
design/interrupt-routing.md (the Ctrl-C fork, decided in DECISIONS §24 and now built per its
implementation amendment: shell-held two-tier routing, proven on both ISAs).**

**Deliverable.** The layer Unix calls the tty line discipline, as a swappable userspace component
between the input/console drivers and applications: line editing (backspace, cursor keys,
kill/yank), history, ANSI escape parsing in and out, control characters, and a written contract
for what a terminal owes a program. The interesting design is **Ctrl-C**: interrupting the
foreground process is a capability-routing question (who holds the right to interrupt whom), and
this project's answer will not be Unix signals; that answer is the milestone's kernel-adjacent
substance. Serial, deliberately: the terminal emulator stays on the host end of the wire.

**Why.** Milestone 23's flagship line ("the console hot-swap is instance one") deserves a
component with real behavior, and 27's stdio semantics (line buffering, `read_line`) need a
terminal that has semantics. Pure userspace on machinery that all exists; could land any time.

**Prior art and reuse.** Userspace, outside the TCB: the rule says actively prefer porting.
`noline` (a no_std readline) and `embedded-cli` are live candidates for the editing core; the
component contract and the interrupt routing are ours. Read the Unix tty layer as the
mistake-catalog (its tangle is famous) and Plan 9 (editing pushed to the client) as the
counter-design. **Effort: 1 lane** (measured: it took one).

### 29. A display terminal: framebuffer, virtio-gpu, and a foreign component

**In brief.** An on-device terminal: a userspace virtio-gpu driver (arriving over **PCIe**, which the §18 transport just made reachable), a framebuffer component, font rendering, and a VT state engine maintaining the grid; input from a virtio keyboard

**Why it matters.** the first pixels the demonstrator ever puts on a screen, and the strongest form of the milestone-23 claim if the VT engine is **libghostty-vt** (zero-dependency, no-libc, no-alloc, C ABI, Zig): a vendor component in a foreign language, capability-confined and hot-swappable. **Promoted from optional (2026-07-28): rung one of the display ladder (see "The display ladder" below), whose destination is a capability-routed compositor**

**Increment one built (2026-07-29, both ISAs): the first pixels, and the framebuffer seam.** A
confined userspace virtio-gpu driver (`display`) drives the control queue through the proved validator
over the §18 PCIe transport on both `virt` boards, behind the IOMMU; a *separate* client (`painter`)
holds only an endpoint and a shared surface and draws a coordinate-derived pattern into it. Two
witnesses in two address spaces digest the result against a value the kernel computes itself, so the
**framebuffer** is proven byte for byte; and the **scanout** is proven too, from the host, by driving
QEMU's monitor beside the suite and comparing a `screendump` PPM against the same pattern definition
pixel for pixel (both ISAs, with a negative control on the checker). The memory decision
generalized to a rule (a framebuffer is a bigger grant, never an exemption) and the GPU's own
confinement hazard (backing addresses ride in a command payload the transport validator cannot see, so
the IOMMU is the barrier) is proved by an attacker test. DECISIONS §29,
notes/framebuffer-contract.md.

**Increment two built (2026-07-30, both ISAs): glyphs, the grid, and a real keyboard.** DECISIONS
§37, notes/glyphs.md. A public-domain 8x8 bitmap font (`crates/bitfont`; the licence drove the choice,
because a font is compiled into the image), a **sans-IO VT engine** (`crates/video_terminal`) checked against the
*real* line discipline's echo stream rather than a written-down list of escape sequences, a display
terminal (`user/src/display_terminal.rs`) that is a client at **both** display seams with exactly `painter`'s and
exactly `window`'s authority, and a confined virtio-input keyboard driver (`user/src/kbd.rs`).

Three things are worth carrying forward from it. **The picture is a value three witnesses compute
independently** (the terminal to draw, the kernel to predict the framebuffer, the host to grade QEMU's
scanout), which is what replaces "it looked right" for text; the host checker's negative control is a
screen with **one letter changed**. **Neither display contract needed a line changed**, and that is
now a spawn literal rather than a claim. And **the authority to type is a mapping**: the keyboard's
power is the input ring no client maps, while the doorbell it rings carries nothing, so focus stays a
capability from the producing side as well as the receiving one.

**Still deferred, and stated rather than implied:** scrollback (it wants a ring of off-screen rows and
a viewport, which changes the damage model), UTF-8, reflow, and line editing in the display terminal
(`line_editor` composes in front of it with no new protocol, which `crates/video_terminal` proves on the host). The VT
engine's language remains an open question, and notes/glyphs.md now **prices** it: building the Rust
engine first changed what the comparison is about, because a VT engine fits the §31 C seam's shape
almost perfectly and the real cost of adopting libghostty-vt is rebuilding the *proof structure*, not
the rendering. The recommendation there is to adopt it as a second engine behind the same seam rather
than a replacement. **Architect's call.**

**Deliverable.** The demonstrator's first pixels: a userspace **virtio-gpu** driver (the device
arrives over PCIe on both `virt` boards, which the §18 transport just made reachable), a
framebuffer mapped into a terminal component, font rendering, and a VT state engine maintaining
the grid (escape parsing, scrollback, wrapping, reflow); keyboard input via virtio-input. The
serial console remains; this is a second head, not a replacement.

**Why, and the 23 connection.** The VT engine is the strongest candidate anywhere in the plan
for the full form of milestone 23's claim: **libghostty-vt** (Ghostty's extracted core:
zero-dependency, no libc, fixed buffers with no allocations, C ABI, implemented in Zig) running
as a capability-confined, hot-swappable vendor component would mean the kernel safely runs code
we did not write, in a language we do not use. Costs stated plainly: a Zig toolchain enters the
build for that one component, and their API is still in flux, so any adoption pins a version.
The single-toolchain fallback is `vte` (alacritty's parser): same shape, our language, much less
complete (no scrollback or reflow).

**Sequencing.** Needs the PCIe transport (done) and wants 28's contract first so the display
terminal implements a contract rather than inventing one. Optional and well off the thesis path;
a reach in the 24 spirit. **Effort: 2 lanes** (measured: first pixels, then glyphs/VT/input).

### 30. The network stack as a confined component

**In brief.** A userspace **virtio-net** driver behind the DMA confinement (extended to multi-queue: RX means the device writes INTO driver memory), and the TCP/IP stack itself (`smoltcp`) as a swappable userspace component with a capability-shaped socket contract; backs `std::net` for 27

**Why it matters.** **the canonical microkernel component**, the one people ask about first when a minimal kernel claims to stand next to Linux; and milestone 23's most convincing instance, hot-swapping a network stack under open connections. The reuse call is the plan's easiest: the thesis is the kernel confining the stack, not the stack

**Deliverable.** Two components and a contract. A userspace **virtio-net** driver, confined by
the same shadow-ring validator as the disk, which requires the one genuinely new kernel-adjacent
piece: **multi-queue transport support** (virtio-net needs RX and TX; the §18 seam and the
confinement are queue-0-only today, and RX is the direction where the *device writes into*
driver memory, so the validator grows a proved, tested second direction rather than an ad hoc
one). Above it, the TCP/IP stack itself as a swappable userspace component, `smoltcp` inside a
net server, speaking a capability-shaped socket contract: an endpoint plus shared frames per
connection, no ambient "the network"; a process holds a capability to a stack or it does not.
The contract is what `std::net`'s PAL (milestone 27) binds to, replacing its honest
`Unsupported`. Scope discipline: TCP, UDP, DHCP, done; no sockets-API mimicry beyond what the
PAL needs.

**Why.** A userspace network stack has been the defining microkernel component since Mach and
L4, and it is the first thing people ask about when a minimal kernel claims to stand next to
Linux. Milestone 23 gets its most convincing instance: live-replacing a network stack under
open connections is a far harder-nosed test of the component contract than a console swap. And
the multi-queue RX confinement is real DMA-isolation work that should land under the
validator's discipline, not be retrofitted when a NIC needs it on real hardware.

**Prior art and reuse.** The reuse call is the easiest in the plan: `smoltcp` (no_std,
kernel-agnostic, event-driven, proven across embedded Rust; Redox has shipped on it). Building
TCP by hand proves nothing thesis-relevant. Prior art to read before the contract is drawn:
seL4's net_stack componentization, Fuchsia's Netstack3 (Rust, capability-routed, the closest
cousin), and Plan 9's /net as the counter-design (per-connection filesystem, everything a
file). Testing is cheap: QEMU's user-mode networking NATs the guest with zero host setup.

**Sequencing.** After the PCIe transport (done); the multi-queue confinement is the
prerequisite piece and worth building first as its own tested step. Feeds 23 and 27.
**Effort: 3 lanes** (measured: multi-queue confinement, the driver and net_stack, then the socket contract).

### 31. A capability shell: designation is authorization

**In brief.** The command line becomes a **grant expression**: naming a resource in a command IS the capability grant (`wc report.txt` passes one readable file cap; `wc` alone can read nothing, and the refusal is "no such capability", not EPERM); untyped budgets as first-class grants; a SHILL-style manifest per program checked at spawn; a `caps` command printing a process's whole endowment. **Phase 1 built, both ISAs**: `grant_plan` (host-tested parse + manifest + spawn protocol), the shell over the existing surface, `--mem N` made real by the `budgeter` program, manifest refusals, `caps`/`caps <command>` introspection; one kernel fix, `Untyped::SPLIT` now grants the child `GRANT` (DECISIONS §16 amendment). **Phase 2 built, both ISAs**: the FS contract's `CREATE`/`TRUNCATE` (so `std::fs::write` works), and per-file grants as a **caretaker process** (`fs_file_caretaker`) that narrows a directory capability to one file in one direction, proven by a read-only and a writable attacker. One scope note: the interactive shell still refuses a named file because its boot wires no FS service, so it holds no directory to narrow. **The grammar shown here is milestone 47's**, which deleted the `run` verb and the `file:` designator this milestone shipped with; the mechanism did not change, only the spelling. Notes: grant-expression.md, program-manifest.md, fs-server.md

**Why it matters.** **no-ambient-authority made user-visible**: the inversion of Unix's model at the one interface a human touches. Milestone 23's component contract in embryo, met first at the shell

**Phase 1 built (both ISAs).** The command line is a grant expression: `grant_plan` (a host-tested crate)
parses it and checks it against a per-program manifest; the shell holds its own untyped budget and
delegates from it. `budgeter --mem N` splits N pages off the shell's budget and delegates the
untyped to init, which endows the child; the budgeter maps them and reports the count (15 of 16, the
rest paid for page tables), proving the grant is real, not parsed-and-ignored. Manifest mismatches
and a named file a program declares but this shell cannot back ("you hold no such capability") are
refused at the prompt; `caps` and `caps <command>` print a process's whole endowment. (The spelling
is milestone 47's: it shipped as `run --mem N budgeter` and `file:PATH`.) One kernel change: `Untyped::SPLIT` grants the
child `GRANT` so an untyped is delegable (DECISIONS §16 amendment), which the headline feature
required and no other object type lacked. Notes: grant-expression.md, program-manifest.md.

**Phase 2 built (both ISAs): per-file grants.** The FS service's unit of authority is a *directory*
(DECISIONS §27), and `run wc file:report.txt` says less than that, so the narrowing is a
**caretaker** in Mark Miller's sense: `user/src/fs_file_caretaker.rs` holds the directory
capability, opens the granted name once, and serves the same contract on its own endpoint with a
namespace of exactly one name. Any other name is `ENOENT` (in this scope there is no such name);
`CREATE` is `ENOTDIR` (a file is not a directory); a write without the direction is `EROFS`. Each
refusal is a fact about what the holder has, not a permission that could have said yes.

It is a separate process for two reasons. The FS server receives on one endpoint, so serving a
second narrower one would need a receive over a *set*, which means badging endpoint capabilities
(seL4's answer) and is a design fork, recorded rather than taken. And it makes the claim checkable:
the confined program holds an endpoint to the caretaker and nothing that names the FS server, so "it
cannot reach a second file" is a property of its cspace rather than of a branch it is trusted to
take.

**Proven by an attacker, twice, and the second run is what makes the first mean anything.** It
reports a bitmap of what got through. Read-only grant: every bit clear, against a neighbouring file
that exists and that the caretaker could open. Read/write grant, same shape: the two write bits set
and everything else clear. A caretaker that refused every request passes the first and fails the
second. Phase 2 also landed the contract's `CREATE` and `TRUNCATE` (so `File::create` and
`std::fs::write` work rather than returning `Unsupported`), a name check that was previously true
only by the absence of a path walker, and a measured stack for the FS server after a 528-byte
overflow presented as a mystery 900-second test.

**Why the status is PARTIAL and not BUILT, stated plainly.** The mechanism is complete and gated on
both ISAs, but this milestone's headline is about *the one interface a human touches*, and at that
interface `wc report.txt` is still a refusal. The interactive shell holds no directory to
narrow, because the boot that starts it wires no FS service; the refusal it prints ("you hold no such
capability: this shell was granted no directory to narrow") is **true** rather than a placeholder,
and `caps` says the same. `grant_plan` carries the whole vocabulary (`FileSpec` in the manifest, a
`FileGrant` in the endowment, refusals both ways, `caps` printing the file endowment), and the
decision is a function of what the shell *holds* rather than of the calendar, which phase 1's
hardcoded "arrives with milestone 32" was not.

**Phase 3, then, is exactly one thing:** wire an FS service into the interactive boot (the kernel's
shell boot path, a RedoxFS disk on the interactive runner, and init building the caretaker per
grant), and flip `holdings()` in `user/src/swish.rs`. It was not done here because **nothing in the
test suite boots the interactive shell**, so it would ship unexercised, and a demonstrator's ungated
feature is worse than a recorded gap. Whoever takes it should consider gating that boot first.

**Deliverable.** Invert Unix's authority model at the command line. A Unix child inherits your
entire authority; a cricker-os command line is a **grant expression**: every argument that
designates a resource passes a narrowed capability, and nothing else flows. `run wc report.txt`
grants exactly one readable file capability, because typing the name IS the grant (Miller's
principle: designation is authorization); `run wc` alone spawns a process that can read
nothing, and the failure is "you hold no such capability", legible, not EPERM. Untyped budgets
become first-class grants (`run --mem 16 prog`), the most cricker-os-native piece of the
inversion, with no Unix analog. From SHILL, adapted: a small **manifest** per program declaring
its expected endowment (one readable file, one endpoint, N pages), checked at spawn, so a
mismatch is a refusal at the prompt rather than a mystery hang; this is milestone 23's
component contract in embryo. Introspection is a feature: a `caps` command prints a process's
complete endowment, making §14's "reading one literal tells you a process's whole authority"
interactively true.

**Scoping constraint, honest.** File capabilities need something to point at; phase one grants
what exists (program spawns, endpoints, frames, untyped, device caps), and per-file grants
arrive with milestone 32's FS server, whose handles must be capability-shaped from birth partly
BECAUSE this milestone will point at them.

**Prior art and reuse.** Designs only; nothing portable. SHILL (OSDI 2014: capability
contracts for scripts, on Capsicum) is the academic anchor; Mark Miller's object-capability
line (E, CapDesk, Polaris) supplies the organizing principle; Plash is the Linux attempt worth
reading as the mistake catalog. Feeds 23 and 22 (shrinking ambient authority, met at the human
layer); sits behind 28's terminal contract. **Effort: 2 lanes built** (the grant expression, then
CREATE/TRUNCATE and per-file grants), **1 more estimated** for phase 3, which is one item: gating the
interactive boot so an FS service can be wired into it.

### 32. A real filesystem: RedoxFS behind a capability FS server

**In brief.** A write-capable block path, an FS-server **component** whose handles are capabilities from birth (open-by-path exists only INSIDE the server, relative to a granted directory cap), and **RedoxFS** as the on-disk engine, ported behind its own `Disk` trait over blk IPC

**Why it matters.** the flagship **userspace-reuse** story the prior-art note predicted: a real CoW filesystem we did not write, running confined; and the thing 31's per-file grants point at

**Phase 1 built** (the write-capable block path; DECISIONS §22 area, notes/dma.md). **Phase 2
built, read path** (2026-07-28; DECISIONS §27, notes/fs-server.md): RedoxFS runs confined as a
three-process userspace service (block server over blk IPC, FS server over the vendored no_std core
with its own untyped heap, client holding a directory capability), and a client opens the shipped
`motd` through a granted directory capability and reads it back, proven on aarch64 and riscv64 with
a host-tool consistency check. The contract is capability-shaped from birth and adds no syscall; the
error type maps to the wire exactly once; creation stays host-side. **Phase 2 write path proven
too** (2026-07-29, through `std::fs`): the old "on-device writes loop inside RedoxFS's allocator
commit" open item was stale and is corrected in DECISIONS §27 and notes/fs-server.md. A guest write
now reads back when the host tool reopens the image, and that reopen is in the gate. What remains is a
*contract* gap, not a write-path one: no `CREATE` and no `TRUNCATE` verb, so `std::fs::write` and
`File::create` are honestly Unsupported and a write means opening a file the image already carries.

**Deliverable.** Three pieces. A **write-capable block path** (the driver and the confinement
validator already speak both directions; the write verbs and tests are the new work). An
**FS-server component** whose contract is capability-shaped from birth: a file handle is a
capability; open-by-path exists only inside the server, resolved relative to a *granted
directory capability*, so designation keeps flowing the 31 way and no global namespace ever
appears. And **RedoxFS as the on-disk engine**: port the `redoxfs` core behind its own `Disk`
trait, implemented over blk IPC.

**Why RedoxFS.** The prior-art survey named it the best single candidate the day the reuse
rule was written: a real CoW, transactional filesystem in Rust, MIT-licensed, shipping daily in
Redox, and only loosely coupled to Redox's syscalls precisely because it also runs on
Linux/FUSE, which is itself a gift (images can be created and inspected on the host with the
same code that serves them on cricker-os). It is the flagship form of the userspace-reuse
thesis: the kernel confining a serious component we did not write.

**The port plan, fixed by the audit** (notes/redoxfs-audit.md; done against 0.9.1, by
building, so the implementer starts here rather than rediscovering):

1. **Pin 0.9.1**, vendor or patch-dep with the audit's patch: two added `use alloc::vec::Vec`
   lines (one each in `filesystem.rs` and `record.rs`, fixing three E0425 sites; with std on,
   the prelude supplied `Vec`, so the untested no_std path bit-rotted). Offer it upstream,
   ideally with a `--no-default-features` CI check, so the pin can eventually drop it. Build with `--no-default-features` on the workspace nightly; both
   bare-metal targets are proven to compile.
2. **The allocator comes first** and is shared work with 27's PAL: an untyped-backed
   `GlobalAlloc` in `user_rt`. The core is alloc-heavy; nothing else runs without this.
3. **The `Disk` impl is a blk-IPC client**: `read_at(block, &mut [u8])`, `write_at(block,
   &[u8])`, `size()`, all synchronous, returning `syscall::error::Result`; map that error type
   to ABI errors once, at the server boundary, and nowhere else.
4. **Only operate on-device; never create.** The std-gated core APIs are exactly creation
   (`FileSystem::create`, uuid v4, getrandom): `mkfs` and inspection stay host-side via FUSE.
   The server opens an existing image, full stop; entropy never becomes a userspace dependency.
5. Known-and-accepted: the unconditional `libc` dep is a manifest wart (host-binaries-only,
   proven harmless on `none` targets), and aes/xts/argon2/lz4 ride along as binary size with
   encryption unused in phase one.

**Risks, priced.** (1) ~~The core's std/alloc footprint needs auditing~~ **Audited, retired**
(notes/redoxfs-audit.md): `std` is a feature, not an assumption; the ~5,400-line core compiles
for BOTH of our bare-metal targets today, three bit-rotted imports away from clean, and the
`Disk` trait is three synchronous methods shaped exactly like a blk-IPC client. The one real
cost is a `GlobalAlloc` for the FS-server process, which milestone 27's PAL needs anyway;
creation paths (`mkfs`, uuid, entropy) are the only std-gated core APIs and stay on the host. (2) The write path is new on our
side, driver through validator through tests, and errors there eat filesystems; the CoW design
is forgiving, but the tests must include kill-mid-write. (3) Upstream coupling: pin a version,
carry patches, and record divergence, the same discipline as any vendored engine.

**Prior art and reuse.** RedoxFS is the reuse. Alternatives on the record: FAT (host interop
and simplicity, no integrity story), littlefs (proven, C, wrong-language FFI for less gain
than ghostty-vt would buy). Feeds 31 (per-file grants), 23 (a component with real state to
hand off across a live swap, the hardest handoff case yet named), 27 (`std::fs`).
**Effort: 3 lanes** (measured: the write-capable block path, the FS server, then integration).

### 36. A foreign-language component, seam first (spike; feeds 29 and 23)

**In brief.** Prove the FFI seam end to end with a *minimal* C component before committing to a large one: bare-metal clang for both bare targets in the build, a Rust `user_rt` shell that holds every capability and does every syscall while the C code gets plain buffers over the C ABI (so the §4 surface does not widen), and only the handful of libc symbols the component actually needs, with `malloc` on milestone 27's untyped-backed `GlobalAlloc`. The deliverable that matters is one test: a deliberate out-of-bounds write in the C code faults the process, touches nothing outside its grant, and its supervisor restarts it. **Built, DECISIONS §31, both ISAs**: clang capability-checked for both backends from one compiler (Apple's is rejected: no RISC-V), `c_shim` holds every capability so the C holds none, the libc turned out to be **two** symbols not five (`compiler_builtins` already supplies the rest), and two witnesses prove the confinement (a read-only page that is the *same physical frame*, and a different frame at the same virtual address). notes/c-seam.md

**Why it matters.** **the thesis in one assertion.** Memory-unsafe foreign code is not a dilution of "a verified core that confines unverified workloads", it is the strongest available demonstration of it: the more unverified the component, the more the confinement has to prove. It also de-risks 29's libghostty-vt rung and 23's vendor-component claim *before* we owe anything to another project's toolchain or API churn

**DONE 2026-07-29**, both ISAs, in QEMU. DECISIONS §31; concept note notes/c-seam.md.

All four deliverables landed as specified, and the two that produced findings are worth reading before
the next foreign component:

1. **Toolchain.** `user/build.rs` compiles `user/c/c_seam.c` with a clang resolved from a candidate
   list and *capability-checked* (`-print-targets` must list both aarch64 and riscv64), object
   handed to the linker for the `c_shim` binary only. One compiler for both ISAs is §19 applied to
   the toolchain, which means **Apple's clang is rejected on purpose** (no RISC-V backend) even
   though it would compile the aarch64 half. `script/bootstrap` grew `brew install llvm` / `apt-get
   install clang`, and the CI clippy job grew the same, since it clippies `user`.
2. **Linkage.** `c_shim` (Rust) holds every capability and makes every syscall; the C gets `(u8*,
   usize)` and returns a scalar. The syscall surface did not change, and could not have: the C
   cannot name a capability slot.
3. **libc.** The object demands five symbols; the linker demands **two** (`malloc`, `free`), because
   `compiler_builtins` already supplies `memcpy`/`memset`/`strlen` weakly for bare targets. **Do not
   shim the other three:** the obvious Rust `memcpy` is `copy_nonoverlapping`, which lowers to a call to
   `memcpy`, so it calls itself, and the symptom is a store fault at `sp` that reads like a stack-size
   problem at any stack size. `malloc` is milestone 27's untyped heap on the instance's own region, so
   one `DESTROY` reclaims it.
4. **The test.** `c_seam_tests`, both ISAs: two out-of-bounds writes (one byte past into a read-only
   page that is the *same physical frame* the confiner holds read/write; one page past into an
   address the component has no mapping for and the confiner does), both fault at exactly the
   address the C computed, both leave a position-derived witness pattern intact byte for byte, and
   the third instance does real C work whose output is checked against an independent Rust
   computation. The control that makes it mean anything: each bug stores *inside* its grant first,
   and that store must be visible.

**The fork this fed, stated concretely.** The confiner is builder, supervisor, and checker in one
process, because reaping needs `WRITE` on the region and `WRITE` is also what builds one. **A
supervisor needs exactly `DESTROY` on one region it did not create**, and nothing narrower exists.
Milestone 22 phase B.2's IPC proxy is the workaround that exists today; this spike deliberately did
not use it, so the requirement is visible in one program instead of hidden behind a hop.

**What it does not prove**, recorded so 29 and 23 do not inherit false confidence: one `clang -c` is
not a build system, one translation unit is not a link order, this component's five symbols are not
another's, and confined is not correct. Sequencing holds: libghostty-vt is tier one (freestanding),
which is the cheapest step up from here.

**Added 2026-07-29, from Chris's question: can we run user services in other languages, like a C
FAT32 that a monolith would have put in the kernel?** The answer is yes, and the roadmap already
commits to one (libghostty-vt, Zig, at 29). This item exists so the *seam* is proven by something
tiny before a large foreign component depends on it.

**Why the language does not matter to the confinement.** Isolation here is enforced by mechanisms
that are entirely language-agnostic: MMU page tables (proved), unforgeable capabilities (proved),
the DMA validator (proved, milestone 35), and the IOMMU. A C component in a confined process can
corrupt its own address space and reach nothing else, and when it dies §26's fault endpoint tells
its supervisor, which restarts it (the tree milestone 22 phase B built). That inverts the usual
worry: memory-unsafe C is not a problem for the thesis, it is the best demonstration of it. The
contrast with a monolith is the whole argument, and it is concrete rather than rhetorical: in-kernel
C means one bug is a kernel compromise (the peer project Atom keeps FAT32, AHCI, and xHCI in the
kernel today); confined C means one bug scribbles its own grant and gets restarted.

**Deliverable, deliberately small.**

1. **Toolchain in the build.** Bare-metal clang cross-compiling for both targets, driven from the
   build the way the rest of userspace is; `script/setup` grows a dependency. The roadmap already
   accepts this cost for Zig at 29, so pay it once, here, where the component is throwaway.
2. **The linkage shape, which must not widen the syscall surface.** A Rust `user_rt` outer shell
   holds every capability and performs every IPC; the C logic is linked in and called over the C ABI
   with plain buffers and makes **zero syscalls**. This is the same sans-IO shape RedoxFS's `Disk`
   trait already uses, just across a language boundary instead of a trait boundary.
3. **The libc question, answered by tier.** Shim only the symbols the component actually needs
   (`memcpy`, `memset`, `strlen`, `malloc`/`free`), with `malloc` backed by milestone 27's
   untyped-backed `GlobalAlloc` (`crates/user_heap` plus `user_rt::heap`).
4. **The test that is the point.** A deliberate out-of-bounds write in the C code must fault the
   process, leave everything outside its grant untouched, and be restarted by its supervisor.

**The line this does not cross.** C dependencies come in three tiers: *freestanding* (no libc,
fixed buffers, no alloc: libghostty-vt, littlefs) is easy; *a handful of symbols* is tractable and
is what this spike proves; *full POSIX* (`open`, `fork`, `socket`, threads) needs a real libc port,
which is the relibc road DECISIONS §15 prices at "later, if ever" and Redox took. Tiers one and two
only. A component wanting the third is a different and much larger project, and saying so here is
what keeps this from becoming one.

**Candidates, and the honest ranking.** Bring in a foreign language only where the foreign
implementation genuinely beats the Rust option. **libghostty-vt** is the roadmap's pick and clears
that bar (a mature VT engine with scrollback and reflow; `vte` is a parser only). **HarfBuzz** if
`rustybuzz` proves insufficient for 33's text shaping. **SQLite** is the canonical "C you cannot
beat" but is tier three. **doomgeneric** has real demonstrator value (memory-unsafe C game,
capability-confined, on a verified core) and Atom already vendored it, so we would be following
rather than leading. **FAT32, the question that prompted this, is a weak first candidate**: RedoxFS
already provides a better filesystem, `no_std` Rust FAT crates exist so the FFI cost buys nothing,
and its real value is host interoperability (write an SD card on a Mac, read it on the milestone-16a
board), which is a 16a story to do in Rust when first silicon makes it concrete.

**Sequencing.** After 29's rung one, so the framebuffer seam exists as a real consumer to point the
component at, and before committing to libghostty-vt. **Effort: 1 lane** (measured). The whole value is that it is
cheap and it fails early: if the toolchain, the shim, or the confinement story has a problem, we
find it with a throwaway component rather than half way into a port.

### 41. Dead code: triage the suppressions, and un-blindfold the gate

**In brief.** Triage all **79** `allow(dead_code)`/`allow(unused)` suppressions in the tree, delete what is dead, and replace the module-wide ones with per-item allows that carry a reason. Three distinct classes, only one of which is tidying. (1) **The gate is blindfolded over 5,831 lines**: six files carry module-wide `#![allow(dead_code)]`, including `sched.rs` (3,166 lines) and `arch/aarch64/mmu.rs` (1,275), so `-D warnings` cannot see dead code in the two largest and most security-relevant files in the kernel. (2) **Suppressions whose own comments name milestones that have since shipped**, e.g. `cpu.rs`'s "by the scheduler in step 3" and `smp.rs`'s "by spawn's placement policy" (both landed as §28), `cap.rs`'s "in 9b", `interrupts.rs`'s "milestone 5's first non-test caller", and two in `mmu.rs` pointing at milestone 8's in-kernel console, which §21 moved to userspace. Each is either now-used (delete the attribute) or genuinely dead (delete the code); either way the comment is false. (3) **Superseded demo payloads** in `user.rs`, which say so themselves ("7c handed the demo over to the real ELF"). Ends with a lint gate refusing new module-wide suppressions, the same shape as the conflict-marker and roadmap checks

**Why it matters.** **a `-D warnings` gate with holes in a third of the kernel is a gate that reports success it has not earned**, which is the same class of problem as the four-times-corrected §27 record and the contradicted `fs_read` comment: the tooling said fine while nobody was looking. It also protects a real asset, since this codebase's unusually heavy commenting is only valuable while the comments are true, and a suppression citing a milestone that shipped weeks ago actively misleads. **Explicitly NOT in scope:** hardware register definitions (`gic.rs`, `timer.rs`, `semihosting.rs`, `mmu.rs` field encodings) where a complete definition is the point, and deliberate diagnostics (`VERIFY_WRITES`, `second_mount`) that encode measurements which killed hypotheses. Those keep their allows and gain a stated reason, which is the difference between a suppression and a decision

#### Built 2026-07-30. The rule is DECISIONS §38, and the ratchet is in `script/lint`.

**Re-measured on the branch point (`b9f4382`), because three lanes had landed since the sweep
below:** **83** suppressions, not 79, in three shapes rather than two. Eight were module-wide
`#![allow(...)]`, not six: `crates/socket_proto/src/lib.rs` had one the sweep missed, and **`main.rs` carried
`#![cfg_attr(target_arch = "riscv64", allow(dead_code))]`, which blindfolded the entire kernel crate
on one of two supported architectures.** That is bigger than the 5,831 lines the sweep found, and it
is the finding this milestone actually turned on.

After: **0 module-wide**, 90 conditional per-item `cfg_attr`, 15 bare per-item allows that each state
why nothing calls them in any configuration. Of the 83 triaged, **7 were deleted as dead** (plus 178
lines of retired shell wiring), **19 were simply not dead** and the attribute came off, and the rest
became a `cfg` predicate the compiler can check.

**What the un-blindfolded gate found, which is the question the milestone existed to answer: mostly
not dead code.** `sched.rs`, 3,166 lines, yielded five items. `mmu.rs`, 1,275 lines, yielded two.
That is the honest result, and it is why the ratchet matters more than the cleanup: the value was
never in the deletions, it was in learning that a third of the kernel's dead-code claims were
unchecked. Four things came out of it that a list of unused functions would not have:

1. **A parity gap on the second ISA.** `user_can_read`/`user_can_write` had no caller anywhere on
   riscv64, because the confused-deputy test is `cfg(target_arch = "aarch64")`. The check between
   U-mode and the kernel was proved on the ISA where it matters *less*: RISC-V has one root register,
   so the same tables translate user and kernel addresses and the `U` bit is the only line of
   defence. Added the twin test; riscv64 goes 114 -> 115.
2. **A false doc comment on live-looking code.** `sched::spawn_balanced` said "which is why the SMP
   balance test uses it", and the test had moved to plain `spawn` when §28 landed.
3. **A vestigial input path.** `console::rx_read` and `Ns16550::read_byte` were dead in *every*
   configuration including `--features shell`: the byte is read by the userspace input driver through
   its device capability, and milestone 20's kernel-side reader had outlived its own design.
4. **A security mechanism with no enforcement point.** Deleting `shell_service` (which main.rs
   described as "kept only as dead code for reference") left `sched::spawn_with_quota` with no
   caller, so **the kernel's spawn quota has been unenforced since §28**. Not a gap, because the
   bound moved into the untyped budget a process spawns out of, but notes/quotas.md and
   notes/security.md both still describe the counter as live. Kept, with a doc comment saying exactly
   where it stands, because removing a documented safety mechanism is a design decision rather than
   dead-code triage. **Worth a look.**

**Two gate holes closed alongside**, both the same shape as the one this milestone was chartered
against. `script/lint` linted riscv64 only under `watchdog_probe`, so the whole riscv `shell` boot
path was compiled by `xtask` and checked by nobody; the boot-mode loop now runs on both ISAs. And
fs_server, its own workspace, had only ever seen the rustdoc pass, so its code was never clippy'd at
all; adding the pass found a real `deref_addrof` in `second_mount`.

**Two premises in the scope note turned out not to hold, and are corrected here rather than quietly
worked around.** The hardware register definitions exempted as out of scope did not need exempting:
`register_structs!`/`register_bitfields!` generate code the lint does not flag, so `gic.rs` needed
one deletion and no allows. And `VERIFY_WRITES` and `second_mount` carry **no suppression at all**;
their existing prose already states the measurement, so there was nothing to give a reason to.

**Chris's question, 2026-07-30: is there dead code that should be removed?** Answered by measurement
rather than impression, and the answer is more interesting than a list of unused functions.

**The negative result first, because it is worth recording.** There are **no dead binaries**. All 28
programs in `user/` are packed into an image and reached by a test. My first sweep reported `hello` as
never packed, which was wrong: it is packed under the archive name `init` through a variable my pattern
missed. Correcting that before reporting it is the whole reason the sweep is written down here rather
than delivered as a verdict.

**The real finding: the `-D warnings` gate is blindfolded over 5,831 lines.** Six files carry
module-wide `#![allow(dead_code)]`:

| File | Lines |
|---|---|
| `kernel/src/sched.rs` | 3,166 |
| `kernel/src/arch/aarch64/mmu.rs` | 1,275 |
| `kernel/src/memory.rs` | 631 |
| `kernel/src/arch/aarch64/timer.rs` | 430 |
| `kernel/src/drivers/gic.rs` | 274 |
| `kernel/src/arch/aarch64/semihosting.rs` | 55 |

That includes the two largest and most security-relevant files in the kernel. Clippy runs with
`-D warnings` and cannot see dead code in any of them, so the gate reports success it has not earned.
This is the same class of problem as §27's four-times-corrected record, the `fs_read` doc comment that
contradicted `notes/benchmarks.md`, and the conflict markers that survived a full gate run: **the
tooling said fine because nothing was looking.**

**Second class: suppressions whose own comments cite milestones that have shipped.** Each of these is
either now-used, in which case the attribute should go, or genuinely dead, in which case the code
should. Either way the comment is false today, and false comments are expensive here specifically
because this codebase is commented far more heavily than production code on purpose. A suppression
citing a milestone that landed weeks ago actively misleads a reader who is trusting the prose.

- `kernel/src/cpu.rs:243`: "used by the tests now, and by the scheduler in step 3". Step 3 shipped as §28.
- `kernel/src/smp.rs:64`: "used by the SMP tests now, and by spawn's placement policy when it...". Also §28.
- `kernel/src/cap.rs:130`: "first used by the virtio driver setup in 9b".
- `kernel/src/arch/aarch64/interrupts.rs:63`: "milestone 5's first non-test caller".
- `kernel/src/arch/aarch64/mmu.rs:647` and `:660`: both point at milestone 8's *in-kernel* console, which §21 moved into userspace and retired.

**Third class: superseded demo payloads** in `user.rs`, which admit it in place ("`allow(dead_code)`
because 7c handed the demo over to the real ELF").

#### Deliverable

Triage all 79 suppressions; delete what is dead; convert the module-wide ones into per-item allows that
each carry a reason; and finish with a `script/lint` check that refuses a new module-wide
`#![allow(dead_code)]`, the same shape as the conflict-marker and roadmap-status gates. The point of
that last step is that this is a ratchet: without it the file-level suppression comes back the first
time someone finds it inconvenient.

#### Explicitly not in scope

- **Hardware register definitions** (`gic.rs`, `timer.rs`, `semihosting.rs`, and `mmu.rs`'s field
  encodings), where defining the complete register set is the point and using only part of it is normal.
- **Deliberate diagnostics** that encode measurements which killed hypotheses: `VERIFY_WRITES` in the FS
  server (off by default, and its comment explains that turning it on overflows the server's stack from
  RedoxFS's deep recursion) and `second_mount`, whose 30-cycle flat-heap measurement is what disproved
  the accumulated-mount-state theory.

Both keep their suppressions and gain a stated reason. That is the distinction the milestone is really
about: **a suppression with a reason is a decision, and one without is a leak.**

**Sequencing.** Independent of everything else, and a good candidate for a low-priority background lane
precisely because it touches many files shallowly and conflicts with any lane editing the same files. Do
it when no other lane is open, or accept the rebases. **Effort: 1 lane estimated**, mostly reading.

### 40. Documentation as a system service: searchable, rendered, and installed by packages

**In brief.** Markdown authored, **rendered** for display rather than shown raw, searchable locally, and installed by the package that owns it. Reuse `pulldown-cmark` for parsing (CommonMark is a fiddly spec worth taking from someone else) and write the ANSI renderer against `line_editor`'s contract, because `termimad`/`mdcat` sit on `crossterm` and assume a POSIX terminal we do not have. Phase 1 is a terminal viewer and pager, phase 2 a host-built inverted index shipped as a per-package shard, phase 3 a graphical viewer riding the display ladder. Two constraints found while scoping: **`readdir` refuses and the §27 contract has no such verb**, so nothing can walk a tree for documents, and **font rendering is still milestone 29's remaining increment**, so the terminal comes first

**Why it matters.** **the OS explains itself, on itself.** The project's whole argument is already markdown (DECISIONS, thirty-plus notes, this roadmap), so a capability-confined viewer serving them is a better milestone-23 demonstration than another synthetic test and costs the documentation nothing. The missing `readdir` turns out to be a feature: **enumeration is authority**, so indexing at package-build time is both the way around the gap and the more honest shape, which is the same answer `apropos` reached for a different reason. And `doc notes/ipc-naming.md` granting exactly one readable file is milestone 31's designation-is-authorization made into something a person uses

**Chris's direction, 2026-07-30.** Markdown as the authored format, rendered for display rather than
shown raw, searchable on the local machine, and installed *by the package that owns it*, so a
component brings its documentation with it.

**Why this belongs on a demonstrator's roadmap rather than being a nicety.** The project's own
argument is written in markdown: `DECISIONS.md`, thirty-plus notes, this roadmap. A cricker-os that
serves its own design notes, on itself, through a capability-confined viewer, is a better
demonstration of milestone 23's component story than another synthetic test, and it costs the
documentation nothing because it already exists. It is also the first *application* on the display
ladder that anybody would actually use.

#### Two constraints found while scoping, both real

1. **There is no directory iteration.** `readdir` refuses in the std PAL and the §27 file contract has
   no such verb, so nothing can walk a tree looking for documents. Adding one is a decision, not a
   detail, and **the capability model argues against it anyway: enumeration is authority.** A viewer
   that can list a directory can discover what it was not given. So the design below indexes at
   *package build time* and ships the index, which sidesteps the missing verb and is the more honest
   shape. Unix reached the same answer for a different reason: `apropos` reads a prebuilt `mandb`
   because scanning was slow.
2. ~~**There is no font rendering yet.**~~ **There is now** (milestone 29, 2026-07-30): a bitmap
   font, a VT engine, and a display terminal that is a compositor client. A *graphical* documentation
   browser is therefore unblocked in principle, though the honest limits still argue for the terminal
   first: a 16x8 grid, no scrollback, and no UTF-8 (notes/glyphs.md).

#### Reuse: take the parser, write the renderer

CommonMark is a fiddly specification with a large conformance suite, and parsing it is exactly the
kind of work worth taking from someone else. Rendering to *our* terminal contract is ours and small.
That split is the reuse judgment, and it is the same one milestone 32 made about RedoxFS.

| Piece | Option | Judgment |
|---|---|---|
| Parse | **`pulldown-cmark`** (pure Rust, CommonMark, event-stream API, few dependencies) | **Take it.** The event stream is the right shape for a renderer that emits ANSI. Milestone 27's `std` is what makes this buildable at all. |
| Parse | `comrak` (GFM: tables, strikethrough, footnotes) | Consider later if GFM tables matter; more dependencies. |
| Render | `termimad`, `mdcat` | **Do not take.** Both sit on `crossterm`, which assumes a POSIX terminal (termios, ioctl). Porting that is more work than emitting ANSI against `line_editor`'s contract, which we own and already speak (§21). |
| Search | `tantivy` | **Too heavy.** It assumes a filesystem and mmap. |
| Search | A host-built inverted index shipped in the package | **Take this shape.** Built by `xtask` where there are no constraints, merged by the viewer across installed packages. |
| UI | `ratatui` | Possible for a pager later; needs a backend against our terminal contract first. |

#### Shape

- **A doc bundle is part of a package**: rendered-source markdown plus a small index shard, installed
  into a documentation store when the component is installed. This is where milestone 39's packaging
  observation pays: manifest, hash, version, and now a doc bundle.
- **The viewer holds a directory capability to the doc store** and nothing else. It cannot read the
  rest of the filesystem, which is the point, and it does not need to because the index tells it what
  exists.
- **The index is a merge of shards**, one per installed package, so installing a component makes its
  documentation searchable without a reindex pass and without any component being able to see
  another's files.
- **`doc search <term>`** and **`doc view <topic>`**, shell verbs. Milestone 31's grant expression
  makes this a demonstration rather than a convenience: `doc notes/ipc-naming.md` passes exactly one
  readable file capability, and a viewer invoked with no argument can read nothing.

#### Phasing

- **Phase 1, the terminal viewer.** `pulldown-cmark` to an ANSI renderer over `line_editor`'s contract:
  headings, emphasis, lists, block quotes, code blocks, and a pager. Works on the serial console
  today and inherits the display terminal for free when 29's glyph work lands. Host-tested in
  milliseconds like every other pure-logic piece: markdown in, styled bytes out.
- **Phase 2, search.** The host-built index, the shard merge, and `doc search`.
- **Phase 3, the graphical viewer.** Rides the display ladder: needs 29's font rendering and sits as a
  client of 33's compositor. Rung three of the ladder is where this becomes a real application.

**Prior art worth reading:** `man` plus `apropos` plus `mandb` for the split between format, index and
pager, which is the architecture this proposes minus the troff. Dash/Zeal *docsets* (a bundle with its
own index) for the packaging shape. `cargo doc`'s HTML output as the road not taken, since HTML would
need a browser engine, which is a mountain with no thesis behind it.

**Sequencing.** Phase 1 wants milestone 31 phase 2 finished (per-file grants make `doc <file>` the
demonstration it should be) and nothing else; it can precede the packaging work and be wired into it
later. **Effort: 1 lane estimated per phase**, three phases, and they can land separately.

### 39. Repository structure for a loosely-coupled OS, and the road to a distribution

**Prior art to read before designing packaging:** `design/haiku-bfs-and-packages.md`. Haiku's `packagefs`
**activates** packages rather than installing them, composing the filesystem view from a set of read-only
package files instead of letting installers mutate shared directories. It reached a shape close to milestone
47's conclusion that **installing a program is granting it into a namespace**, from an entirely different
motive (atomic, rollback-able installs), which is the useful kind of convergence.

**In brief.** **Analysis recorded, no decision taken.** The tree is a monorepo for a deliberately loosely-coupled system, and it is straining in measurable ways: `user/` is 28 binaries and 9,324 lines in one crate that is also a shared library, `fs_server/` has already escaped into its own workspace for real dependency reasons, `crates/` conflates kernel proof crates with wire contracts and userspace runtime so the boundary a third party cares about is invisible, and every crate is version 0.1.0. Four options are written up with their trade-offs (restructure in place; multiple workspaces in one repo; split repos; monorepo plus a later distribution *manifest* repo), along with a naming argument (**components** and **services**, never "daemons", because a Unix daemon is defined by the ambient authority this OS does not have) and the observation that milestone 31's program manifest plus §22's measured-boot hashing are already three quarters of a package format

**Why it matters.** **the structure has to serve the thesis, and one constraint dominates.** A single `script/test` proving the whole system on both ISAs is this project's credibility mechanism and what makes rule 5 a gate rather than an aspiration; splitting repos trades that for decoupling nothing external needs yet. Recommendation recorded (monorepo now, distribution as a separate manifest repo, executed as multiple workspaces, not before 23 forces it) so the eventual decision starts from evidence rather than from taste

**Status: analysis recorded, NO DECISION TAKEN (2026-07-30, Chris's request).** Deliberately a
roadmap milestone rather than a `DECISIONS.md` section, because nothing was decided; §-sections are
for decisions, and recording an undecided question as one would be a lie about its status. This
block exists so the analysis is not lost and so the eventual decision starts from evidence.

The question Chris raised: cricker-os is a monorepo for a microkernel, but it is a collection of
deliberately loosely-coupled things, and the structure may not support that long term. Plus a
naming question (should the userspace servers be "services" or "daemons"), and the observation that
a Linux-distribution-shaped layer will eventually sit on top of the OS components.

#### Where the current structure is straining, measured rather than felt

- **`user/` is one crate doing two incompatible jobs.** 28 binaries, 34 files, 9,324 lines. It is
  also a library: `net_transport` and `socket_test_client` are shared modules sitting
  beside the programs that consume them. So no component can express "I need the virtio driver bits
  but not the network stack", every component rebuilds when any shared module changes, and no
  component can take a dependency without handing it to all 28.
- **One component has already escaped, for real reasons.** `fs_server/` is its own workspace with its
  own `Cargo.lock`, because RedoxFS's default features pull `fuser` (whose build script panics on
  macOS) and its core wants `std` under test. Milestone 36 did the same to the toolchain by requiring
  a cross-capable clang. Two instances is a pattern: the first components with genuine dependency
  needs of their own had to leave.
- **`crates/` conflates three audiences with different rules**, so the boundary a third party would
  care about is invisible: kernel proof crates (`capability`, `paging`, `frames`, `regions`, `slots`,
  `asid`, `intrusive`, `dtb`, `elf`, `dma_validator`, `measured_boot`, `user_heap`, Kani-proved and nobody
  else's business), wire contracts (`fs_proto`, `gfx_proto`, `line_editor`, `compositor`, `abi`, the
  **only** things an external component needs), and userspace runtime (`user_rt`, `grant_plan`,
  `crickerfs`, `pci`).
- **Every crate is `version = "0.1.0"`.** Correct for internal crates, fatal for a published
  contract, and contracts are exactly what milestone 23's live replacement makes into a compatibility
  surface.
- **Not everything in `user/` is a service.** `heeder`, `spinner`, `flaky`, `allocator_exerciser`, `worker`,
  `builder`, `coremark`, `os_primitives_benchmarker` are fixtures and benchmarks. Mixing them with `net_stack`, `display`,
  `compositor` and `line_editor` is much of why the directory reads as shapeless.

#### Naming: components and services, not daemons

A technical objection rather than an aesthetic one. A Unix **daemon** is defined by what it detaches
from: no controlling terminal, inherited ambient authority, a pid file, started by init holding the
system's privilege. Every one of those is something this OS deliberately does not have, and
importing the word imports the model. The project has already had to push back on exactly that pull
(§27's "open-by-path exists only inside the server", §24's "Ctrl-C is a capability, not a signal").

The project's own word already carries the thesis: milestone 23 is "a capability-routed **component**
OS with live replacement", "a vendor-shippable unit behind a stable contract". Proposed vocabulary,
matching what DECISIONS already says:

- a **component** is the shippable unit, a binary plus its manifest (`components/`);
- a **service** is what it offers over a contract ("the FS service");
- a **contract** is the wire protocol (`contracts/`).

"Server" stays a fine role word inside a component (`fs_server` serves the FS service). "Daemon" gets
dropped.

#### The four options

| | Shape | Buys | Costs |
|---|---|---|---|
| **A** | One workspace, restructured directories (`kernel/`, `components/`, `contracts/`, `runtime/`, `fixtures/`, `tools/`) | Legibility, cheapest | Does not fix per-component dependencies unless each component also becomes its own crate, which is the actual work |
| **B** | One repo, multiple workspaces (generalize what `fs_server/` already does, driven by `xtask --manifest-path`) | Real dependency isolation; a component can use `std` or a foreign toolchain without infecting the kernel build | More lock files, slower cold builds, more complex xtask |
| **C** | Split repos: kernel, components, distribution | Maximum decoupling; what an ecosystem with third-party components looks like | **The integration gate**, see below |
| **D** | Monorepo now; distribution as a separate *manifest* repo later | Keeps the gate; distro consumes released artifacts, the way Yocto, Buildroot and Alpine aports separate recipes from sources | Defers the decoupling question rather than answering it |

#### The constraint that decides it

**The single-command gate across both ISAs is the project's credibility mechanism.** One `script/test`
boots the kernel and proves the whole system on aarch64 and riscv64, including every component's
confinement, and rule 5 (DECISIONS §19) says parity is a gate and not an aspiration. Split into
separate repos and that becomes a multi-repo CI problem where the integration proof either lives
somewhere awkward or quietly stops running on every change. For a demonstrator whose entire argument
is "measured, both architectures, same suite", that is an expensive thing to trade for directory
cleanliness *before any external party needs it*.

**Recommendation: D, executed as B, and not before milestone 23 forces it.**

#### The packaging observation worth acting on early

Most of a package format already exists and is not called one. Milestone 31's per-program **manifest**
(SHILL-adapted: declared endowment, checked at spawn) is package metadata. §22's measured boot already
hashes a component against a trust root. A distribution needs manifest, hash, version, and contract
version; three of those four exist. Naming that as the packaging layer would make the distribution an
assembly step rather than a new subsystem, and would give the contracts a reason to carry real version
numbers, which is what lets components evolve independently at all.

#### Publishing crates is a different question from splitting the repo (Chris, 2026-07-31)

Chris asked whether the crates should get their own repos and builds, since some are useful outside
cricker-os. **Two decisions hide in that, and only one is expensive.**

`cargo publish -p calendar` works from a workspace. The thirty crates with no external dependencies
have no path dependencies to strip either, so **crates can be handed to the world without touching
the repo structure at all.** Splitting into separate repositories is the costly move, and the
constraint above already rules on it: a single `script/test` proving the whole system is what makes
parity a gate, and split repos let a crate be green in isolation while broken in the OS. That is the
exact failure rule 5 exists to catch, plus cross-repo changes become multi-PR dances.

**How many are actually useful outside is fewer than it feels: two to four of about thirty.**

| Crate | External value |
|---|---|
| `gpt` | **Real.** `no_std`, zero I/O, 8 machine-checked proofs. crates.io's `gpt` uses `std::io`, so an I/O-free proof-carrying one is a genuine gap |
| `calendar` | **Real**, competing with `time` and `chrono`; the differentiator is the proofs plus strict `no_std` |
| `dtb`, `pci` | Plausible, though `fdt` already occupies much of that space |
| `ntp_proto` | Overlaps heavily with ntpd-rs's mature `ntp-proto` |
| Everything else | Bound to our kernel model (`capability`, `slots`, `frames`, `regions`, `ipc`, `paging`, `asid`) or specific to us (`crickerfs`, `grant_plan`, `dma_validator`) |

**The argument for publishing is a thesis argument, not a utility one**, and that is the version worth
acting on. A `no_std`, I/O-free GPT parser carrying eight machine-checked proofs is a **publishable
artifact**, and it is stronger evidence for the verified-Rust claim than any write-up, because a
stranger can run the harnesses. Same for `calendar` over all 3,652,425 days in range. Publishing then
is not about sharing utilities; it is about putting the verification claim somewhere it can be checked
by people with no stake in agreeing with us.

**The trigger: wait until each crate has a real in-tree consumer.** `calendar` shipped 2026-07-30 and
`date` does not exist yet; `gpt` has no caller at all. **An API that has never had a consumer is not
ready to publish**, because the first real caller always finds the shape wrong, and after publication
that costs a semver break rather than an edit. So `date` exercises `calendar`, `mkfs`/partitioning
exercises `gpt`, the NTP client exercises `ntp_proto`: publish after, not before.

**Two frictions to know now.** The crates.io names `gpt` and `calendar` are almost certainly taken
(worth checking rather than assuming), so they would need prefixing, which quietly weakens the
generally-useful-library pitch. And publishing a proof-carrying crate is a **promise to strangers that
the proofs keep passing across Kani versions**, which is a maintenance obligation §14 does not ask
for: we are a demonstrator, not a library vendor. `design/capsicum-and-the-retrofit-question.md`
records CloudABI dying partly of maintenance rather than of being wrong.

**Recommendation, not a decision:** keep the monorepo, publish selectively once each crate has an
in-tree consumer, and treat publication as evidence rather than as a product line.

#### The cheap first move, which commits to none of the four

**Split `user/` three ways**: `components/` for the services, `fixtures/` for the test programs, and
lift `virtio`, `net_transport`, `socket_proto`, `suptree` into `runtime/` crates. That ends the
crate-is-both-a-program-collection-and-a-library problem, makes dependencies expressible, and leaves
the gate untouched.

**Whichever option is chosen, do the move as one mechanical commit with the pairing audited.**
Renaming directories touches `xtask`'s `--bin` lists and the initrd packing, and a union merge in
exactly that code dropped a `--bin` flag on 2026-07-29 and duplicated a loop header the same day. It
must not be folded into feature work.

### 24. A second aarch64 *board*: Virtualization.framework (optional)

**In brief.** Boot under Apple's Virtualization.framework, not QEMU's `virt`: a virtio-console driver (VZ has no PL011), VZ's interrupt/memory layout and boot handoff, device discovery through the machine VZ presents

**Why it matters.** proves the `arch/` **board** boundary on a second machine of the *same* ISA (cheaper than 16's silicon, distinct from 20's second ISA), and lets cricker-os run under the same VMM as macOS/Linux guests. Optional; portability exercise, **not** a benchmarking prerequisite (guest-internal microbenchmarks are VMM-independent)

### 25. Cross-OS performance comparison (extends 21)

**In brief.** EL0-measured primitive benchmarks (syscall, context switch, IPC, map, spawn) the lmbench way, so the numbers include the trap the kernel-side benchmarks skip; then line them up against lmbench (Linux, macOS guests) and `sel4bench` (seL4), at a matched virtualization tier, with release builds. Fold in the icount codegen-sensitivity fix.

**Why it matters.** **turns perf claims into cross-OS numbers**: where does a Rust capability microkernel stand next to Linux, macOS's XNU, and seL4 on the primitives that define an OS. **Largely done**: four EL0 primitives (null syscall, context switch, IPC, page map) on both instruments, a release build path, and the three-way comparison (cricker-os vs Linux-under-HVF vs native macOS) with cricker-os winning null/IPC ~5x. `spawn` landed too (its real prerequisite was never retype, which had already shipped, but **object revocation**, reclaiming a child's TCB/aspace/endpoint so a spawn loop can repeat; that shipped as its own milestone, notes/object-revocation.md, and the EL0 `lat_proc` bench, `spawn_el0`, is in the suite and the committed baseline). **Remaining**: only `sel4bench` (built and booting for qemu-arm-virt, but it times single ops via the PMU cycle counter, which neither QEMU-TCG nor Apple HVF provides, so it is **deferred to real hardware**, the milestone-16 machine, which has a real PMU; this validates our CNTVCT + long-loop design). notes/benchmarks.md

### 26. Object revocation: tear a process back down

**In brief.** Reclaim the TCBs, address spaces, and endpoints a process built, and the regions behind them, so a workload that comes and goes can leave. **Built:** region-ownership + generational staleness (no CDT), `Untyped::SPLIT`/`DESTROY`, generational region slots (retires the 256-lifetime cap), endpoints (safe subset). Extends §13 from frames to objects; DECISIONS §16, notes/object-revocation.md

**Why it matters.** **the teardown half of "run real workloads":** a process can be reaped, not just built

### 33. A compositor: one screen, mutually distrusting clients

**In brief.** **Built (2026-07-29), both ISAs**, rung two of the display ladder: `compositor` multiplexing one screen among three clients, each holding a capability to its own surface; software composition honouring a damage rectangle; input routed by capability over the terminal contract's `OP_BYTES`; enumeration and screenshots as read-only mappings rather than verbs. No new syscall and no new method. notes/compositor.md, DECISIONS §33

**Why it matters.** **the canonical multiplexer of one device among distrusting clients**, and the thesis at its sharpest: a client is *proved* unable to reach its neighbour's pixels even when handed the exact address of them, and the compositor holds no authorization code because the authority is a mapping rather than a message. It also found the kernel's one missing primitive (no wait-any), recorded as a fork

### 34. GPU acceleration via virtio-gpu 3D (the display ladder's rung four)

**In brief.** The **Venus** path: Vulkan commands serialized over the virtio-gpu device, arriving on the §18 PCIe transport, so the guest gets real GPU acceleration without owning a hardware driver. Needs the 3D context and command-submission side of virtio-gpu that rung one deliberately left alone (rung one sets up no cursor queue and no 3D context, keeping the §23 two-queue ceiling untouched), the confinement story extended to command-carried backing addresses (DECISIONS §30's residual gap: those are the addresses the descriptor validator structurally cannot see, and today only an IOMMU stops them), and something to consume it, which is what would give `wgpu` a real target

**Why it matters.** **how every VM gets a GPU without a hardware driver**, and the honest ceiling on the display ladder: rung five (a bare-metal driver for the VisionFive 2's BXE-4-32 3D core) is struck as a Linux-scale multi-year effort that proves nothing this does not. A mountain, priced as such, and it reopens the parked competitor question the ladder's governance note names as the architect's call

### 37. Prove RedoxFS's crash consistency (DECISIONS §34, condition 1)

**Built 2026-07-30, both ISAs. §34's condition 1 is met, and the claim it earns is narrower and
sharper than the one it replaces** (DECISIONS §34's amendment carries the full statement).

What is measured, on the host, exhaustively: 93 fault points across a seven-operation workload, each
one a power cut with the process gone and the recovery a fresh mount. Every one recovers a state that
**really existed**, the prefix never goes backwards as the cut advances, and at the last cut point
nothing is lost. The same sweep with the interrupted write **torn** at four offsets: 372 points, same
result. A separate sweep models a device that *lies* (acknowledges a write it never persists, then
carries on): 186 damages, 112 recovered, 74 refused at the mount or the read, and **zero silently
wrong**, which is the honest limit and the honest guarantee in one number.

The controls, which is what makes the rest mean anything: with the header ring's older generations
removed, **92 of the 93 fault points do not mount at all**; a commit torn at 2048 bytes fails
`Header::valid()` while the previous generation's slot stays valid and older. And the injector caught
a bug in the *harness* first, which is the best evidence it bites: nine fault points looked like
filesystems that never existed until it turned out `snapshot` was reading `EIO` as "the name is
absent".

On device, on its own disk on both ISAs: the FS server is killed one block write into its second
transaction, with that block torn in half by a real virtio write, and a **different FS-server
process** mounts what it left behind through the same block server and reads the file back whole.

**In brief.** Inject the failure a copy-on-write filesystem exists to survive, and measure whether it does: torn writes (a block partially written), dropped writes (a write the device acknowledged and did not persist), and a kill mid-transaction, then reopen with the same `cleanup: true` header-ring replay the FS server always mounts with, and assert the filesystem is consistent and every acknowledged write is either wholly present or wholly absent. The seam is `IpcDisk` and the block server, which sit between the engine and the device and can drop or truncate a write deliberately; the sans-IO core already runs on the host against a real image, so most of this is host-testable in milliseconds and only the device-level kill needs QEMU. Includes the negative control that makes the rest mean anything: the injector must be shown to actually corrupt something when the replay is disabled

**Why it matters.** **the condition that decides whether §34's label is earned.** Crash consistency is RedoxFS's central selling point and the reason it beat ext2, and we currently assert it on the strength of the upstream design description rather than any measurement. That is a claim of exactly the kind this project's rules forbid, and it is the first thing a skeptic asks a filesystem. Until it passes, the docs say "designed for crash consistency" and never "crash consistent". Note this is a gap in **our harness, not in RedoxFS**: no candidate engine's crash consistency is tested here, so switching engines would not address it

### 38. Filesystem throughput, and the comparison (DECISIONS §34, condition 2; extends 21/25)

**In brief.** Sequential and random read/write throughput through the confined FS server, against ext4 on Linux and APFS on macOS at a matched virtualization tier, the way milestone 25 did the primitives. Requires deciding what is honestly comparable: our reads are device-latency-dominated (`fs_read` is ~204 us/read under HVF, and `relay_rtt` puts the isolation tax a thousand times below that), so the interesting question is whether the userspace-server architecture costs throughput once the device dominates, which is a claim a microkernel skeptic will press

**Why it matters.** **"primary filesystem" invites a comparison we cannot currently make.** We have the per-request numbers and the isolation tax, and no MB/s figure at all. Milestone 21's rule is measure rather than argue, and 25 already established that the honest way to do this is EL0-measured against real systems rather than self-reported. This is where the "userspace servers are too slow" objection gets an answer or a concession

### 42. Supply chain and fuzzing in CI (extends the 2026-07-30 CI audit)

**Two of three legs built 2026-07-30; the decisions are DECISIONS §36.** Advisories and licences:
`deny.toml` (written rather than defaulted, a reason next to every knob) run over each workspace by
`script/supply-chain`, in CI. First run found no advisories, no yanked crates and no unknown sources,
which is a result rather than a null because it is the first time anyone could say it; plus one
duplicate (`getrandom`, host-side, under redoxfs, skipped with a reason and an expiry condition),
three licences beyond MIT/Apache-2.0 that are genuinely needed, and two crates that needed
`publish = false` before a path dependency could be told apart from `version = "*"`.

Vendored integrity: `script/vendor-verify` hashes the published .crate, applies a committed
divergence patch with zero fuzz, and requires byte identity with the tracked tree. **It found drift
on its first run**, which is the argument for it: vendor/README.md claimed the published redoxfs
package ships no `Cargo.lock` and that ours was a deliberate addition. It ships one, and ours was a
regeneration that had re-resolved 25 dependencies. Nobody had edited the filesystem, and nobody could
have proved that either.

**The fuzzing leg landed 2026-08-02** (notes/fuzzing.md). Four cargo-fuzz targets over the parsers
that read bytes from outside the trust boundary (`dtb`, `elf`, `gpt`, and a `crickerfs` round trip),
run by `script/fuzz` and by a CI job of its own with a **sixty-second-per-target budget**, because
fuzzing has no completion condition and so cannot be a step inside a gate anyone waits on.

**Three bugs, and how each was found is the finding.** `dtb::Region::end` overflowed on a hostile
memory map, which the kernel's boot path calls on every RAM region: **the fuzzer found that one**, in
ten minutes, from a mutated copy of the committed QEMU device tree. `crickerfs::write_image` accepted
a name containing a NUL, wrote it, and could never read it back, the same silent-collision family as
the truncation bug fixed on 2026-08-01: **a round-trip property found that one**, in under a minute,
and no totality proof could have, because nothing panicked. And `dtb::node_reg` indexed past its
16-entry cell stack on a tree nested 17 deep: **reading the code found that one, and ten minutes of
fuzzing did not rediscover it**, because deep recursive structure is what a mutational fuzzer is
worst at synthesizing. All three are fixed and pinned by host tests that run in milliseconds.

The question the leg was held open for is answered in the note's first section: Kani is exhaustive
inside a bound and a fuzzer is unbounded and random, and the three cases above show the boundary
between them is not always the bound. Sometimes it is that nobody wrote the property down.

**In brief.** Three things CI does not do. **Advisories and licences**: no `cargo-audit`/`cargo-deny`, so a published advisory against a dependency is invisible, and licence obligations go unrecorded, which stops being cosmetic the moment milestone 39's distribution exists. **Vendored integrity**: `vendor/redoxfs` is pinned at 0.9.1 with a `patches/` discipline and *nothing verifies the tree equals upstream-plus-our-patches*. **Fuzzing the parse surface**: Kani proves `elf`, `dtb` and `crickerfs` under *chosen bounds*, and a fuzzer explores byte sequences past those bounds and finds panics rather than property violations, which is complementary rather than redundant. Several crates are unproved entirely and take attacker-shaped input: the `fs_proto`/`gfx_proto`/`line_editor` decoders, `grant_plan` (which parses the human's command line), `compositor` (clipping arithmetic, where its own note says off-by-one is the classic bug), and `measured_boot`, the SHA-256 behind the measured-boot trust root

**Why it matters.** **the thesis is confining code we did not write, so not knowing when that code has a published advisory is an odd blind spot**, and milestone 32's flagship claim ("a real filesystem we did not write") is only as good as our ability to say what we are actually running. Fuzzing is the honest complement to bounded model checking: Kani answers "is the property true inside these bounds", a fuzzer answers "does anything crash outside them", and the project currently only asks the first

### 43. A second security audit, with a different lens

**In brief.** The first audit (notes/arch-audit.md) read the **assembly and arch layer** and found three real bugs: the `eret`/`sret` privilege-escalation staging race, a stale `tp` on S-mode trap return corrupting cross-hart per-CPU data, and the PLIC's lock-free read-modify-write. A second pass should deliberately NOT re-read that, and should take the surface that has appeared since. Headline lens: **time-of-check to time-of-use across shared pages.** Every service contract now moves bulk data through a page shared with the client (blk, file, gfx, compose, line_editor, net_stack), so a server that validates a length or an offset from the request word and *then* reads the page has a double-fetch window a malicious client controls; 19 files touch that pattern. Further lenses: integer overflow in the wire's size and offset arithmetic (`fs_proto` packs a 40-bit length, and `TRUNCATE` takes a size in the second word); capability lifetime races between revocation and an in-flight use, now that generational names, `Untyped::DESTROY` and `Endpoint::REAP` all reclaim; and a census of the **804** `unsafe` occurrences, triaging which carry a stated safety argument

**Why it matters.** **the attack surface roughly doubled after the first audit was written**: the compositor's shared surfaces, the C seam, the reap right, `std::fs`/`std::net`, and the FS service all arrived afterwards. The first audit's value came from reading for a *pattern* rather than waiting for a failure (it found the PLIC race that way), so the return on a second pass depends entirely on choosing a lens the first one did not use. Double-fetch is that lens: it is invisible to every gate we run, because both the check and the use are individually correct

### 44. GitHub repository hardening: policy, private reporting, code scanning, pull requests

**The committable half is built 2026-07-30 (DECISIONS §36); the settings half is written down and
waiting on an admin (notes/repo-hardening.md).** `SECURITY.md` states the scope at confinement, with
the distinction that carries the weight: a missing feature on this roadmap is a roadmap item, a
defence that is *claimed* and does not work is a vulnerability.

**Code scanning: checked rather than assumed, and the answer was no.** The obvious argument for an
advanced (committed-workflow) setup is that it would see more of the tree; the extraction log says
otherwise, because default setup finds all five cargo workspaces by itself and reports 176 of 176
Rust files scanned. The number worth carrying forward is the other one: **60 of those 176 were
extracted with errors**, against the *host* target with default features, for a kernel that does not
build for the host at all. "Zero alerts" means less than it looks, and that belongs next to the claim
rather than in a footnote.

**Waiting on Chris**, both in notes/repo-hardening.md with exact steps: enable private vulnerability
reporting (the committed `SECURITY.md` currently points at a button that does not exist), and apply
the `main` ruleset with seven required checks, an empty bypass list, and *not* linear history. Apply
the ruleset only after this branch merges, because one required check does not exist yet and a
required check that never reports blocks every merge.

**In brief.** Four items, and they split into files we can commit and settings someone with admin has to toggle. **Files:** a `SECURITY.md` policy stating what is in scope (the kernel's confinement boundaries) and what is not (a demonstrator running under QEMU is not a production system), and a code-scanning workflow. **Settings:** private vulnerability reporting, and a ruleset requiring pull requests into `main`. Note the plumbing for the last one already exists, since CI runs on `pull_request`; what is missing is the branch protection that makes it mandatory. One thing to check rather than assume: **CodeQL's Rust support** has been moving through preview, so confirm its current state before committing to it; if it is not ready, the practical scanners are the clippy gate we already run, `cargo-audit`/`cargo-deny` from milestone 42, and a SARIF upload from whatever does work

**Why it matters.** **a public repository with a security thesis should be able to receive a security report privately**, which today it cannot. The pull-request item also changes how this project is built: work currently lands by merging feature branches into `main` locally, and requiring PRs would put every merge behind the same gate rather than trusting the person merging, which is the discipline that caught the reap flake and the conflict markers only because I happened to run the gates by hand

### 45. Triage the CodeQL code-scanning alerts, and decide what the tool is for

**Built 2026-07-30. All nine alerts are fixed, and the policy is DECISIONS §35.** The seven
`actions/missing-workflow-permissions` went first: every CI job held a `GITHUB_TOKEN` with permissions
it never used, which is an odd default for a project whose thesis is that a component holds the
authority its job needs and nothing more.

The two `rust/access-invalid-pointer` alerts turned out to be two different findings wearing one label.
**Nullness was structurally fixable and the type was failing to say so**: every pointer entering the
intrusive queues comes from a `&mut Thread`, so non-nullness is a fact of construction rather than a
caller's promise. `Fifo`, `Endpoint` and the `Node` trait moved to `NonNull`, every conversion at every
call site is infallible (`NonNull::from`, never `NonNull::new(..).unwrap()`), and `Option<NonNull<T>>`
is the same size as `*mut T` through the niche, so it costs nothing. **Validity and aliasing remain
inexpressible**, which is the design of an intrusive queue rather than a gap, and that reasoning now
lives in the crate's own docs as the standing caveat.

Two things recorded because they were wrong or nearly so. I predicted twice that `NonNull` would improve
the code *without* satisfying CodeQL, reasoning that the rule was about validity generally; it cleared
both alerts, so the rule was more precise than I credited. And I first "proved" that with a query
against `refs/heads/<branch>`, which has **zero analyses**, so it would have returned zero whatever the
code did. The real comparison is `/language:rust`: 2 results on `refs/heads/main`, 0 on
`refs/pull/5/head`, holding across four commits each side.


**In brief.** Nine alerts on first run. Seven (`actions/missing-workflow-permissions`) were fixed immediately by giving every workflow an explicit least-privilege `permissions: contents: read`, which is the right call for this repo specifically: a project whose thesis is that a component holds the authority its job needs and nothing more has no business letting its CI token default to write access it never uses. **The two that remain are high severity and need judgement, not configuration**: `rust/access-invalid-pointer` at `crates/intrusive/src/lib.rs:93` and `:109`, the raw-pointer dereferences in the intrusive wait-queue's `push_back` and `pop_front`. Both already carry `SAFETY` comments citing the queue's caller contract, and `intrusive` is one of the 13 Kani-proved crates, so the question is precisely what CodeQL sees that Kani does not: Kani proves the pure logic under chosen bounds, while the pointer validity here rests on a *caller* contract enforced by convention rather than by the type system. Decide per alert whether it is a true positive worth restructuring for, or a false positive to dismiss **with a written reason**; then set the standing policy for how alerts get triaged, since an alert list nobody dispositions decays into wallpaper

**Why it matters.** **the alerts land exactly where this project's most-used unsafe abstraction lives**, so the answer is worth having either way: either the wait queue's contract can be made structural rather than documented, which is a real improvement to the code every blocked thread passes through, or we write down why it cannot be and what upholds it instead. Also forces the meta-decision milestone 44 left open, now that scanning is actually running: a scanner whose findings are never dispositioned is worse than none, because it manufactures the appearance of review

### 46. Rename the components for what they are, and write down the naming rules

**Built 2026-07-30, both ISAs.** Five renames in one mechanical commit: `netd` → `net_stack`,
`compd` → `compositor`, `gpud` → `display`, `termd` → `line_editor`, and the crate `crates/linedisc` →
`crates/line_editor`. The scope estimated here at 398 came in at **457 whole-word token replacements
across 4 file moves and 1 directory move** (`netd` 184, `linedisc` 93, `termd` 77, `gpud` 67,
`compd` 36); the estimate was measured before milestones 23 and 37 landed and the tree grew under it,
which is the ordinary way a count like this drifts. The conventions are notes/naming.md, indexed in
notes/README.md, and four of them are checked in `script/lint`: no name ending in `-d`, the word
"daemon" nowhere outside the documents that argue about it, one spelling for contract crates, and a
recognised branch prefix. Each was proved to fail before it was trusted, and the strongest of those
controls is that the `-d` check run against unmodified `main` reports exactly `compd gpud netd
termd`.

**Why it matters.** The rule and its argument are DECISIONS §39. The short version: a `-d` suffix
tells every reader "this is a daemon" before they see a line of code, and a Unix daemon is defined by
the ambient authority this OS deliberately lacks. `netd` holds five explicit capabilities, cannot name
its own callers, is supervised, and can be reaped by something that lacks the authority to build it.
The name is a false claim, which is the same defect as a stale comment except that every reader is
guaranteed to read it. `linedisc` failed the second half of the same test: it is the correct Unix term
of art, and the person who built this system did not recognise it.

**Execution discipline, because this is the change milestone 39 warns about.** One commit, nothing
else in it. **Whole-word tokens only**: `display` and `compositor` already appear as ordinary English
throughout the notes, so this replaces identifiers, not vocabulary. Count the `--bin` name/token
pairing before and after: this is the same `xtask` code where a union merge dropped a `--bin` flag on
2026-07-29 and where git silently duplicated a loop header. Then zero surviving references to any old
name, and the full gates on both ISAs. `script/lint`'s script-documentation check plus the roadmap and
decisions checkers catch prose stragglers.

**Sequencing (the reason this is a milestone and not an afternoon).** It must land *after* milestones
23 and 37, because 23's instance one is the console hot-swap and it is editing `termd`: the file this
renames away, plus `kernel/src/user.rs`, which both lanes share. Landing 398 token replacements
underneath an active branch turns a mechanical commit into a merge fight, which is precisely what it
must never become.

#### Second half: the conventions, and checks for the ones a machine can check

Looking for the tree's naming conventions on 2026-07-30 turned up three real inconsistencies, none of
them anybody's decision:

- **Word separation in crate names is split down the middle.** `fs_proto`, `gfx_proto`,
  `dma_validator`, `user_rt` use underscores; `grant_plan`, `crickerfs`, `bitfont`, `line_editor`, `coremark`
  run the words together. Two habits, no rule.
- **The wire contract is spelled four ways**: `fs_proto` and `gfx_proto` (crates, underscore),
  `socket_proto` (a module, no underscore), and `line_editor::proto` (a submodule). One concept.
- **Branch prefixes contain a literal duplicate**: eight in use, including both `feature/` and `feat/`.

Write the *principle* in prose, because it needs judgement and no checker can evaluate it: name a
component for what it is, and prefer a word that parses without prior Unix exposure. DECISIONS §39
already carries the reasoning; the note should point at it rather than restate it.

Then **check the mechanical ones in `script/lint`**, because this project's own pattern is that a
convention which matters gets a checker rather than a paragraph: the roadmap status vocabulary,
DECISIONS numbering, script documentation, conflict markers and module-wide suppressions all became
checks today, and a rule with no enforcement decays (which is the entire argument the dead-code
ratchet was built on):

- **No `-d` suffix on a binary.** §39 made this a rule; without a check it lasts until the first
  inconvenient moment.
- **One spelling for contract crates.** Pick `*_proto` or `*proto` and fail the odd one out.
- **Branch prefixes from a fixed set**, which retires `feat/` versus `feature/`.

The note lands in `notes/` and is indexed in `notes/README.md`; `script/lint` already enforces that
every script has an entry in `notes/scripts.md`, so the precedent for gating documentation exists.

**Why both halves are one lane.** They share a landing point: the note must describe `net_stack`,
`compositor`, `display` and `line_editor` rather than names that are about to change, and the `-d` check
would fail until the rename lands. Splitting them would mean writing documentation that is stale on
arrival, or a checker that is red on arrival.

**Effort: 1 lane estimated**, almost entirely verification rather than editing.

### 47. Navigation and naming: `cd`, `pwd`, `ls`, `mkdir`, `rm`, paths, and environment

**In brief.** A navigation model for a system with no global namespace. Keep the Unix command names
and behaviour wherever they can work honestly; diverge only where the capability model forces it, and
say why each divergence is earned. **The keystone is built** (the directory capability and its
six-rung rights ladder, DECISIONS §47, notes/dir-capability.md), and so are **the five commands, on
both ISAs**: `cd`, `pwd`, `ls`, `mkdir` and `rm` as shell builtins, `..` clamped at your root by
popping the stack of capabilities the shell descended through, `pwd` relative to that root, and a
name on a command line resolved against the shell's position **at the moment the grant is made**, so
a child holds a capability to one file and cannot re-resolve anything. `rm` is `UNLINK`, added to
`fs_proto` here and separated from revocation in the contract's own words; revocation is not offered,
because the FS server's handle table is per *server* and it cannot enumerate the clients holding
handles. The headline is proven with the real shell binary: two shells rooted in two subtrees, each
told nothing about which it holds, and neither can name the other's files (notes/shell-navigation.md).
**Still to do**: attaching the built `crates/glob` to an attenuated name-set caretaker, completion,
environment, and `PATH`. The `std` PAL still answers `Unsupported` for `rename`, `unlink` and
`rmdir`, which is now a binding gap rather than a missing verb for the first two.

**Why it matters.** Chris's framing, and it is the governing constraint: *"I hate Windows/DOS
specifically because they went differently than virtually every other OS I've used."* Gratuitous
divergence taxes every user forever. So the bar is not "is this more capability-pure", it is **"does
the model actually force this."** Three divergences clear that bar; the rest of Unix's surface should
survive unchanged.

#### The reframe: `cd` was never the problem

A working directory, in capability terms, is *a directory capability the shell holds, used as the
default base for resolving names*. Held by the shell that is entirely legitimate, the same as its
untyped budget. The badness in Unix is three specific things, none of which is `cd` itself:

1. **Children inherit it silently**, so every process gets a starting point nobody granted it.
2. **Relative paths resolve implicitly**, so a program's reach depends on invisible state.
3. **`..` walks out**, so the cwd bounds nothing.

Fix those three and the command is fine.

#### `cd`, `pwd`, `ls` are shell builtins, not programs

The same category as `caps`, which already prints the shell's whole endowment: they spawn nothing,
need no grant, and confer no new authority, because the shell is reading and rebinding what it already
holds. This also retires a worry raised while designing `ls`: that a listing program would be
over-granted, holding the power to read everything it lists. It is not a program.

**The cwd stops at the process boundary.** `wc report.txt` resolves the name against the
shell's current directory *at the moment the grant is made*, and the child receives a capability to
that one file. The child has no cwd, inherits nothing, and cannot re-resolve anything. The convenience
is the shell's; the authority is explicit.

#### The three earned divergences

- **No global absolute paths.** There is no namespace to root them in. Already true and already
  correct in the `std` PAL, which answers `InvalidFilename` rather than `PermissionDenied`: nothing
  checked a permission, the name simply cannot be expressed.
- **`..` stops at your root.** You descend from what you hold and never ascend past it. This is
  chroot's shape arrived at from the other direction.
- **`pwd` is relative to your root**, because naming anything above it implies a namespace that does
  not exist.

What that buys, and Unix cannot: **every shell has its own root.** Two shells can hold different
subtrees and neither can name the other's files, not by policy but because no capability reaching them
exists.

#### `mkdir` and `rm`

**`mkdir` is the same verb family as descending**: it mints a directory node and hands back a
capability to it, exactly as `CREATE` already returns a file handle. `mkdir` is descend-with-creation,
and the two should be designed together rather than separately.

**`rm` is where Unix conflated two operations.** `rm` unlinks a name; the data survives while anyone
holds a descriptor, and the blocks survive after that, so it cannot promise what people mean when they
delete something sensitive (and `shred` only pretends to on a copy-on-write filesystem like ours).
Separate them:

- **Unlink**: remove a name from a directory; existing capability holders keep reading. Unix's
  semantics, and genuinely useful (atomic replace and the temp-file idiom both depend on it).
- **Revoke**: the object dies and *every* capability to it goes stale.

The second is not exotic here: §13 revokes frames, §16 revokes objects, and generational names
(`crates/slots`) make a stale capability fail safely rather than point somewhere wrong. **One
implementation caveat to design rather than gloss:** the FS server validates handles against its own
table, so invalidating them is mechanically easy, but that table is per-session and the server does
not track all outstanding sessions today.

**The rights ladder becomes explicit**: a directory capability needs separable **enumerate**, **open**
(read versus write), **create** and **remove**. A program handed a directory to write logs into should
not thereby be able to delete what is there. `FileSpec` already makes this split for files, where the
manifest declares direction and the human designates the file without typing a mode.

**And one safety property falls out free.** `rm -rf /` is bounded here by what your directory
capability reaches, structurally. A shell rooted at a subtree cannot recursively delete the system,
because no capability naming those files exists in it. Not a guard rail, not a confirmation prompt,
not a check that could be wrong: there is nothing to name.

#### `rmdir` and `rm -r`: Unix already made the safe choice (decided 2026-07-31)

`mkdir` shipped in §48 with no way to remove what it makes: `rm` answers `EISDIR` and there is no
`RMDIR`. The lane declined to add one, on the grounds that "a verb that removes whatever it finds is
how one word takes a subtree away". That objection is right about a *recursive* verb and does not
apply to Unix's, which is the point.

**`rmdir(2)` removes only an empty directory**, and that is the whole safety property. The recursion
in `rm -r` lives in **userspace**, as a loop of individually safe single-step operations: walk, unlink
files, remove empty directories bottom-up. **No single call in the contract can take a subtree away.**

So: `RMDIR` requiring `REMOVE` on the parent, refusing non-empty with `ENOTEMPTY`, and explicitly
**not** revocation, for §48's reason: the handle table is per server, so handles cannot be
invalidated for clients the server cannot enumerate.

**The recursion is bounded by construction, which Unix cannot say.** `rm -r` needs `ENUMERATE` to see,
`DESCEND` to recurse and `REMOVE` to delete, *at every level*, so the walk stops exactly where the
capabilities stop. Unix bounds `rm -rf /` with a permission check per file, which is a check that can
be wrong and famously has been. This milestone's existing note stands: not a guard rail, not a
confirmation prompt, "there is nothing to name".

**`rm` is a program, not a builtin, and that is Unix's shape rather than a divergence from it.**
`cd`/`pwd`/`ls` are builtins here because the shell is rebinding what it already holds; `rm -r` is a
destructive loop, not a rebinding. A builtin would run with the shell's **entire endowment**, while a
program takes an explicit attenuated grant, so `caps rm -r logs/` prints the subtree at risk before
anything happens, and a bug in the recursion can only reach what it was handed. Same shape as
globbing below: attenuate, then hand over.

**`-f` stays, with Unix's semantics** (Chris, 2026-07-31). An earlier draft of this section argued it
should not exist, on the reasoning that with no prompting its only remaining meaning is suppressing
errors, which §42 forbids. **That was wrong about what `-f` does.** It means *ignore nonexistent files
and do not prompt*: a permission failure on a file that exists still reports. Its real value is
**idempotency**: `rm -f maybe-there` succeeding is what makes a script re-runnable, and "absence is
the desired state" is not a lie about failure. The divergence did not earn its keep.

**Reporting is Unix's, and it is quieter than an earlier draft of this section claimed.** Checked
against `rm(1)` rather than remembered: **silence on success**, `-v` exists precisely because the
default prints nothing ("be verbose when deleting files, showing them as they are removed"). Failure
is a diagnostic plus exit status: "exits 0 if all of the named files or file hierarchies were
removed… If an error occurs, rm exits with a value >0." So a partial `rm -r` says what it could not
do and exits non-zero, and says nothing about what it did. An earlier draft here said it should
"report what it removed", which is the `-v` behaviour, not the default.

`-f` is also broader than that draft assumed: "attempt to remove the files without prompting for
confirmation, **regardless of the file's permissions**. If the file does not exist, do not display a
diagnostic message **or modify the exit status**." So it suppresses the missing-file diagnostic *and*
its effect on the exit status. The claim that a permission failure still reports under `-f` was wrong.

**One thing to settle when building it.** A `rm -r` interrupted halfway leaves a partial tree, and
there is no transaction spanning requests: adding one would mean the server holding a transaction
open across receives, which conflicts with the serve-loop-runs-one-request-to-completion property §47
relies on for concurrency atomicity. Partial, with failures reported and a non-zero exit, is the
answer, and it happens to be exactly what Unix already does.

**Worth noticing while copying Unix here:** `rm(1)` says "it is an error to attempt to remove the
files `/`, `.` or `..`". That is a **literal special-case guard for `/`**, shipped in the utility,
precisely the "guard rail, a check that could be wrong" this milestone contrasts itself against. We
need no such case: a shell holding a subtree cannot name the root, so there is nothing to special-case. And `rm` on a directory stays a **refusal** (`EISDIR`) rather than a silent
escalation to recursive removal, which is Unix's behaviour and worth keeping for the same reason
`rmdir` is empty-only.

#### `ln`: hard links make it not a tree, and symlinks stop being an escalation

Two verbs with very different stories. Neither is built.

**Hard links are mechanically easy.** RedoxFS keeps link counts, and **§48's deferred-delete fix
already depends on them**: "the last link goes" is exactly what made `rm` an unlink rather than a
revoke. A second name for one node is a short step from there.

**The problem is structural, and it is ours rather than Unix's.** §47 justified `DESCEND` as a
separate right because otherwise "the shape of the tree would decide how much authority a grant
carried". **Hard links make it not a tree.** A file reachable from two directories sits in two
subtrees, so "this subtree" stops having a clean boundary: you granted a name, and the node is also
reachable through one you did not mention. That is not automatically wrong (the grant was the name),
but every piece of subtree reasoning written so far quietly assumes a DAG cannot happen, and that
assumption should be made explicit before it is falsified. Unix forbids hard links to *directories* to
prevent cycles; the argument is stronger here, where a cycle also defeats `rm -r`'s bottom-up
termination.

**Symlinks are the interesting one, and the answer is a real result.** A symlink stores a **path**,
resolved at open time, and this milestone already decided paths resolve **in the client, against the
holder's own position**, with `..` clamped at the root (§48). So: resolved against *whose* namespace?

Resolve against **the holder's**, and it follows that **a symlink cannot escalate**. It can only name
what the resolver could already reach. Unix's symlink attacks: the `/tmp` races, the confused-deputy
TOCTOU classics: work because resolution happens against a *global* namespace carrying the
*victim's* authority. There is no global namespace here and no borrowed authority, so a symlink can
**misdirect but cannot grant**. Same shape as the `PATH` result above: the escalation vector closes
because there is nothing ambient to point into.

The cost is that one symlink means different things to different holders. That sounds alarming and is
exactly Plan 9's per-process namespace behaviour, so it is a well-explored place to stand rather than
a novel one.

**What to settle before building either:** whether hard links are offered at all given the DAG
consequence (declining is defensible, and `mv` plus `RENAME` already covers the common
atomic-replace idiom that hard links are usually reached for); and, for symlinks, what a stored path
containing `..` means when the holder's root is shallower than the creator's: §48 clamps, so it
should clamp here too rather than erroring, but that is a decision.

##### ~~Open fork~~ **SETTLED 2026-07-31: `bind`, not stored paths** (DECISIONS §50)

**Chris chose namespace composition.** The analysis below is kept because the naming search is the
evidence for the decision rather than a digression: twenty-eight-plus candidates, terminating without
a winner, which is what a construct that does not fit any familiar relationship looks like. `bind`
needed no search: Plan 9 and `mount --bind` already named it. See §50 for the decision, what it
costs, and the inert-stored-path escape hatch if milestone 55 turns out to need on-disk fidelity.

###### The analysis that settled it: was the mechanism right, and if so what is it called? (raised 2026-07-31)

**Not decided.** Two questions, in this order, because the second keeps answering the first.

**Mechanism first. Plan 9 has no symlinks: it has `bind`.** Per-process namespaces made them
unnecessary: you do not need a stored path that resolves oddly per holder when you can compose the
holder's namespace directly. This milestone already took Plan 9's answer for absolute paths and for
`PATH`; taking Unix's here, renamed, would be the inconsistent choice. **Settle whether we want
namespace composition instead** before settling a noun.

**Then the name, because "symbolic link" fails §39 on both halves.** "Symbolic" is defined *against*
"hard", so if hard links are declined the adjective contrasts with something that does not exist.
"Link" is worse: **it links nothing.** The by-name-ness is the entire content: there is no object
identity, and two holders may resolve the same entry to different files or to nothing.

The criterion, which rules out most candidates at once. A name here must **not imply object
identity**, must **not imply a connection**, and must **not collide with "reference"**: in a
capability system a reference is unforgeable and holder-independent, the exact inverse of this. That
disposes of `link`, `reference`, `shortcut` and `pointer`.

Worked, and rejected with reasons rather than by taste:

| Candidate | Why not |
|---|---|
| `alias` | Semantically closer than `link`: a shell alias is stored text, expanded at use, meaning what the current environment makes it mean, with no identity claim. But **taken twice**: zsh's `alias` (which this milestone tracks, so we would collide with ourselves), and macOS "aliases", which store a file ID and **survive the target moving**: they track the object, the inverse of ours. Borrowing a Mac term for its opposite is a poor trade on a project whose first real user is a Mac |
| `costume`, `disguise` | Both imply **an underlying thing being dressed or concealed**, reinstating exactly the object identity the word must avoid. `disguise` also claims intent to mislead, naming into existence a danger this design removes: a stored name here cannot escalate, because it resolves only within what the holder already reaches |
| `projection`, `shadow` | Honest about viewpoint-dependence without implying concealment, and still **metaphors**. This project names descriptively (`net_stack`, `compositor`, `line_editor`), which is §39's doing; `link` got away with a false claim partly *because* it was a metaphor |
| `mirror` family (`erised`, `matsuyama`) | **The best framing anyone found, and the only family to pass all three tests**: a mirror shows something viewer-dependent, implies no object identity, implies no connection, and does not collide with "reference". It fails on the word rather than the idea. In computing a **mirror is an identical replica at another location**: "same content, elsewhere", which is the identity claim we are trying to avoid. The literary instances add their own wrong axis: Erised shows what you **desire** (ours shows what your namespace resolves to, often nothing), and the Matsuyama tale is about a **mistake** (the deception axis where `disguise` failed). Both also need a decoder ring, and `notes/naming.md` sets the bar at names that parse without prior exposure |
| `fsalias` | Fixes the zsh collision, and prefixes are in-style here (`fs_file_caretaker`, `fs_subtree_caretaker`, `c_confiner`). But **"filesystem alias" is exactly what Finder calls a macOS alias** (the object-tracking one), so the prefix picks the *wrong* one of the word's two meanings. And prefixing to fix a collision is a smell: it answers *which* alias, where the objection was that **alias claims another name for the same thing** |

**The descriptive candidate, if the mechanism survives:** a third **entry kind** beside file and
directory: a **`path`**. A directory entry names a file, a directory, or a path; it stores a path and
the holder resolves it, which is the whole description. It also reads correctly when it fails: *"that
entry is a path that does not resolve"* is what happened, where *"that link is broken"* implies
something was once connected. The verb becomes writing a path into a directory rather than "linking",
which retires the `ln -s` shape and its trailing-slash footgun with it.

**A further seven produced no new failure modes** (`speculum`, `glass`, `scryer`, `mimic`, `imitate`,
`parallel`, `echo`), which is what an exhausted search looks like. They re-derive the four already
listed: `speculum` and `glass` and `scryer` are the mirror family with added baggage (a medical
instrument, a *material* that only means mirror with "looking" in front, and a word naming **the
person looking rather than the thing looked into**, plus divination); `mimic` and `imitate` reinstate
**an original being imitated**, which is where `costume` and `disguise` died, and `imitate` is a verb
besides; `echo` collides with a shell builtin **we already have**, exactly as `alias` collides with
zsh's; and `parallel` means concurrency, in a system with four cores and per-CPU run queues.

**Two later candidates are worth their own line.** `harmonic` clears all three tests: the stored path
as fundamental, the holder as resonator, and fails on **the direction of causation**, a failure mode
none of the others had: a harmonic is *determined by* its fundamental, whereas our resolution is
determined by the **namespace**, not by the stored name. The metaphor points the causal arrow
backwards. (`harmony` is simply the wrong axis: it means concord, where ours may resolve to nothing.)

`reflection` is **the best of the mirror family**, better than `mirror` itself, because a reflection is
explicitly *not the thing* where a computing mirror implies an identical replica, and its causation is
right, since what you see depends on the mirror *and* where you stand. It fails on a harder collision:
in programming, **reflection is runtime introspection of types**, which is precise, universal, and in
our own domain.

**And that is the pattern behind the whole family.** `mirror` → replica, `reflection` → introspection,
`echo` → a shell builtin we ship, `parallel` → concurrency. **Physical-optics vocabulary has been
comprehensively borrowed by computing for unrelated meanings**, so the one metaphor that actually fits
this construct is the one whose every word is already spent. That is not bad luck; it is why a flat,
non-metaphorical name is the likely answer if the mechanism survives at all.

**That the naming is this hard is itself evidence.** Seventeen candidates, the first eight failing for a
*different* reason each and the rest finding almost none, and the one that passed every test failed on the word being occupied by its own inverse. The
construct is the only thing in this design whose meaning depends on who is looking, and the vocabulary
has no slot for that. Plan 9 hit the same wall from the same premises and answered with a different
mechanism rather than a better noun, which is why this fork is **mechanism first**.

##### Where `rm` meets them, which is where the sharp edges are

**`rm` on a symlink removes the link, never the target.** `rm(1)` says so outright: "the rm utility
removes symbolic links, not the files referenced by the links", and it is right for the reason §48
already established: `rm` operates on a **name in a directory**, and a symlink is a name.

**`rm -r` must not descend through a symlink**, and our reason differs from Unix's in a way worth
recording. Unix declines because following would **escape**: a symlink to `/` inside a directory
would turn `rm -r` into `rm -rf /`. Here it could not escape: a symlink resolves in the holder's
namespace, and `rm`'s namespace is the granted subtree with `..` clamped at its root (§48), so a
symlink cannot name anything outside the grant. **We keep the behaviour and lose the reason.** The
behaviour still earns its place: following would delete a different set of names than the grant
named, and "surprising but bounded" is still surprising.

**`rm` on a hard-linked file removes one name and the data survives.** That is not a special case, it
is exactly §48's unlink-versus-revoke distinction, and the mechanism is already built: RedoxFS's
deferred delete (`on_open_node` / `on_close_node` and the release list) is what makes the last link,
not the first, the one that frees.

**The sharp one: `rm -r subtree/` where a file inside is also linked from outside.** The subtree goes
away and the data does not, because the outside name still holds it. That is correct: you removed the
names you were granted, and you were never granted the other one, but it means **"I deleted the
subtree" and "that content is gone" stop being the same statement.** For a backup target (milestone
55) that distinction is worth stating rather than discovering.

**And the cycle, which is a termination argument rather than a taste one.** `rm -r` works bottom-up,
so a hard link making a directory its own descendant does not merely confuse it: it **does not
terminate**. Unix forbids hard-linked directories for this reason among others; here the same
prohibition is load-bearing for a verb we have already shipped the recursion for.

**One footgun inherited if symlinks land:** `rm -r link` versus `rm -r link/`. The trailing slash
changes whether the target's contents are in scope, which is a real source of accidents in Unix.
Decide it explicitly rather than letting the path parser decide by accident.

#### `touch`, and the reason file times were refused has expired

Not built, and it splits the way `mv` and `rm` did. **Creating an empty file if absent** is
expressible today (`fs_proto::fs::CREATE`, milestone 31 phase 2, and §49's `DirSpec` already shapes
"a program granted the directory a name lives in"). **Updating the modification time** is not, and the
reason is narrow: the `std` PAL records that "the server keeps an mtime **but the contract does not
carry one**". RedoxFS tracks it; `fs_proto` does not expose it.

**The justification for that has gone stale.** `notes/std.md` refused file times partly because "there
is no wall clock to interpret it against anyway": true when written, and false since milestone 51
landed the clock (§43, RTC drivers on both ISAs, `date`). Same shape as §43's own untestability note,
which milestone 47's `date` work disproved: **a scope note outlives the condition that justified it.**

**The authority question, which should be decided rather than defaulted into.** `touch` does two
different things to a timestamp: set it to *now*, and `touch -t` set it to *whatever you say*. The
second is the ability to **lie about history**, which matters for anything reasoning from mtime,
backups included, and milestone 55 is a Time Machine target. That is §43's asymmetry again (reading
harmless, setting an authority), one level down: is "set to now" the same right as "set to an
arbitrary value", and does the file's write right already cover both? Neither answer is obvious.

#### Globbing, which decides how every multi-file operation grants

##### Built 2026-07-31: the matcher, then the grant. See notes/glob.md and notes/glob-grant.md.

The decided answer is implemented rather than revisited: `rm *.txt` grants a directory capability
attenuated to a **name set**, served by `user/src/fs_nameset_caretaker.rs`. Four things this section
did not predict, and one it did:

- **It predicted the shape of the change to `grant_plan`**, and that is exactly what happened.
  `plan_against` fills its slots by **index** now, and takes an `Expansion` keyed to that index,
  because the endowment is the set rather than the pattern. `DirGrant.name` became `DirGrant.names`,
  which is the finding in the type system: a literal operand is the set of one.
- **The caretaker is a third one, not a generalization of `fs_file_caretaker` or a mode on
  `fs_subtree_caretaker`.** `fs_file_caretaker` serves the *file* protocol, so teaching it a set
  would be writing a directory caretaker; and `fs_subtree_caretaker`'s design property is that it
  performs **no checks at all**, which a name filter (on seven name-taking verbs) would end. The
  grants also have different shapes: a name rides in registers, a set needs a frame.
- **An empty match is a refusal**, zsh's answer. The obvious argument for bash's pass-through was
  checked and is **wrong**: nothing here refuses `*` in a component, so passing the pattern through
  builds a grant whose namespace is a name nobody has, and which acquires a referent the moment
  somebody creates that file.
- **`ARG_MAX` landed at eight names, set by a stack overflow rather than by reasoning.** Sixteen was
  the number the argument produced; the shell ran off the bottom of its stack planning one grant,
  twice. Exceeding the bound is a loud refusal at the prompt, never a truncation.
- **Qualifiers and `**` stayed out**, for notes/glob.md's reasons, which are authority questions and
  not scheduling ones. `xargs` is still not built: the answer at the bound is a refusal.

Tests: `grant_plan` and `fs_proto` host suites; `kernel::user::glob_grant_tests` on both ISAs (a real
shell expanding one pattern two ways, then `rm` as its own attacker behind a real
`fs_nameset_caretaker`); and `xtask::redoxfs_glob_grant_took_exactly_the_match` reading the image
from outside the guest.

zsh's glob engine is the best thing in the shell (`**/*.rs`, and qualifiers: `*(.)` for regular
files, `*(om[1])` for newest, `*(Lm+1)` for over a megabyte). The mechanism is unremarkable here
because **a glob is an enumeration**, and the rights ladder above already separates `enumerate` out.
The fork is not how to match. It is **what a match grants**.

`rm *.txt` with five hundred hits, four candidate answers:

| Answer | Verdict |
|---|---|
| Grant 500 file capabilities | Honest, and it exhausts capability slots |
| Grant the directory plus a name list | Cheap, and it **over-grants catastrophically**: `rm` could touch anything in that directory, which is the thing this whole model refuses |
| Make `rm` a builtin so the shell deletes and nothing is granted | Dodges the question, and costs `rm` as a program |
| **A directory capability attenuated to a name set** | **The principled one** |

The last is a smaller change than it looks, and that is the finding. `fs_file_caretaker` today
serves "a namespace of exactly one name"; globbing generalizes it to a **set** of names. Same
caretaker, same `fs_proto` protocol above and below, wider namespace. **Nothing new in the kernel**,
and the attenuation stays checkable from outside the confined program exactly as it is today.

**The property worth demonstrating: the expansion you see is the grant.** `echo *.txt` prints
literally the authority that `rm *.txt` would transfer, because the matched set *is* the namespace
the caretaker will serve. Unix cannot make that claim, since `rm`'s authority never came from the
command line at all; the glob merely told it which of its existing powers to use.

**Who expands.** The shell, before planning the grant, which is also what Unix does, so there is no
divergence to earn. The structural consequence is that `grant_plan::plan` must see the expanded set rather
than the pattern, since the endowment is the set.

**Two costs to design rather than gloss.**

- **Qualifiers are not free.** `*(.)` and `*(om[1])` need type, mtime and size *per candidate*, so one
  `enumerate` becomes N `FSTAT` calls, and they need a read right beyond enumerate. Decide whether
  qualifiers are in scope at all before building the matcher around them.
- **`ARG_MAX` becomes a capability limit rather than a buffer limit.** Unix's "argument list too long"
  is why `xargs` exists; here the ceiling is that you cannot hand a child a hundred thousand
  capabilities. The same failure with a more honest cause, and it wants the same answer (batching),
  so `xargs` earns its place for a better reason than Unix had.

**Completion shares this mechanism and should be designed with it**, not after it: tab completion is
also an enumeration, so the completion menu is a rendering of your authority and cannot offer a path
no capability reaches.

#### Absolute paths: Plan 9's answer, not DOS's

Distinguish a path as *authority* (`open()` resolving against a namespace nobody granted you: out
permanently) from a path as a *name* (a string, and a name is not a capability). The syntax can
survive even though the semantics cannot.

**Plan 9 kept absolute paths and made `/` the root of *your* namespace**, assembled from what you were
given, so two processes can both open `/lib/foo` and get different files. That is the counter-example
to gratuitous divergence: the system that took namespaces furthest did not abolish paths, it made them
personal. It also lines up with "every shell has its own root" above, which is not a coincidence.

**The real decision is where the resolver lives**, and it changes the security story:

- *In the FS server*: it accepts multi-component paths and walks them. Workable, but it puts
  path-walking back into a server, against §27's discipline that open-by-path exists only inside the
  server relative to one bound directory.
- *In the client's runtime* (`user_rt`): a small table of prefix to directory capability, granted at
  spawn, resolved locally and privately. The server still only ever sees a **single-component name
  relative to a capability presented to it**, leaving §27 intact.

**Recommendation: the client's runtime.** It yields absolute-looking paths with no server learning a
name it did not already own, and the namespace becomes another endowment, inspectable in `caps`,
which Unix cannot do, since you cannot enumerate what your paths could reach. The honest cost is that
two processes seeing different files at one path is powerful and confusing, and Plan 9 users will
attest to both halves.

#### Environment variables, which are the same question wearing a string costume

**Clean slate**: there is no `argv` and no `envp` today. `notes/abi.md` is explicit: "no libc, no
`argv`/`envp` array, no dynamic loader, no `main` wrapper", so a program gets argument words in
registers and a populated cspace. Nothing has to be undone, and §15 already carries the natural seam
as a deferred item: a **BootInfo** page, "a structured block the loader hands the program".

Unix puts three different things in one string-to-string map, which is why environment variables are
both indispensable and a security disaster:

- **Inert configuration** (`LANG`, `TZ`, `TERM`). Genuinely just data, no authority in it.
- **Names for finding things** (`PATH`, `HOME`). This is namespace, and therefore *this milestone's*
  question: `HOME` is a directory capability wearing a string costume, and `PATH` is "the set of
  directories I may spawn programs from", which is a set of capabilities.
- **Secrets** (`AWS_SECRET_KEY` and friends). These are **authority badly encoded as a bearer
  string**. In a capability system a credential is a capability to a service, not a value you can
  print, log, or leak into a crash dump.

So the three go three different places: data stays data, names become capabilities (the work above),
and secrets become endpoints.

**The property worth designing for is not secrecy, it is that environment is an *open channel*.** In
Unix anyone can set any variable and hope the program reads it, which makes every process carry an
unbounded implicit input. `LD_PRELOAD`, `IFS`, `PATH` and a long tail of library-specific variables
are attacks that work because a program can be influenced by something it never asked for and does not
know exists.

Invert it: **a program declares the configuration it reads, and undeclared variables cannot reach
it.** That is not a new mechanism, it is exactly what the SHILL-style manifest already does for
capabilities: a program declares its expected endowment, the manifest is checked at spawn, and a
mismatch is a refusal at the prompt rather than a mystery later. Configuration is the same shape, and
declaring it closes the entire `LD_PRELOAD` class by construction rather than by blocklist.

**And no inheritance.** Unix's environment is inherited by default, which is exactly why a secret in a
shell leaks into every child including those with no business seeing it. Here it is granted like
everything else: at spawn, explicitly, visible in `caps`. The honest tension is the governing
constraint above: environment variables are convenient *because* they are inherited, and full
explicitness is verbose. Proposed middle ground: **inheritance with visibility.** The shell holds a
default config set and passes it, but the passing is explicit and inspectable, so `caps run prog`
shows exactly what that program will see before it runs. Convenient in the common case, never
invisible.

**One thing to decide deliberately rather than drift into.** If configuration is declared in the
manifest, the manifest grows from "what capabilities do I need" into "what do I need at all". That is
a larger claim than it makes today, and it is the sort of scope creep that is easier to accept early
than to reverse later.

#### Milestone 64 is what forces the namespace half of this milestone

Recorded here as well as in 64, so neither is picked up without it.

**What remains open in this milestone is the namespace machinery**: absolute paths, environment
variables, `PATH`, and `bind`, which §50 decided and explicitly did not build because it needs "a
mount table per process and resolution through it" beyond the per-shell roots that exist today.

**None of it has a forcing use case from the shell.** `swish` works with per-shell roots; `bind` is
a mechanism nobody currently has to have. That is why this milestone has sat IN-PROGRESS with its
navigation half done and its namespace half designed.

**Milestone 64 supplies the missing demand.** `std::fs::File::open` takes a **path**, and a `std`
program is not a shell: it cannot be handed a root and told to `cd`. A crate that writes
`Path::new("assets").join("x.png")` is a concrete request for per-process namespace resolution, which
is exactly what `bind` is for. The `PATH` conclusion below, that a program namespace **is** an
endowment, gets its first real customer at the same moment.

**The sequencing this implies**, and it runs the other way from the obvious: **let 64 measure first.**
Its probe crates will report what a real dependency actually needs, and that evidence is what this
milestone's remaining scope should be sized against, rather than building the general namespace and
hoping it fits. `File::open`'s resolution is then **one fork answered once**, spanning both
milestones, instead of a PAL trick here and a design there.

#### `PATH`: there is no search, because there is no ambient namespace to search

The absolute-paths section above takes Plan 9's answer for paths in general; `PATH` is that same
question narrowed to programs, and Plan 9 answers it the same way. **Plan 9 has no `PATH` variable
at all.** `/bin` is bound per-process, union-mounted from whatever that process's namespace assembled,
so what you can run is what is bound. Taking the same answer here is consistency, not a new idea.

**`PATH` is two bad things at once.** It is a *search*, over a namespace you have *ambient access to*.
The search makes the order of a string into a security boundary: a writable directory ahead of a
system one, or `.` anywhere in it, and someone plants an `ls` that you then run. The ambient access is
why the order matters at all, since `PATH` never controlled *access* (permissions did), only which of
your already-reachable options wins. The tell is that `which` exists as a whole command whose job is
answering "which one did I actually get?".

**So the program namespace is the endowment**, and a name binds to exactly one thing in it. The
property that follows is the same class as `rm -rf /` above: **`PATH` injection is structurally
impossible rather than mitigated.** No search order to manipulate, no `.` to include by accident, no
writable directory that can precede a system one, because there is no search.

**The distinction that makes it work.** A shell may extend its program namespace **only with
capabilities it already holds**, so extending is a naming convenience and never an authority increase.
Unix nominally has this property too and loses it in practice: ambient authority means everyone can
read `/usr/bin`, so `PATH` order becomes the de facto security boundary. Here it cannot be, because
naming and access are separate things.

**Four open questions, none decided:**

- **Unions and shadowing.** A namespace unioned from several sources brings first-match-wins back,
  which is the ambiguity just removed. Plan 9 chose ordered union with explicit before/after on
  `bind`; the alternative is to **refuse ambiguity** (an error when two sources offer `ls`), which is
  more honest and probably more irritating.
- **Enumeration.** "What can I run?" is enumeration of the namespace, the same insight as globbing and
  completion above and bounded the same way: completion cannot offer a program no capability reaches.
- **Compile-time set to runtime lookup.** `Prog` is a closed enum with `from_name` today, and init
  already loads from the initrd by name, so half the mechanism exists. What is missing is enumeration
  and not being a fixed set.
- **Does `$PATH` survive as a string?** If the namespace is a capability, `echo $PATH` has no
  referent, and that is a divergence on one of the most-referenced variables in shell scripting. It
  looks earned (the variable's two real uses, inspect and modify, become `caps` and a grant) but the
  cost is real and should be named rather than glossed.

**Two milestones this reaches into.** Milestone 49 (users, login, and attribution) is what *hands* a
session its program namespace, so "who gets which capabilities at startup" includes which programs.
And milestone 39 (repository structure and the road to a distribution) inherits the sharper
consequence: **installing a program becomes granting it into a namespace**, which is a materially
different packaging story and is worth being on the record before anyone designs a package manager
around the assumption that installation means writing into a globally readable directory.

#### `file:` and `run` are not earned, and come out (decided 2026-07-30)

##### Built 2026-07-31: the grammar change, ahead of the commands.

`run` and `file:` are gone from `grant_plan` and the shell. A bare program name spawns it (`worker 9`,
`budgeter --mem 16`, `date`); a bare token in a file position designates the file, and the manifest
still declares the direction. `--mem N` stays, and is now accepted on either side of the program
name because with the verb gone a leading flag reads wrong.

**The change the analysis above did not anticipate: the parser stopped classifying tokens at all.**
`RunSpec` keeps the positionals in the order typed and `plan_against` places them into the slots the
manifest declares, which is what makes "the manifest says what it is" true in the code rather than
only in the prose. A shape-based rule (a number is the argument, anything else is the file) would
have read `wc 2026` as a missing file.

`caps <command>` is the preview's new spelling: the tail is the command you would have typed, so
what you inspect and what you run cannot drift apart, and it is the Unix prefix-word idiom (`time`,
`nice`, `env`) rather than new grammar. The refusals moved from "drop the `file:` designator" to
positional wording, and one refusal **order** changed on purpose: a program's own declaration is
checked before what the shell holds, so `worker report.txt` answers "takes no file; drop the name"
(true whatever this shell holds) rather than "you hold no such capability" (an accident of this
boot). The consequence, recorded rather than glossed: no shipped program declares
`FileSpec::Required`, so the headline "no such capability" refusal is no longer reachable from the
prompt, only through `plan_against` in the host tests.

**`date` came along with it**, because with `run` gone `date` is exactly what a person types, and
the shell had never heard of it (`Prog` knew four programs). It has a `Prog` entry and an all-
`Forbidden` manifest; the shell spawns it with the register defaults, since `ArgSpec` has no
position or arity yet. **It is the first program whose whole authority the command line cannot
name**: a read-only mapping of the clock page, which init endows. This boot starts no clock service,
so it prints "the time is unknown: this process holds no clock capability", and `caps date` says so
before you run it. What a shell that could delegate a clock would need is assessed in
notes/grant-expression.md and is its own lane: kernel boot wiring on both ISAs, a spawn-protocol
position, both inits, and nothing in the suite boots the interactive shell to prove any of it.

Tests: `crates/grant_plan` host suite, 34 cases. Notes: grant-expression.md, program-manifest.md, date.md.

Chris asked to be convinced they were worth the typing. They are not, and the case against each is
stronger than the case that put them there.

**`run` fails on consistency, the DOS objection turned inward.** This milestone adds `ls`, `cd`,
`pwd`, `mkdir`, `rm`, and nobody would type `run ls`. So builtins become bare words while programs
need a verb, and a user has to know *which class a command is in* to know how to type it. That is
precisely the arbitrary divergence this milestone exists to refuse. Milestone 50 (pipes and
redirection) finishes it: `run a | run b` is indefensible. The lookup that replaces it already
exists (`Prog::from_name`, and `dispatch`'s `Unknown` arm), so `run` is phase-1 scaffolding from
when there were two programs, not a design position.

**`file:` fails because it announces the wrong half of the grant.** `wc file:report.txt` reads and
`tee file:report.txt` writes: identical syntax, opposite authority. Direction lives in the manifest
by design (milestone 31, a capability shell, took the SHILL shape deliberately), so the prefix marks
the part already visible and stays silent on the part that matters. The safety argument fails too,
on inspection: `worker 5 extra` is refused as unplaceable because worker's manifest says
`FileSpec::Forbidden`, not because of any prefix. **The manifest was doing all the work and the
prefix was taking credit.**

The reason it cannot carry the thesis is deeper than either. **The capability claim is about absence,
not presence.** That a filename grants access to that file surprises nobody; what `wc report.txt`
proves is that wc got that file *and nothing else*, and that claim lives in the tokens which are not
on the line. A prefix decorating a token that *is* present cannot express it. `caps run <cmd>` can,
including direction, which makes it the visibility mechanism and an argument for making it good.

**What survives:** the manifest declaring direction (load-bearing, untouched); `caps` as the sole
visibility surface; `--mem 16`, a real grant with no Unix analogue spelled as an ordinary flag.

**Do it in this milestone, because the window closes.** No program today takes both an argument and a
file (worker takes an int, budgeter takes memory, heeder and spinner take neither), so positional
resolution is at most one bare token and the manifest says what it is. Once a program wants both
(`grep pattern file.txt`), `ArgSpec` has to grow position and arity. The cost is that this changes
grammar in milestone 31, which is **built and host-tested**, so the refusal wording changes from
"drop the `file:` designator" to something positional. Those tests are the work; it is a contained
edit, not a redesign.

#### Open fork: should the shell be function calls rather than whitespace? (raised 2026-07-30)

Chris proposed `wc(cat(this-file.txt))` or `cat(this-file.txt).wc()`, on the grounds that shells lean
too hard on whitespace to tell a name from its arguments. **Not decided.** Recorded because the idea
contains one thing worth keeping whatever the syntax ends up being.

**The diagnosis needs adjusting first.** Whitespace is not ambiguous about which token is the
command; position handles that and always has. The real pathology is that a value containing a space
is **silently re-split into two arguments** after substitution, and then `IFS`, `"$@"` versus `$@`,
and glob expansion firing at the wrong moment. Call syntax does cure it, but so does never
re-splitting a value, which costs no syntax and which we can adopt freely having no legacy.

**The two proposed forms are not equivalent.** `wc(cat(f))` is command substitution, not a pipeline:
the inner call must complete and return a value, which buffers the whole output. `cat(f).wc()` reads
in the direction data flows and is genuinely pipe-shaped, which is why `|>` exists in Elixir, F# and
OCaml. But a method implies an object with a type, and milestone 50 currently carries **bytes**; over
bytes, `.wc()` is `| wc` with more punctuation, promising something the substrate lacks. **Typed
pipelines are a separate and larger fork** and should be decided in milestone 50 on their own merits,
not smuggled in through notation.

**The part that is genuinely ours: application is grant.** `f(x)` means "spawn f, grant it x", so in
`wc(cat(f))` the nesting *is* the authority tree, and the delegation structure can be read straight
off the syntax. No other shell can say that, because in Unix both `f(x)` and `f x` mean "f can
already reach everything, here is a string". **Worth writing down as the mental model regardless of
which surface wins**, and it is a better answer than `file:` ever was.

**Three objections.** It costs more keystrokes than the `file:` this same milestone just deleted for
costing five. Bare `ls` becomes `ls()`, miserable interactively, so both spellings get allowed and
commands acquire two classes, which is the *same* objection that killed `run`. And shells are
optimised for typing where languages are optimised for reading; Oil/YSH, Elvish and Nushell all ran
at this, and Plan 9's `rc` is the one that worked, precisely by fixing quoting and word splitting
while keeping the terse surface.

**The recommendation, if this is settled without further design:** kill word splitting outright,
keep whitespace application with parentheses for **grouping only** (`wc (cat report.txt)`, the ML and
`rc` answer), and record "application is grant". That takes what the idea is pointing at and drops
the notation.

#### The finding that should drive the build order

`cd`, `mkdir`, and per-process namespaces each converge on the same missing primitive: **a verb that
returns a directory capability rather than bytes.** It would be the first place this contract hands
back authority instead of data, and it deserves the care `Endpoint::REAP` got (§32): what rights does
the child directory carry, can they ever exceed the parent's, and who may call it. Build that first;
the commands are the easy part once it exists.

**Sequencing.** After milestone 37, which owns the FS server's block path. **Effort: 2 lanes
estimated** (one for the descend/create verb and the builtins, one for namespaces), noting that
estimates for unbuilt work are guesses on a scale calibrated from history, not measurements.

### 48. Job control: `jobs`, `wait`, `kill`, `fg`, `bg`, and a stopped state

**In brief.** Shell job control, in two phases split by whether they need a new kernel primitive.
**Phase one needs none**; phase two is `Tcb::SUSPEND`/`RESUME`, which DECISIONS §24 deferred and whose
own trigger list names "real job control (`fg`/`bg`, a stopped-process state) in the shell" as trigger
2. That trigger has now fired.

**Why it matters.** Unix job control is one of the most intricate things in a kernel: sessions, a
controlling terminal, process groups, `tcsetpgrp`, and `SIGTSTP`/`SIGCONT`/`SIGTTIN`/`SIGTTOU`. Most
of that machinery exists to answer one question (*who may read the keyboard*), and here that question
answers itself.

#### A job is what the shell holds capabilities for

Structural rather than conventional. The shell built its children through the granular verbs, so it
holds their TCBs, their untyped region, and the supervision endpoint they report to. Unix's process
group is a *number* with inherited, mutable membership; "what I hold" cannot drift.

#### Phase one: no new kernel surface

- **`jobs`**: the shell listing its own holdings, the same category as `caps`, `pwd` and `ls`.
- **`wait`**: §26 already delivers exit as a message with a kernel-stamped tid, so this is a receive
  on the supervision endpoint.
- **`kill`**: §24 already built this under another name: the cooperative tier is the shared-flag
  interrupt, and `kill -9` is the forcible tier (`Untyped::DESTROY`). Job control needs no signal
  model because the two-tier one exists.
- **`&`**: running in the background is simply *not granting the terminal*.
- **`fg` on a running background job**: a capability transfer, below.

**Foreground versus background is: who holds the terminal input capability.** `fg %1` is the shell
revoking that capability from whoever held it and granting it to job 1; revocation (§13, §16) is
already built and is exactly the primitive this needs. A background job that reads the terminal does
not get `SIGTTIN` and does not get stopped: **it has no capability to read with**, and the refusal is
"you hold no such capability". Sessions, controlling terminals, `tcsetpgrp` and two of the four signals
disappear, not by reimplementation but because the question they answer is already answered by who
holds what.

#### Phase two: the stopped state

Only Ctrl-Z, `bg` on a stopped job, and zsh's `suspend` need pausing a thread resumably. §24's tiers
are notify and kill, with pause deliberately absent. Build `Tcb::SUSPEND`/`RESUME` per that tracker's
own instruction: **design it as one surface with the fault endpoint** (both are "the kernel turns a
thread's state into a message a supervisor holds"), and give the method its own DECISIONS entry. The
same verb unlocks the other two triggers, a userspace pager and a debugger, so it should be designed
for all three rather than for job control alone.

#### The open question: `disown`

If the shell drops its capabilities to a job, nobody can reap it, and §26's dead-until-reaped means the
corpse persists. Unix reparents orphans to init and lets init reap them; here reparenting means
**transferring the supervision endpoint**, which is an explicit act rather than a rule nobody thinks
about. **Decided as DECISIONS §40**: a supervisor's death is its subtree's death, because a child's
resources come from its supervisor's region and §16's revocation reclaims the whole subtree in one act.
So `disown` means **transfer supervision upward**, not "abandon", and §40 records the hole that makes
the cascade close to the only coherent answer, namely that §32 authorizes reaping by matching the
child's recorded `fault_ep`, which nobody can satisfy once the supervisor's endpoint is gone.

**Sequencing.** Phase one after milestone 47 (it wants `jobs` alongside the other builtins and the same
shell surface). Phase two is gated on nothing but the SUSPEND decision. **Effort: 1 lane estimated per
phase**, noting estimates for unbuilt work are guesses on a history-calibrated scale.

### 49. Users, login, and attribution: what identity is for once it stops being authority

**In brief.** Unix's uid does four different jobs at once. Three of them are already answered here,
structurally and without anyone having declared it; the fourth has no mechanism whatsoever. This
milestone writes down the first three, builds a login service that produces capabilities instead of
changing an identity field, and then decides what to do about the fourth.

**Why it matters.** Users and groups **are** Unix's ambient authority mechanism. A process's authority
comes from who it belongs to rather than from what it was given, which makes every program a confused
deputy by default; `setuid` is that idea in its purest form, a program running with the union of its
owner's authority and its invoker's intent, and it has been a security disaster for fifty years.
Saying "we do not have uids" is not the interesting claim. The interesting claim is that *the work a
uid does still has to get done*, and here it gets done by four different mechanisms rather than one
overloaded number.

#### The starting position: the tree is already identity-free, by accident of good design

Verified rather than assumed: **no `uid` or `gid` appears anywhere in our logic.** The vendored
RedoxFS on-disk `Node` carries the fields and `create_node` inherits them from the parent, because
that is the format; nothing ever reads them for an access decision. The `std` PAL lists permissions
under Unsupported, and `set_permissions` refuses rather than pretending. So this milestone documents
and completes a position the code already holds instead of migrating to a new one.

#### What each of the uid's four jobs becomes

| Unix uses uid for | Here | Status |
|---|---|---|
| **Authorization** | Capabilities. There is no check to bypass because there is no check | Built |
| **Isolation between humans** | Milestone 47's per-shell root. Two people's shells hold different directory capabilities and neither can *name* the other's files | Built (never demonstrated multi-user) |
| **Resource accounting** | The untyped budget. A user's allowance is the region they were granted; `run --mem 16` splits from it | Built |
| **Attribution** (*who did this?*) | Nothing | **Missing entirely** |

Isolation is the one worth dwelling on, because it is stronger than what a uid buys. Unix isolates by
*refusing* a request that names another user's file; the name is still sayable, the check is still
code that can be wrong, and root skips it. Here no capability reaching those files exists in that
shell, so the request cannot be phrased. A check that cannot be wrong because it is not performed.

#### There is no root, and that is a statable property

Milestone 22 did something Unix structurally cannot: `root_supervisor` **gives its authority away**, deleting
its untyped once the sub-servers are running. The consequence is worth stating plainly next to the
benchmarks, because it is the kind of claim a demonstrator exists to make: **there is no point after
boot at which any principal can do everything**, not as a policy or a hardening measure but because no
capability naming everything survives. Unix's root is always one `sudo` away by construction.

#### Groups are a delegation pattern, not a mechanism to build

Sharing is two parties holding the same capability, or capabilities derived from a common one;
nothing needs to be added to support it. Managed sharing (revocable, narrower for some holders,
auditable) is a **caretaker**, and `fs_file_caretaker` is already that shape: a component holding a
resource and serving several clients on its own terms. So this milestone builds no group mechanism
and instead documents the two patterns, because the alternative is someone inventing a group table
later.

#### Login: authentication produces capabilities

Unix login authenticates and then mutates an identity field. Here it authenticates and then **hands
over a capability set**: a root directory, a budget, a terminal. That is a better failure mode as well
as a cleaner model, since a compromised login service leaks *what it can grant* rather than the
ability to become anyone. It is the powerbox pattern with the human at one end, and it needs a real
answer to a question we have never faced: **who gets which capabilities at startup**, which is
currently a build-time fact baked into `root_supervisor`.

#### Attribution is the actual work, and the one place Unix does something we do not

A capability says what you may do. It says nothing about who did it. Unix gets audit almost free
because the uid is present at every syscall and doubles as the answer. Measured boot (§22) establishes
*what code* is running and capabilities establish *what it can reach*, but nothing records *who
asked*, and that gap is real rather than rhetorical.

The design fork to settle before building, and it should be settled deliberately: attribution can be
**a property of a capability** (an invocation carries a stamped origin, which risks re-growing an
ambient identity through the back door), or **a property of a channel** (a server logs which endpoint
a request arrived on, which is honest, needs no kernel change, and gives coarser answers). The second
looks right and the first should not be dismissed without argument. Whichever wins gets a DECISIONS
entry, because a wrong answer here quietly reintroduces the thing this whole model removed.

**Sequencing.** After 47 (isolation is 47's per-shell root, and login hands out exactly what 47
defines). The documentation and the group/caretaker write-up are cheap; the login service is a real
component; attribution is a design fork first and a build second. **Effort: 2 lanes estimated**,
noting estimates for unbuilt work are guesses on a history-calibrated scale, and the attribution half
could be one lane or three depending on which fork wins.

### 50. Pipes and redirection: one sink protocol, and `|` turns out to be an endpoint

**The protocol lane built 2026-07-31** (`crates/sink_proto`, `user/src/sink.rs`, the std PAL's
`sys/stdio`, and `abi::Error::Gone`; concept note: notes/sink-protocol.md). One framing for "write
these bytes there", proven on both ISAs by running one `std_exerciser` ELF against two destinations that
share nothing but sixteen bytes of message and comparing the bytes.

Three things came out of it that were not in the plan below.

- **The kernel could not express the thing the plan required.** "Gone" and "never had one" both
  arrived as `NoSuchSlot`, so no amount of userspace protocol design could have recovered the
  distinction; the ABI grew a variant. That is the finding, and it is why doing this before `|`
  existed was right.
- **`SIGPIPE` needed no new mechanism above the ABI**, because std already splits fatal from
  swallowed print failures through `is_ebadf`, and the old PAL was defeating it by answering `true`
  unconditionally.
- **A sink capability must not double as a terminal-service capability**, which is what stops
  `line_editor` from simply serving the contract on the endpoint it already has: that endpoint also
  carries `OP_READLINE`, so handing it to a child as its output slot would grant the child the
  terminal's *input*. The terminal's sink is therefore a separate endpoint served by an adapter,
  which is the shape `user/src/sink.rs`'s file role proves against a real backend. Converting
  `line_editor` and the console server is left with the shell work, because their clients are the
  shell and `system_initializer`.

**The operators lane built 2026-07-31** (`crates/grant_plan/src/line.rs`, `user/src/wc.rs`, the shell,
`system_initializer`, and two bits on `grant_plan::spawnproto`; concept note: notes/pipes.md). `date | wc` runs at a
real prompt on both ISAs, with the shell minting the endpoint out of its own budget and init putting
it in the child's output slot. The kernel did not change.

Four things came out of it that were not in the plan below.

- **The input slot's shape had to be decided and the smallest answer was the right one**: a source
  is *the sink contract received rather than sent*. No new protocol, and `<` and the right end of a
  `|` become one convention, exactly as `>` and the left end already were.
- **The manifest had to learn that not every program's slot 0 carries bytes.** `worker` answers with
  a `u64` in a register and the interrupt demonstrators hold no output capability at all, so
  `worker 9 > out.txt` would have written an unreadable word into a file with no error anywhere.
  `OutputSpec` makes it a refusal at the prompt. This is a wart the sink protocol inherited rather
  than created: the register fastpath is older than the contract.
- **`InputSpec` produces a refusal Unix cannot.** `wc` with nothing feeding it blocks on a receive
  forever, and on Unix that is a shell that appears to hang, because fd 0 always exists there.
  Here the manifest knows, so the prompt knows.
- **A pipe needs its own untyped region, not just an endpoint.** Deleting every capability naming an
  endpoint does not destroy it (the object lives in a page of a region), so a producer blocked in a
  `SEND` after its reader finished would stay blocked forever. The shell splits a region per
  pipeline and `DESTROY`s it, and that is what turns a dead reader into `Gone`.

**The append lane finished it 2026-08-02** (`line::Mode`, `swish`'s `open_sink`,
`cargo xtask shell-check`). `>>` is the cheapest of the four operators and that is DECISIONS §55
paying out: the shell already backs the file, so append is one bit about how it opens one, and
`grant_plan` asserts that `date > f` and `date >> f` plan to endowments equal in every other field.
That lane also built the gate notes/pipes.md named as the milestone's most valuable missing test:
`script/shell-check` boots `--features shell` on both ISAs and types at the prompt, which is the
only thing in the tree that runs the real `system_initializer`.

**Still open here, and named honestly in notes/pipes.md's BUGS**: buffering (a pipeline is full
lockstep and has not been benchmarked against a Unix pipe), the terminal's own sink adapter, and
**`2>`, which is a design fork rather than a task**. This system has no ambient anything, so a
program holds one output endpoint and its diagnostics ride it in-band; a second stream is either a
second capability in a second slot (Unix's fd numbering with a capability underneath, and it forces
a numbered slot convention first) or a second opcode on the one endpoint (§51 intact, but a
diagnostic then flows down a `|` into a `wc` that would count it). notes/pipes.md weighs both. What
is already separated is the half that hurts most on Unix: the shell's own refusals never enter a
redirection, because the shell is a different process and its output was never in the substituted
slot.

The paragraphs below are the design as it stood before either lane; where they differ from the two
notes, the notes are what was built.

**In brief.** The shell has no `|`, `>`, or `<`, and a shell without them is not a shell. The
surprise on investigating is that **the mechanism is already built** and the missing piece is
somewhere else entirely: the work is unifying the four byte-sink protocols we already have, after
which the pipeline operators are parser changes and wiring.

**Why it matters.** Pipelines are Unix's composition primitive and the reason the shell is worth
having. They are also the place where the capability model gets to show a result that is *better*
rather than merely equivalent, which is what a demonstrator exists to produce.

#### The finding: stdout is already a capability in a slot

`patches/std-cricker/.../pal/cricker/rt.rs` fixes `STDOUT_SLOT = 1`, and `sys/stdio/cricker.rs`
implements `println!` as a SEND on that slot. So **a program's output destination is a capability the
spawner chose**, and redirection is putting a different capability in that slot. No kernel change, no
new object, no `dup2`. The existing doc comment even anticipates the case: a failed SEND is swallowed
so "a program without a console still runs, it just prints into the void".

The same is true at the other end of the design. `line_editor::proto::OP_BYTES` already documents
`the rendezvous is the flow control`, which is exactly a pipe's back-pressure story.

#### A pipe is an endpoint, not an object

For `a | b`, the shell creates an endpoint, grants SEND to `a` as its output slot and RECV to `b` as
its input slot, and spawns both. **Nothing is added to the kernel.** Unix needs a pipe object with a
64 KB buffer because fds are anonymous and the kernel has to decouple two parties who cannot name
each other; here the shell names both, so the rendezvous is the pipe.

The cost is honest and should be measured rather than argued: this is **full lockstep**, where Unix's
buffer lets a producer run ahead. The reply is that IPC speed is the thing this project has spent
itself on, so measure `a | b` throughput and only then decide. If buffering earns its place it
arrives as **a component that speaks the sink protocol on both sides** and is inserted into the
chain, not as a redesign. An optimization that is a process is the shape a microkernel wants.

#### SIGPIPE becomes a return code, the same way `SIGTTIN` disappeared in 48

`yes | head`: `head` exits, its endpoint dies, and the producer's next SEND fails. Unix needs a signal
because there is no other way to tell a writer that an anonymous fd is gone; §16 revocation already
destroys the capability and the failure arrives as an error return. **A third signal disappears**, on
the same grounds milestone 48 retired `SIGTTIN` and `SIGTSTP`: the question the signal answered is
already answered by who holds what.

This forces one concrete change. Today's swallow ("print into the void") is right for a program with
no console and **wrong for a pipeline**, where a dead reader must end the writer. So the sink protocol
needs a distinguishable "gone" versus "never had one", and std's `Stdout::write` must stop discarding
the result.

#### The actual work: four sink protocols, one needed

| Sink | Protocol today |
|---|---|
| std `println!` | SEND, register-only, 16 bytes/msg, w0 = len, w1\|w2 = bytes |
| `line_editor` (crate and component) | CALL, shared page, `OP_WRITE`, r0 = bytes consumed |
| `fs_proto` | CALL, handle + offset + shared page, `WRITE` |
| console server | shared page, SEND length, ACK on a separate reply endpoint |

Four shapes for "write these bytes there". A child cannot be indifferent to what is in its output
slot until they are one, and **that unification is the milestone**. The precedent is
`fs_file_caretaker`, which is a caretaker precisely because it "serves the same `fs_proto` protocol
its own client speaks": narrowing preserves the protocol, so a pipe, a file, and a terminal become
substitutable.

#### The result that is better than Unix, and worth stating plainly

`>` and `file:PATH` must stay **different mechanisms**, and the difference is the payoff. `file:report.txt`
grants the *program* a file its manifest declared it wanted (milestone 31, a capability shell; the
filesystem contract it grants against is §27). `> report.txt` substitutes the
*stream the shell owns*, and the program never holds a file capability at all: it cannot seek, cannot
truncate, cannot re-read, cannot stat. It can append bytes to a sink. **Unix hands the same program fd
1 with full file semantics**, so our redirection grants strictly less than Unix's while doing the same
job.

Keeping them distinct is also what keeps the manifest meaningful: `run worker 5 > out.txt` must not
become a way to route around `FileSpec::Forbidden`, and it does not, because the sink is not a file
grant.

The demo that shows it: **`caps` can print where your output goes** ("output: terminal" versus
"output: file report.txt, append-only"), because the destination is a capability rather than an
integer with a convention attached.

#### What is genuinely missing

- **stdin.** `sys/stdio/cricker.rs` returns honest EOF because "nothing grants a std program input
  yet". Both `< file` and a pipe's read end need an input-slot convention that does not exist.
- **`>>`.** Append is expressible with `FSTAT`/`SIZE` then write-at-offset, but that is racy if the
  file is shared. Decide whether append is a mode on open or a sink property; do not over-solve it.
- **Multi-stage pipelines.** `a | b | c` is two endpoints, not one, and the shell's spawn path builds
  one child at a time today.

**Sequencing.** The protocol unification is independent of 47 and 48 and could start now; the parser
work wants 47's tokenizer changes, and the "who holds the terminal" story is shared with 48, so the
natural order is unify the protocol, then `>` and `<`, then `|`, then revisit 48's `fg` with pipelines
in hand. **Effort: 3 lanes estimated** (protocol, redirection, pipelines), noting estimates for
unbuilt work are guesses on a history-calibrated scale, and that the unification is the one most
likely to surprise.

### 51. Wall-clock time, the `date` command, and an NTP service

**Lane A built 2026-07-30** (the two RTC drivers and the clock service; DECISIONS §43,
notes/clock.md). The machine knows what time it is, on both ISAs, and `SystemTime` is real. What the
build settled that this block left open, and one place it went somewhere the block did not predict:

- **The three authorities are three different objects, and only one is a message.** Reading is a
  **read-only mapping of the clock page** (two loads and an add, no syscall, no server); setting is
  the **same page mapped writable**; proposing is the endpoint. Nothing new in the syscall surface.
  The block imagined all three as capabilities without saying what kind; a process has one blocking
  wait point, so two message-borne authorities would have needed two servers.
- **Discovery is by `compatible`, not by node name**, because the aarch64 board calls the node
  `pl031@9010000` and the RISC-V one calls its RTC `rtc@101000`. `dtb::node_reg_compatible` is new
  for it, and the kernel passes the *binding* to the driver so the register layout comes from the
  machine rather than from `target_arch` (the VisionFive 2 is riscv64 with neither device).
- **The unknown state is the default**, since a zeroed page reads as `UNKNOWN`. Its one uncomfortable
  consequence: `SystemTime::now()` has no error channel, so an unknown clock is a **panic**, and std
  has no way for a program to ask before it asks. Recorded in DECISIONS §43 as a limit, not a win.
- **Still open:** nothing in this milestone. The timed-wait fork below is recorded here but is a
  kernel-surface decision of its own, tracked separately and not a milestone-51 deliverable.

**In brief.** The machine does not know what time it is, and says so in a way that is easy to miss:
`SystemTime` is the monotonic counter offset from `UNIX_EPOCH`, so **it reports January 1970 plus
uptime**. Give it a real clock, a `date` command, and a network time client, and take the chance to
put the authority in the right place.

**Why it matters.** Time is where a capability system gets to make a distinction Unix cannot afford
to. **Reading the clock is near-harmless; setting it is a genuine authority** over certificate
validation, log ordering, filesystem timestamps and build reproducibility. In Unix `ntpd` runs as
root and may set the clock to anything. Here the network client should not be able to set it at all.

#### The starting position, which is honest but wrong

`notes/std.md` already records the caveat: "`SystemTime` is monotonic-since-boot, not wall-clock. No
RTC, no NTP, so 'system time' honestly measures 'since this machine came up'." Differencing two
`SystemTime`s gives a correct duration; any absolute reading is a fiction. It is also why file times
are `Unsupported` in the `std` PAL, per that file: "there is no wall clock to interpret it against
anyway". Milestone 47's `mkdir`/`create` work will want them, so this unblocks that.

#### The time source, and it is two drivers because parity is a gate

Verified from the DTB fixtures in `crates/dtb/tests/fixtures/`, not assumed:

| Platform | Device | Address |
|---|---|---|
| QEMU `virt`, aarch64 | `arm,pl031` | `0x9010000` |
| QEMU `virt`, riscv64 | `google,goldfish-rtc` | `0x101000` |
| VisionFive 2 | its own RTC (board bring-up, milestone 16a) | via DTB |

Two small drivers, both discovered through `crates/dtb` rather than hardcoded, both following rule 2
(a driver takes a base address and knows nothing else). Neither is large; the point is that shipping
one and not the other is the bug rule 5 exists to catch.

#### The design: an offset, which makes NTP safe for free

Keep the split the code already has. `Instant` stays the **raw monotonic counter**, ambient and
one instruction (see the §10 exception recorded in `arch/aarch64/timer.rs` and its riscv twin).
Wall-clock time becomes **counter + offset**, and the clock service owns the offset.

The payoff is that adjusting the wall clock cannot perturb monotonic time, **by construction rather
than by discipline**. Unix needs `adjtime` slewing partly because stepping the clock backwards breaks
things that assumed it only moves forward; here `Instant` never sees the adjustment, so a step is
just an offset write. Whether to *also* slew for the benefit of wall-clock readers is then a policy
choice the service can make, not a correctness requirement.

#### Where the authority sits

- **The clock service** holds the RTC device capability and the offset. It is the only thing that can
  set the time.
- **Readers** hold a read capability. Nearly everything.
- **The NTP client** holds a network capability and a capability to **propose** a time, which is
  deliberately not the same as setting one. The service applies policy: sanity bounds, a maximum
  step, and a refusal to move backwards past a threshold.

That attenuation is the milestone's demonstrable claim, and it is one Unix cannot make: a compromised
NTP client here can lie *within the service's bounds* and can do nothing else. It cannot set the clock
to 2038, and it holds no authority over anything but the network socket it was given.

#### `date`, the deliverable

**Built 2026-07-31** (notes/date.md; `user/src/date.rs`, `kernel::user::date_tests`).

**Reachable from a test, not from the prompt, and that is why this milestone is `PARTIAL` and not
`BUILT`** (found by Chris, 2026-07-31, by typing `date` at `script/server` and getting "unknown
command"). The binary is in the initrd and tested on both ISAs, but `grant_plan::Prog` knows only `worker`,
`budgeter`, `heeder` and `spinner`, so the shell cannot spawn it. The lane deferred that as
"milestone 31's manifest machinery", which is a defensible scope call that nonetheless leaves the
*command* half of "the `date` command" undone. **A program a user cannot invoke is not a command**,
and the status said otherwise until he checked. Being folded into the milestone 47 grammar lane, which
owns `grant_plan` and is removing `run`: after which `date` at the prompt is exactly what he typed. A hundred
lines, most of them comments, because the design had settled everything interesting first: read the
page, add the counter, hand the number to `calendar`. Five formats, a fixed UTC offset in minutes,
and an optional second line naming the clock's **provenance**, which renders `clock_proto`'s four
states for a person and is a distinction no Unix `date` can print. Three things the build is worth
recording for:

- **The absence of `date -s` is a fact about the wiring, not a missing flag.** It holds a read-only
  mapping, so there is no argument it could take and no method it could call. That is the claim
  below made concrete rather than asserted.
- **The unknown clock is a sentence**, with the two causes told apart (`the machine has no clock it
  believes` / `this process holds no clock capability`), because `date` has an error channel where
  `SystemTime::now()` has only a panic. Falsified before it was believed: removing the state check
  makes it print `Thu 1970-01-01 00:00:04 UTC`, and the test catches exactly that string.
- **It closes DECISIONS §43's own scope note** that "the unknown-clock path is not proven in the
  guest". That reasoning was about the machine (both QEMU boards have a working RTC) and the thing
  under test is the page: a frame nobody has published to *is* that machine, as far as any reader
  can tell, so the test allocates one and grants it.

Reads the wall clock and formats it. Timezone and calendar conversion are pure computation and belong
in a host-tested library crate, not in the service (§14's rule about what compiles for the host).
Setting the time is a separate verb with a separate capability, and `date -s` in one binary that does
both is exactly the conflation this design refuses.

**The library half is built** (2026-07-30): `crates/calendar`, host-tested and Kani-proved, holds the
civil-date arithmetic, the weekday and day-of-year, five formats and an RFC 3339 parser, and depends
on nothing in this milestone. Eleven harnesses, ten of them over the full ten-thousand-year range.
Two scope calls are recorded in notes/calendar.md rather than left ambiguous: **a fixed UTC offset is
in and the IANA tzdata is out** (zone rules are a data-distribution problem, not a calendar one), and
there is no `strftime`, five named formats instead. The command itself still waits on the service. The
lane also produced a verification finding worth more than the crate: a 64-bit division by 86,400 is
what bounded model checking chokes on here, not the calendar, and the `&str` boundary costs more than
the parser behind it (notes/verification.md).

#### NTP, and the chicken-and-egg worth recording before it is discovered

Buildable today: `net_stack` runs smoltcp, and NTP is UDP on port 123. Two honest problems:

1. **Plain NTP is unauthenticated and trivially spoofable.** NTS (RFC 8915) is the answer, and it
   needs TLS, which needs certificate validation, which needs **a roughly correct clock**. The
   standard escape is a build-time "not before" timestamp plus the RTC's rough value, and it should
   be chosen deliberately rather than discovered halfway through.
2. **The RTC may be wrong or absent**, so the service needs a defined state for "I do not know what
   time it is" rather than confidently reporting 1970. That state should be visible to readers, not
   papered over, which is the same rule §42 sets for filesystems: no silent degradation.

**The wire format is built** (`crates/ntp_proto`, notes/ntp.md): the 48-byte NTPv4 packet, the
1900-epoch fixed-point timestamp with a fixed era pivot for the 2036 rollover, the offset and delay
arithmetic in modular form so an exchange across that rollover comes out right, and the seven
response checks that are the whole of plain NTP's spoofing resistance. Host-tested and Kani-proved
(the era pivot over all 4.2 billion seconds from 1970 to 2104; parse/serialise and the origin-nonce
check over all 2^384 packets). Problem 1 above is **recorded, not solved**: the crate is
unauthenticated NTPv4 and says so in its own documentation, NTS stays a separate decision, and the
crate does not implement half of it.

**The client is built** (2026-07-31, `user/src/ntp.rs`, notes/ntp.md). It holds **five capability
slots and none of them is the clock page**, so the block's claim above is now a fact the machine
enforces rather than a design intention: `an_ntp_client_holds_no_writable_clock_page` gives the same
binary the same five slots plus the exact address a *setter* maps the page at, and it faults. Four
things the build settled or found:

- **`propose::STATE` is how a client with no mapping reads the time**, which is what the contract
  crate put it there for. One round trip to anchor against the monotonic counter, and the
  unknown-clock bootstrap falls out with no branch: the service answers 0, T1 and T4 are measured
  from 1970, and the proposal lands on the server's time.
- **The nonce is one draw from the entropy service, and its absence is a refusal.** No capability
  means no request at all, not a fallback to the counter-seeded stream, because
  `Query::with_nonce`'s 64 bits are worth nothing if they are guessable (§42's rule, §44's source).
- **A kiss-o'-death is not retried** while an ordinary rejection is, which is a property of the
  client rather than of the crate, and the test counts requests to prove it.
- **It is a one-shot synchroniser, not a continuously polling service,** because the timed-wait fork
  below is unsettled. A
  poll interval is a yield-spin; adding a sleep syscall to get a real one would settle that fork by
  accident. Three attempts a couple of milliseconds apart, one proposal, exit.

The test server is a second role of the same binary holding `READ` on the endpoint the client holds
`WRITE` on, so the client's network path is substituted **at the capability boundary** and its code
has no test-only branch. What that leaves unproven is recorded rather than glossed: smoltcp, UDP and
the NIC are milestone 30's to prove, and nothing in slirp answers UDP 123, so there is no offline
real server to point a gate at.

#### The fork this exposes, which is bigger than the milestone

There is **no timed wait anywhere in the kernel**. The syscall surface is `EXIT`, `YIELD`, `INVOKE`,
`CAP_DELETE`, and `sched.rs` twice calls out its own "no-timeout limitation". So `thread::sleep` is a
yield-spin, which is the *correct* implementation given what exists (it does not monopolise a core),
but it keeps a thread runnable for the whole sleep and costs scheduler work proportional to duration.

Three candidate shapes, and this is a design fork to settle before building:

- **A new `SYS_SLEEP` syscall.** Simplest, ambient, and not capability-shaped, which is a strike.
- **A timer object with a `WAIT` method.** Capability-shaped and consistent with the model; the most
  machinery.
- **A deadline on `Endpoint::RECV`/`CALL`.** One primitive that fixes sleep, the RECV no-timeout
  limitation the kernel already complains about twice, and the shell's `^C` busy-poll that
  `linedisc`'s `OP_INTRCOUNT` doc describes as waiting for "the blocking notification primitive".
  **Three problems, one addition**, which is why it looks strongest.

Worth separating clearly: *reading* time is ambient and harmless, *blocking* on time is a scheduler
interaction and is the part that wants a capability.

**Sequencing.** The RTC drivers and the clock service are independent of the shell milestones and
could start any time; `date` follows the service; NTP follows `date` and wants the network stack
settled. The timed-wait fork is separable and should be decided on its own, since it serves more than
this milestone. **Effort: 3 lanes estimated** (drivers plus service, `date` plus the calendar crate,
NTP), noting estimates for unbuilt work are guesses on a history-calibrated scale.

### 52. Subshells without `fork`, and what copying an endowment means

**STATUS: RECORDED, NOT DESIGNED.** Chris asked for this to be captured as a milestone *and*
explicitly asked to design it together. This block lays out the problem, the options and the
constraints; **it deliberately does not choose.** Do not build from it without that conversation.

**In brief.** `( commands )` is `fork(2)`: Unix runs the group in a copy-on-write duplicate of the
whole process, so changes to variables, the working directory, options and descriptors evaporate on
exit. We have no `fork`, on purpose, and cannot get one cheaply. So the question is what replaces it.

#### Why there is no fork, and why that is not a gap to fill

Spawning here is **build-from-parts**: retype an address space and a TCB, map pages, insert
capabilities, configure, start. That is what makes a cricker-os process a lighter object than a Unix
one, which is a claim the benchmarks rest on. `fork` would need copy-on-write duplication of an
address space *and* duplication of a capability space, neither of which exists.

It is also a primitive with a serious case against it: Baumann, Appavoo, Krieger and Roscoe, **"A
fork() in the road" (HotOS 2019)**, argues `fork` is a poor abstraction, not merely an expensive one.
It does not compose with threads, breaks buffered I/O and locks, is a security hazard, and its
semantics are defined by what Unix happened to be able to implement. Plan 9's `rfork(flags)` is the
better-known fix: choose per-resource what is shared and what is copied, which is *exactly* the shape
of the question below.

#### Most of what subshells are used for, milestone 50 already answers

Worth establishing before designing anything, because it shrinks the problem a lot:

| Unix subshell use | Answered by |
|---|---|
| Each side of `a \| b` | Milestone 50: the shell spawns two children and grants an endpoint. No subshell needed |
| `$( ... )` command substitution | Milestone 50: a pipe whose reader is the shell |
| `( ... ) &` backgrounding a group | Milestone 48: a job is what the shell holds capabilities for |
| **`(cd /tmp && make)`** | **Nothing. This is the residue** |
| **`(umask 077; ...)`, `(set -e; ...)`** | **Nothing, and `umask` is void anyway (§39-era: no permission bits)** |

So once 50 lands, the remaining need is **scoping**, not process duplication.

#### The conflation, which is this project's recurring pattern

`( ... )` means two different things that Unix could not separate because `fork` was the tool it had:

- **Scoping**: run this with a temporarily different working directory / variable / option, and put it
  back. Almost every real use.
- **Isolation**: run this so that *arbitrary* effects cannot escape. Rare, and the only one that
  actually needs a separate process.

That is the same shape as `mv` conflating rename with copy-and-unlink (§42), and `rm` conflating
unlink with revoke (milestone 47). Separating them has been the right answer twice.

#### The question with no Unix analogue: duplication is not total

If a subshell is a real child granted "a copy of the parent's endowment", then **what is a copy of a
capability set?** This is the part that needs design, and it is genuinely new.

- Some capabilities duplicate harmlessly: a read capability to a directory.
- Some **cannot** be duplicated, and we have already proved it. §41 gave `Frame::REVOKE` take-back
  semantics on a `DeviceFrame` precisely because **a device must never have two owners**; milestone
  23's whole witness is that the version never goes backwards. A one-shot Reply capability is
  similarly not copyable: it is consumed once by construction.
- So **"copy the endowment" is not a total function**, and any fork-like design needs a defined rule
  for the rest: refuse the subshell, silently omit those capabilities (a silent downgrade, which §42
  forbids), or require the parent to name what crosses.

There is a promising fit with machinery that already exists. If a child's capabilities are
**derived** from the parent's rather than duplicates of them, then §16's revocation and the derivation
tree already give "destroying the child revokes exactly its copies", and §40's supervisor-death-is-
subtree-death makes cleanup automatic. That is an argument for derivation over duplication, and it is
the first thing to test in the design conversation.

#### The options, none chosen

1. **Simulate in-process.** The shell saves its own mutable state (working-directory capability,
   variables, options), runs the group, restores. Cheap, no new mechanism, and correct *exactly* when
   effects are confined to shell-local state, which covers `(cd x && y)`. It cannot undo anything a
   command did to a capability, so it is a lie for the isolation case.
2. **A real child shell** granted a derived endowment. Honest isolation; costs a full spawn for
   `(cd /tmp && ls)`; and forces the duplication question above to be answered.
3. **Scoped bindings instead of subshells.** `with cwd = /tmp { ... }` says what it means rather than
   reaching for process duplication because that was the available tool. Earns its divergence under
   milestone 47's rule only if it genuinely covers the uses; it does not cover isolation.
4. **Hybrid**: scoping by binding, isolation by an explicit verb, so the two uses stop sharing a
   syntax.

**Open questions for the conversation**, in the order they probably matter: does derivation beat
duplication; what happens when an endowment contains a non-duplicable capability; is the isolation
case common enough to build for at all once 50 lands; and does `( ... )` keep its Unix spelling if it
means something materially different.

**Sequencing.** After milestone 50 (pipes and redirection), because 50 removes most of the
requirement and changes what is left. **Effort: not estimated**, because the design is not chosen and
the options differ by more than an order of magnitude.

### 58. RISC-V TLB shootdown, and the flush that makes ASIDs pointless

**In brief.** `write_satp` follows every `csrw satp` with a bare `sfence.vma`, so **every RISC-V
context switch throws away the entire TLB** while carrying an ASID it then gets no benefit from. The
fix is not deleting the instruction; it is building what has to exist first.

#### This is a parity gap, not a design question

aarch64 already does it right: `set_ttbr0` writes the register and flushes nothing, and a separate
`flush_asid(asid)` is documented as "the teardown half of the ASID contract (crates/asid): after
this, and only after this, the number may tag someone else." `crates/asid` is Kani-proven and its own
header states the intent: "a context switch stops flushing anything". **RISC-V simply does not use
the machinery that is already built and already proven on the other ISA.**

#### Why it is not a one-line deletion

- **`sfence.vma` does not broadcast, and that is the whole milestone.** aarch64's `TLBI` invalidates
  across every core in hardware. RISC-V's `sfence.vma` affects only the hart that runs it, so
  flushing an ASID machine-wide means an IPI to every hart, each running its own `sfence.vma`, and an
  acknowledgement before the number may be reused. **That is a distributed protocol with real races,
  and getting it wrong is silent**: stale translations mean one process reading another's memory with
  no crash to announce it.
- **The free path must flush per-ASID** (`sfence.vma x0, asid`), which today does not exist at all.
- **The `satp.ASID` width must be checked**, which is now done: `mmu::asid_bits()` probes it at boot
  and `the_hardware_has_at_least_the_asid_bits_the_allocator_assumes` fails loudly below 8. **Removing
  the flush must be gated on that number.**

#### The thing to understand before touching it

**The unconditional flush is currently load-bearing for correctness, not merely slow.** `satp.ASID` is
WARL and RISC-V permits *zero* implemented bits; `crates/asid` hands out 255 numbers on the stated
assumption that even the smallest hardware ASID space is 8 bits, which is true of aarch64 (mandated)
and **not guaranteed by RISC-V**. On a core with no ASID bits, all 160 address spaces would carry
ASID 0 and their TLB entries would alias. Nothing has bitten us because the flush discards every
entry before it can. Delete the flush without the probe gating it and the failure mode is
cross-process memory disclosure.

#### The trade, stated plainly

The **win** is a full TLB flush removed from every RISC-V context switch; `ctx_switch` is paying for
it now and would show the improvement. The **risk is asymmetric**, and should drive the sequencing:
the upside is a benchmark number, the downside is silent memory disclosure. So the shootdown gets
**proven, not argued**, and it is why milestone 19's test lane correctly left this alone rather than
taking it as a side effect of writing tests.

**Sequencing.** The probe is done (2026-07-31). Next is the per-ASID flush, then the IPI shootdown
with its acknowledgement, then removing the flush behind the probe's gate, then re-baselining
`ctx_switch`. **Effort: not estimated**; the shootdown is the unknown, and it is the kind of unknown
that deserves measurement before a number.

### 59. The CPU-model matrix: stop testing against one generous emulator

**Status: BUILT (2026-08-01).** `script/cpu-matrix` runs the riscv64 suite against `rv64`,
`sifive-u54`, `rva22s64`, `rva23s64` and `thead-c906`; **211 tests pass on every one**, so the cheap
experiment below came out the reassuring way and "we are already portable to the board's ISA" is now
measured rather than predicted. `script/test` grew `--arch` and `--cpu`, both defaulting to today's
behaviour, and CI grew a `cpu-matrix` job of its own. The full result, the preflight that keeps the
matrix from being theatre, and an honest BUGS list are in [notes/cpu-models.md](../notes/cpu-models.md).
The one thing it did **not** de-risk is the ASID width: every model reports 16 implemented bits, so
the test written for the board still has no machine that can fail it.

Raised by Chris on 2026-08-01, asking whether we should modify QEMU to match
the chip, detect features, or something else.

**The answer to the first is no.** A forked emulator is a machine that exists nowhere: it proves
nothing about the real chip and nothing about the standard emulator, which is the worst of both. We
also pin QEMU for benchmark determinism (`.qemu-version`, and CI builds it from source), so a fork
multiplies that maintenance. QEMU already lets us **narrow** rather than patch, and narrowing is the
whole milestone.

#### What we actually run against today

`qemu-system-riscv64 -machine virt -cpu rv64 -bios default`. **`rv64` is QEMU's maximalist model**: it
enables essentially every ratified extension QEMU implements. The VisionFive 2's JH7110 is a SiFive
U74, which is **RV64GC**. So the emulator will accept things the board will not, and every RISC-V
result we have was taken on the permissive one.

#### The reassuring part, stated before the worrying part

We build for **`riscv64imac`**: no `F`, no `D`. RV64GC is IMAFDC. **We are already a strict subset of
the board's ISA**, so the compiler cannot emit an instruction the U74 lacks. That is a real result
and it narrows this milestone considerably.

What it does **not** cover is the part that is hand-written: `asm!` in `arch/riscv64/`, CSR reads and
writes that QEMU may implement more permissively than SiFive, and implementation-defined widths. That
is the exposure, and it is exactly the class a narrower `-cpu` catches.

#### The work

Run the existing suite against more than one CPU model. QEMU ships **`sifive-u54`** (the U74's
family) and the profile models **`rva22s64`** and **`rva23s64`**; `thead-c906` is a useful hostile
case because it is a real chip with real divergences.

**This reframes what parity means.** Today parity is two ISAs (DECISIONS §19). With hardware arriving
it should be *the same suite across CPU profiles*, because "aarch64 and riscv64 both pass" stops being
the strongest available claim once we know riscv64 was only ever tested on the friendliest model.

#### Why this comes BEFORE discovery (milestone 60)

Because it needs no discovery to run, and **what it breaks tells us what is worth discovering.**
Building an `Isa` record first means guessing which facts matter; running the matrix first means the
machine names them. That is the same posture as the ASID probe and the device-tree-pointer correction:
measure, then write down what the measurement said.

The cheap experiment is one command and it may well pass, in which case the result is "we are already
portable to the board's ISA", recorded with the evidence.

#### BUGS

- **`sifive-u54` in QEMU is still QEMU.** It will not reproduce the JH7110's cache behaviour, its real
  memory map, or its errata. This catches the ISA-and-CSR class and is not a substitute for the board.
- **A green matrix is not a portable kernel.** It is the absence of one specific class of failure.

**Effort: small**, and it is the highest ratio of de-risking to work of anything before the board
lands (~2026-08-21).

### 60. ISA discovery: read the machine instead of assuming it

**Status: NOT-STARTED.** The gap found while answering milestone 59's question: **nothing in the tree
reads the ISA.** No `riscv,isa`, no `riscv,isa-extensions`, no `mmu-type`. We run on what the target
triple implies plus exactly one runtime probe.

#### Why the device tree, and why there is no shortcut

**RISC-V deliberately has no `CPUID`.** `misa` exists but is coarse, is permitted to read as zero, and
says nothing about post-2015 extensions. The architected answer is the device tree
(`riscv,isa-extensions`, `mmu-type` for Sv39 versus Sv48) plus SBI for firmware-provided facilities.
We already parse DTB (`crates/dtb`), so this is parsing plus somewhere to put the answer.

#### The shape, and the trap

**One `Isa` record, populated once at boot, printed at boot.** The trap is `if isa.has_x()` sprouting
across the kernel, which turns a fact into a hundred branches. The places that genuinely vary are few
and nameable: TLB flush strategy, ASID width, Sv39 versus Sv48, IOMMU presence. Keep it to those.

**Do not build a chip-abstraction framework on one board.** CLAUDE.md's rule against speculatively
trait-ifying applies with force here: the second real board should tell us what the abstraction is.
One record and four call sites is not a framework, and that is the point.

#### Discovery has three tiers and we should be explicit about which is which

1. **The device tree** declares what firmware claims.
2. **A targeted probe** measures what the silicon does. `probe_asid_bits()` (built 2026-07-31) is the
   pattern: write ones, read back what stuck.
3. **Trap-and-detect** executes an instruction and catches the illegal-instruction fault. Last resort,
   needs the exception path, and we should not need it.

**Keep the probes even once the tree is parsed.** The tree is a claim and the probe is a measurement,
and when they disagree the machine wins. That is not a hypothetical here: this project has already
been wrong about a QEMU boot register it believed the documentation about.

#### Truthfulness (§42's habit, applied to hardware)

If something required is absent, **say so and stop**, rather than running degraded and reporting
success. §42 makes a filesystem declare what it offers and be honest about it; a kernel that silently
assumes Sv39 on an Sv48 machine is the same violation one layer down.

#### BUGS

- **Discovery does not make us portable**, it makes us honest. Knowing an extension is missing and
  doing something useful about it are different milestones.
- **The device tree can lie**, or firmware can describe a machine it is not. Tier 2 exists for that.

**Effort: not estimated.** Parsing is small; how many call sites genuinely need to vary is the unknown,
and milestone 59 is what answers it.

**What 59 answered, 2026-08-01: zero, on the five CPU models QEMU offers.** The suite passes
unchanged from `sifive-u54` (bare RV64GC) to `rva23s64` (vector, `zicond`, pointer masking), so
nothing in the kernel currently needs to branch on a discovered fact. That does **not** retire this
milestone, and the reason is the sharpest thing 59 found: **QEMU reports `satp.ASID: 16 bits
implemented` on every model**, including `sifive-u54`. The one place we already know a real chip may
differ is the one place no emulator can tell us about, so discovery's value is not the branching, it
is being able to say what the machine is instead of assuming it. See
[notes/cpu-models.md](../notes/cpu-models.md).

### 61. The caretakers: one verb table, and names that say what you get

**Status: BUILT, both ISAs.** Three pieces, three commits, in the order below.

**In brief.** The **rename** first, because these files were being touched anyway: `fwarden` ->
`fs_file_caretaker`, `dwarden` -> `fs_subtree_caretaker`, `swarden` -> `fs_nameset_caretaker`,
`cwarden` -> `c_confiner`, `cshim` -> `c_shim`, `conx`/`cconx` -> `rust_swappable`/`c_swappable`,
`await_*` -> `wait_for_*`, and the C symbols to `c_seam_*`. That is 532 tokens rather than four
filenames, and `c_confiner` deliberately did not take the caretaker noun in its prose. Then
**`fs_proto::verb`**: one row per opcode saying what a request's words mean and which rights the
server demands, with `const assert!`s that make a verb without a row a **compile error**; the three
caretakers dispatch off it and stop being three hand-written matches. Then **extended-attribute
forwarding**, the gap that raised the milestone, with three witnesses: a per-file grant that reads
its file's attributes and cannot write them (and a writable twin that can, as its control), the
three subtree rights configurations one bit wider, and a name-set grant that reads its file's
attributes and still cannot name the entry beside it. Notes: fs-server.md, dir-capability.md,
glob-grant.md, xattr.md, grant-expression.md, naming.md.

**What the table does not share is the attenuation**, which is what let the three programs stay three
programs after the refutation below. A lookup that picks a length or a zero cannot refuse anything,
so `fs_subtree_caretaker` still performs no checks at all.

**One thing found rather than built, recorded in `verb::file_grant::POLICY`'s BUGS and in
notes/grant-expression.md:** writing the per-verb rows down exposed that `fs_file_caretaker` answers
`EBADF` to every directory verb except `CREATE`, because they all fell through one `_ =>` arm shared
with "you named a handle I never minted". `ENOTDIR` is very likely right for all seven by exactly the
argument `CREATE` already makes. Behaviour was preserved, because changing it changes the wire.

**Renamed and rescoped 2026-08-01** after Chris asked why there are three of these and whether we
expect more. Investigating that **refuted the collapse this milestone was first drafted around**, and
the refutation is worth keeping because it is already argued in the tree.

#### The collapse that does not work

The three serve near-identical verb surfaces (`subtree` and `nameset` are identical at 18 verbs), so
"one program parameterized by how the namespace is described" looks obviously right. It is wrong, and
`swarden.rs`'s own header carries a section titled *"Why this is a third warden and not a mode on the
second"* saying why:

**`dwarden` performs no checks at all, and that is its design.** One `OPENDIR` at startup, with the
server intersecting the granted rights and minting a restricted handle; everything after is reached
through that handle, so the attenuation lives in what the server minted rather than in any branch. A
name filter is a check, consulted on **every** name-taking verb. Adding a mode would trade that
program's one strong property for a switch, and put a forget-a-verb surface in the program that most
deliberately has none.

So the two serve the same verbs **by opposite means**. They stay separate.

`fwarden` is different again: it translates between two *protocols*, directory in and file out, which
is why it must inspect. Its narrow 11-verb surface is deliberate, and the tell is the errno.
`CREATE` answers `ENOTDIR`, not `EACCES`, because a file capability is not a directory: the request
does not mean anything, rather than meaning something that was refused. **The verb surface is part of
what the capability is**, not a filter over a wider one.

#### What the milestone is

1. **A verb table in `fs_proto`**, so a verb is taught once rather than three times. This survives
   the refutation: the duplication is real even though the programs must stay distinct. Today a new
   verb is simply absent from a caretaker and the capability silently is not there, which is exactly
   how the xattr gap happened.
2. **Extended-attribute forwarding**, the gap that raised this. All three answer `EOPNOTSUPP`, so a
   program behind a per-file grant cannot read its own file's attributes.
3. **The rename**, because these files are being touched anyway and doing it twice is worse.

#### The rename

`warden` is a synonym we invented for a pattern that has a name. `DECISIONS.md` §31 already cites
the right one, **caretaker** (Mark Miller's term), while the code says warden; §50 settled that
using the existing name claims "this is that", and inventing a synonym asserts novelty where there
is none.

Names say **what the holder ends up able to do**, so a reader can predict the surface without opening
the file:

| Current | Proposed | A reader should predict |
|---|---|---|
| `fwarden` | `fs_file_caretaker` | a file; cannot list or create |
| `dwarden` | `fs_subtree_caretaker` | a directory and everything under it |
| `swarden` | `fs_nameset_caretaker` | exactly these names, in one directory |
| `cwarden` | `c_confiner` | **not a caretaker**: holds a region and confines foreign code |

`dwarden` is the one that buys correctness rather than clarity: it is named for what it **holds**,
while both siblings are named for what they **serve**, and since all three hold a directory the
current name distinguishes nothing.

#### Settled 2026-08-01

- **The family noun is `caretaker`.** Settled when Chris chose `wait_for_caretaker` over
  `await_warden`: a helper cannot be named for a pattern its callees are not. §50's rule is the
  reason (use the name the literature already has; a synonym asserts novelty where there is none),
  and `DECISIONS.md` §31 has cited Miller's term correctly since milestone 31 while the code said
  warden.
- **The `await_*` helpers become `wait_for_*`.** `await` reads as async/await, which this project
  rejected at a design fork, and there is no async here. Four of them travel together
  (`wait_for_service`, `wait_for_caretaker`, `wait_for_compositor`, `wait_for_ready`) plus the
  `warden_ready` parameter, because three renamed and one not is worse than either consistent state.
- **`wait_for_caretaker`, not `wait_for_caretaker_ready`.** It waits for the caretaker to be
  *serving* rather than to exist, and the shorter name does not say so; the doc comment carries that
  precision. Taken because the whole family shares the ambiguity and resolves it the same way, and
  parallelism with `wait_for_service` is worth more than the extra word.
- **`cwarden` becomes `c_confiner`**, out of the caretaker family entirely: it holds a **region**
  and confines foreign code rather than attenuating a directory capability to a narrower one.

#### The names, settled 2026-08-01

| Current | Settled | A reader should predict |
|---|---|---|
| `fwarden` | **`fs_file_caretaker`** | a file; cannot list or create |
| `dwarden` | **`fs_subtree_caretaker`** | a directory and everything beneath it |
| `swarden` | **`fs_nameset_caretaker`** | exactly these names, in one directory |
| `cwarden` | **`c_confiner`** | not a caretaker: holds a region, confines foreign code |

**The `fs_` prefix is the resolution of a real objection rather than decoration.** Chris raised that
"subtree" means three things around here: `supervision_proto` *is* the supervision tree, `CLAUDE.md`
uses "the tree" throughout to mean this repository, and git has its own `subtree`. The first answer
was to put the disambiguation in the doc comment. Carrying it in the name is strictly better, and
`fs_subtree` cannot be misread as either of the others.

`fs_` and not `file_`, because `file` is already one of the qualifiers and `file_file_caretaker` is
the reductio. It is also **not a new convention**: `fs_proto`, `fs_server` and `fs_service` already
use `fs` as this project's filesystem marker, so this applies an existing one where it was missing.

An earlier draft of this block settled on bare `file_` / `subtree_` / `nameset_`, on the objection
that a domain prefix breaks parallelism. Chris's answer removed the objection rather than ignoring
it: apply the prefix to **all three**. That also leaves the four programs on one scheme (domain,
then what it serves, then what it is) instead of two unrelated ones, and it groups them in `ls`,
which matters in a `user/src/` holding 48 programs and no subdirectories.

**Why these qualifiers.** `file_` rather than `one_file_`, because cardinality is not the
interesting property: you cannot enumerate at all, so "one versus few" never arises. `nameset_`
rather than `glob_`, because §52 records that a BFS-style query result and a glob result are the
**same object** granted by the same attenuation, so the name is about a designated set of names and
globbing is merely its only caller today. `subtree_` rather than `directory_`, because all three
**hold** a directory capability, so naming one of them for what it holds distinguishes nothing, which
is the exact defect `dwarden` has and this rename exists to fix.

**Two costs, recorded rather than discovered later.** `fs_subtree_caretaker` and
`fs_nameset_caretaker` are 20 bytes against `crickerfs`'s archive limit (`NAME_LEN`), which was 24
when this was written, so four bytes of headroom and a four-part name would not fit; that constraint
was load-bearing and is what led to raising the limit to 32 on 2026-08-01 (notes/crickerfs.md).
And `fs_file_caretaker` says filesystem twice, which is the price of the scheme being uniform.

The rename also resolves an inconsistency already in the source: `dwarden.rs`'s header says
"attenuated to one **subtree**" while its second paragraph says "narrows it to one **directory**".

#### The C-seam family converts in the same pass (Chris, 2026-08-01)

| Current | Settled |
|---|---|
| `cwarden` | `c_confiner` |
| `cshim` | `c_shim` |
| `crates/c_seam` | already done, 2026-08-01 (rule 7) |
| `user/c/c_seam.c` | **`user/c/c_seam.c`**, and this one is a repair |

**That last row fixes a split the integrator created.** `c_seam` was chosen over `c_abi` partly
*because* it keeps the Rust and C halves paired, and then only the Rust half was renamed when rule 7
turned it into a crate. So the pairing argument is currently false in the tree: `crates/c_seam`
faces `user/c/c_seam.c`.

The pairing is not cosmetic. Both files state the same constants **by hand**, because a C compiler
cannot see Rust, and `crates/c_seam`'s test reads the C source with `include_str!` to prove they
agree. Two names that no longer match make the duplication look accidental rather than mechanical,
which is exactly what that test exists to contradict. **Renaming the C file means updating the
`include_str!` path**, and the test failing is how a mistake there would announce itself.

`user/build.rs`'s `C_SOURCES` table names both the source and the program it compiles into
(`("c/c_seam.c", "cshim")`), so it changes on both counts in one edit.

#### The live-replacement pair, settled 2026-08-01

| Current | Settled |
|---|---|
| `conx` | **`rust_swappable`** |
| `cconx` | **`c_swappable`** |
| `user/c/conxsvc.c` | **`user/c/c_swappable.c`** |

`conx` was the most opaque name in the tree: **no recorded expansion anywhere**, not §41, not
`notes/live-replacement.md`, not the commit that introduced it. These are milestone 23's swappable
component in two implementations, and a client that does not notice the swap.

**`rust_` breaks a precedent deliberately, and the exception is the point.** When `c_seam` was
settled, the argument was that Rust is the constant and the foreign language is the variable, so
naming only the variable is economical: it is why there is no `rust_kernel` or `rust_shell`. That
holds where the language is **incidental**. Here it is the **subject**: this pair exists so that a
Rust component can be replaced by a C one while `chatty` keeps calling, and the language is the whole
reason there are two of them.

Symmetry does real work too. `swappable` plus `c_swappable` would read as *the* swappable one and a
C variant, implying a default. That is actively wrong: `conx` is the incumbent and `cconx` the
replacement **only until the swap**, after which the roles invert. Neither is the default, and a
symmetric pair says so.

**One cost, recorded so nobody infers a family that is not there.** `c_` will then mean "written in
C" across two unrelated milestones: `c_shim`, `c_seam` and `c_confiner` are milestone 36's
foreign-language seam (§31), while `c_swappable` is milestone 23's replacement demo. The prefix
means the same thing in both cases; the milestones are not related. Worth a line in
`notes/naming.md`.

#### BUGS

- **A table is a new place to be wrong, and a wrong row is wrong in three programs at once.** It is
  pure data in a host-testable crate, so Kani and host tests can reach it, which a hand-written match
  in a `no_std` binary cannot.
- **It does not make the caretakers interchangeable**, and after the refutation above it must not
  try to. Only the verb dispatch is shared; what each attenuates to stays hand-written.

**Effort: medium.** The table is small; teaching three programs and proving each on both ISAs is the
work, and the rename touches roughly 180 references.

#### The draft this replaced

The original framing, kept because the refutation above is only legible next to it. Chris asked on 2026-08-01 whether xattr support in the wardens deserved a
milestone. It does, but the useful milestone is the general one, and the xattr gap is its proof.

#### The immediate gap

`fwarden`, `dwarden` and `swarden` answer `EOPNOTSUPP` to all four extended-attribute verbs
(milestone 57). A program behind a per-file grant cannot read its own file's attributes. That is
uniform and §42-honest, and it is still a capability the confined program should have.

#### The general problem, which is why this is a milestone and not a chore

**Each warden is a hand-written `match` over the verb.** Milestone 57 added four verbs, so closing
this means twelve new match arms across three programs, and **the next contract addition will cost
the same again.** The contract is around twenty verbs now. Nothing makes a warden and the contract
agree, so the way this fails is that a new verb is simply absent from a warden and the capability
silently is not there. That is exactly how the xattr gap happened: the verbs landed, the wardens were
not taught, and nothing failed.

#### Why "just forward everything" is the wrong fix

Worth stating plainly, because it is the obvious idea and it is a security hole. **The enumeration is
doing real work.** `fwarden` substitutes its own handle for the caller's, refuses anything that is not
`grant::HANDLE`, and enforces direction so a read grant cannot write. A blind proxy would forward the
caller's handle and hand back the wide capability the warden exists to attenuate.

#### The shape

**A verb table in `fs_proto`**: each verb declares its argument shape (does it name a handle, a name,
a length?) and the right it requires. The warden's loop becomes generic over the table, and adding a
verb becomes **one row in the contract** rather than three match arms in three programs.

This inverts the failure mode, which is the actual deliverable. Today, forgetting a warden yields a
capability that is quietly missing. With a table, a verb with no row is a build failure, and a verb a
warden should refuse is an explicit row saying so, which is a decision somebody wrote down.

#### This is not the speculative abstraction CLAUDE.md warns against

The rule is not to build an abstraction before the requirements are known. We now have three wardens,
about twenty verbs, and a fresh instance where a whole contract addition reached none of them. That is
the second data point, not a guess about the future.

#### Scope

The table, the three filesystem wardens, and xattr forwarding as the thing that proves it. Each
warden needs its own answer for the write verbs: a read-only grant must not forward `SETXATTR`, and
that is per-warden policy rather than something the table decides.

**`cwarden` stays out.** It confines a C component and is not a filesystem proxy; it shares the name
and not the mechanism.

#### BUGS

- **A table is a new place to be wrong, and a wrong row is wrong in three programs at once.** The
  mitigation is that it is pure data in a host-testable crate, so it is reachable by both host tests
  and Kani, which a hand-written match in a `no_std` binary is not.
- **It does not make the wardens interchangeable.** They differ in what they attenuate to, and that
  stays hand-written; only the verb dispatch is shared.

**Effort: medium.** The table is small; teaching three wardens and proving each on both ISAs is the
work.

### 62. Tests that assert on time: make a red run mean something

**Status: NOT-STARTED.** Raised 2026-08-01, from evidence rather than from taste.

#### The problem

A population of tests assert on **elapsed time or on a fixed number of yields**. `sched.rs` alone
holds about nineteen `for _ in 0..N { yield_now() }` spins, and the shape is always the same: give
the scheduler N chances, then assert something happened. `threads_round_robin` gives twenty yields
and asserts every thread ran at least once. `ticks_arrive_at_the_configured_rate` and the riscv
timer-drift assertion compare guest ticks against elapsed counter time.

None of them is wrong about what it wants to prove. All of them fail when the host is busy, because
a yield is not a guarantee and the guest's clock is the host's clock.

#### Why this is worth a milestone rather than tolerance

**It makes a real regression invisible.** On 2026-08-01 it cost the integrator three separate
diagnosis cycles, and two of those ended in the wrong conclusion before being re-run. The credentials
lane hit three flakes, the xattr lane two, the CPU-matrix lane two, and the integrator hit three more
in different tests each time. A suite that fails for reasons unrelated to the change trains everyone
to re-run rather than to read, which is the exact habit that lets a genuine failure through.

**Milestone 59 multiplies it fivefold.** The CPU matrix runs the same suite five times, so every
timing test now has five chances per run to be unlucky, on a shared CI runner nobody controls.

**And the honest diagnostic we rely on is expensive.** The current rule is "a green run under load is
conclusive, a red one is not, so re-run quiet." That works, and it costs a full suite run every time,
and it depends on a human remembering to apply it.

#### What the fix probably is, per class

- **The bounded spins** are the easy majority. Waiting for a condition with a deadline is a different
  thing from taking N turns: the test wants "eventually", not "within twenty yields". An
  event-driven wait, or a bound expressed in **guest ticks** rather than host-scheduler turns, makes
  them insensitive to what else the machine is doing.
- **The genuinely temporal tests** cannot be made deterministic and should not pretend to be.
  `ticks_arrive_at_the_configured_rate` is *about* the clock. These want an explicit, stated
  tolerance and a recorded retry budget, so a flake is a documented cost rather than a surprise.
- **A third class may want to move off the emulator entirely.** Scheduling policy is pure logic and
  some of it could be host-tested against a simulated clock, which is where this project already puts
  logic it wants to check in milliseconds.

#### BUGS

- **Fixing this cannot be verified by running the suite once.** A flake that fires one run in six is
  indistinguishable from a fixed one until you have run it many times, so the acceptance evidence is
  a repeat count, not a green run.
- **Deleting the timing assertions would be worse than the flakes.** `ticks_arrive_at_the_configured
  rate` is the test that catches re-arming the timer from `now()` inside the handler, which is a real
  bug this project has a comment about. The goal is tests that fail only when something is wrong, not
  fewer tests.

**Effort: not estimated**, and deliberately: the count is known (~19 spins plus a handful of clock
assertions) but how many are mechanical and how many need a rethink is not.

### 63. Directory and package names: one spelling per thing

**Status: BUILT, 2026-08-01, both ISAs.** Raised 2026-08-01, after `fsserver` was fixed and the
survey behind it found the rest.

**Every table and paragraph below keeps the OLD spellings**, because this block is the record of the
decision and a name's argument is unreadable once the name it argued against is gone. Everywhere
else in the tree carries the new ones. What landed, and the three things that did not, are in
[notes/naming.md](../notes/naming.md).

#### The standard, which is derived rather than invented

The naming tenet (CLAUDE.md) covers crates, programs, modules, shell entry points and markdown. It
says nothing about **directories**, and the tree has three spellings as a result.

The rule that already fits the tree and needs only to be written down:

- **A directory that holds a Rust package is named exactly as the package**, so `snake_case`. Thirteen
  of the multiword directories under `crates/` already do this (`fs_proto`, `dma_validate`,
  `supervision_proto`).
- **Any other directory is lowercase, and hyphenated if it needs two words**, the same convention as
  markdown filenames and `script/` entry points, because a directory is a path element and paths are
  hyphenated in the world outside this repository.

That is not a new tier. It is the existing "each domain keeps its own convention" applied one level
out: a package directory is a Rust name, and everything else is a path.

#### Crate renames settled in review (2026-08-01)

| Now | Settled | Why |
|---|---|---|
| `shell` | **`swish`** | the shell gets a proper name rather than a category. See the block below. |
| `capsh` | **`grant_plan`** | it plans grants from a command line; `sysinit` executes them, and that boundary is real. Not named for `swish`, because **seven things use it** (`swish`, `sysinit`, `rm`, `heeder`, `hello`, `fs_nameset_caretaker`, `kernel/src/user.rs`), so naming it for one consumer repeats `dwarden`'s defect. `designation`, `designate` and `designator` were considered and rejected together: **the user designates by typing a name**, and this crate's job starts after that, so all three put it in a role it does not hold. Synonyms of grant (`endow`, `award`, `confer`, `bestow`, `allot`, `furnish`) were rejected because "grant" is already this tree's word and a synonym is a decoder ring; `endow` is additionally taken by `supervision_proto::Endow`. |
| `vterm` | **`display_terminal`** | the tree's own phrase for it, verbatim from its header: "The display terminal (milestone 29, the display ladder's text)." Deliberately **not** the same name as its crate, unlike `compositor` and `line_editor`: the crate is named for the **protocol** it implements (the VT standard, bytes in and a grid out) and the program for its **role** (the terminal on the display), and both facts are worth keeping. It also sits next to `display`, the virtio-gpu driver it is a client of, so the display ladder reads straight from the filenames. `text_console` was rejected because `console` is already a program. |
| `sysinit` | **`system_initializer`** | `system_builder` is unavailable and the collision is the real defect: `builder` already calls itself "a minimal init: **the system builder**", so two programs claim the same phrase. `session_initializer` was rejected for squatting milestone 49's vocabulary, since sessions arrive with users and login and this program manages none. `shell_init` was rejected on evidence: `sysinit` looks the shell up **by name in the initrd**, so it brings up whatever is packed as `shell` rather than `swish` specifically, and it also stays alive as the **spawn service**, which is not a shell concern at all. |
| `elbench` | **`os_primitives_benchmarker`** | **25 bytes: this one requires `NAME_LEN` to be raised first** (see below). `el` was aarch64-only vocabulary for a program that runs on both ISAs, since RISC-V has U/S/M-mode and no exception levels. `os_primitives` is §14's own phrase and disambiguates: in a kernel tree, bare "primitives" reads as *synchronization* primitives. `benchmarker` rather than `benchmark` because **this tree already uses "benchmark" for the output**: `bench/baseline.txt` holds the committed measurements and `notes/benchmarks.md` is about the numbers. The agent noun names the producer, distinct from the product, and joins `broker`, `spawner`, `painter`, `budgeter`, `compositor` and `credentialer`. (`coremark` is not a counter-example: it is a proper noun, EEMBC's industry benchmark.) |
| `vnet` | **`net_transport`** | named for its **role**, not the device class, because naming it `virtio_net` would collide: `crates/virtio` also drives net. It is the adapter that presents smoltcp's `phy::Device` so frames can cross the virtqueue, which is a different job from the driver underneath. Same principle that made `display_terminal` beat `video_terminal` for the program: distinguish by what a thing does, not by what hardware it touches. |
| `rootsup` | **`root_supervisor`** | 15 bytes. `sup` was the abbreviation, so `root_sup` would have relocated the problem rather than fixed it. |
| `subsup` | **`sub_server_supervisor`** | 21 bytes. Exact where `sub_supervisor` is ambiguous: it supervises **a sub-server**, rather than being a supervisor beneath another one. "Sub-server" is established vocabulary, 44 occurrences across `DECISIONS.md`, `supervision_proto`, the kernel and the notes, so the name is built from a word a reader has already met. |
| `allocdemo` | **`allocator_exerciser`** | `alloc` is the crate it *uses*; the allocator is what it *proves*. It wires `user_rt::heap::UntypedHeap` and shows freed memory is reusable rather than leaked. **`exerciser`, not `demo`**: see below. |
| `user-std/` + package `hellostd` | **`std_exerciser`** (directory and package) | the worst mismatch in the tree: directory and package disagreed and neither described the contents. It is "the std proof: an ordinary Rust program, no `no_std`, running on the native capability ABI", one binary whose three behaviours are chosen by the authority it was granted. Same milestone 27 as the allocator one, so they are siblings by construction and now read as the pair they are. |
| `credential` | **`credentialer`** | an agent noun in the `broker`/`swapper`/`painter` family, and a **real profession**: a credentialer verifies licenses against records they hold and never hands the record back, which is this service exactly. I argued for the plain noun on the `clock`/`entropy` resource pattern and was wrong twice: `credentialer` is not a coinage, and **this service will never give you a credential**, so naming it for the resource implies the one thing it exists to refuse. |
| `credcli` | **`credentialer_test_client`** | 24 bytes, exactly the current cap, comfortable once `NAME_LEN` moves. |
| `fsclient` | **`fs_test_client`** | see the note below on why all three carry `test`. |
| `netcli` | **`socket_test_client`** | a client of the socket contract (`socket_proto`), which is what its own first line claims. It drives three fixed exchanges against QEMU user-mode networking (slirp's built-in TFTP, a real DNS query that leaves the machine and is therefore non-gating, and a TCP echo round trip) and reports `OK` or a stage code so the kernel test fails loudly rather than hanging. |
| `netstack` | **`net_stack`** | same: `net` is already this tree's word. |
| `dma_validate` | **`dma_validator`** | it calls itself "the DMA-confinement **validator**" in its own first line; the name simply did not. |
| `measure` | **`measured_boot`** | "measured boot" is the standard term for boot-time hashing, so this gains the guard-rail benefit too: a reader who knows secure-boot vocabulary recognises it. |
| `compose` | **`compositor`** | the noun, and accurate about scope: the whole compositor problem (scene, clipping, damage arithmetic, contract), not just one of them. `compositor_proto` was rejected earlier because this is a logic crate, not a wire contract; `scene` undersells the arithmetic. **Sharing a name with the program is the point, not a collision**: the crate is that program's logic lifted out to be host-testable, and `coremark` and `lineedit` already do exactly this. |
| `lineedit` (crate **and** program) | **`line_editor`** | the crate's own header calls it "a sans-IO **editor**", and there is no `Editor` type to stutter against (it exports `proto`, `expand_output` and the `OP_*` constants). `line_discipline` was rejected as overclaiming: that term covers the whole tty layer including echo, canonical mode, signals and flow control, and this crate is narrower. `line_edit` was rejected for being a verb phrase where its sibling `video_terminal` is a noun. |
| `uheap` | **`user_heap`** | the `u` was *userspace*, and `user_rt` already establishes `user_` as the prefix for it. |
| `vt` | **`video_terminal`** | the true expansion: DEC's VT100 and VT220 were **Video** Terminals. `virtual_terminal` was proposed and rejected as wrong twice, since that is not what VT stood for and "virtual terminal" already names Linux's virtual consoles. `screen_grid` was rejected because the crate carries 63 escape-sequence references: the grid is the output and interpreting the protocol is the work. Chris's reason for expanding rather than keeping `vt`: a reader can relate it back to the thing they already know, with less ambiguity than two letters that could read as *vector table* in a kernel. |
| `caps` | **`capability`** | the crate is the capability *model*, not a container: it exports `Cap`, `Rights`, `Object`, `Reap` and `CSpace`. `cap_space` was considered and rejected because it names one of five exports and stutters as `cap_space::CSpace`. `CSpace` itself stays: it is seL4's own spelling. |

These are folded in here rather than given their own milestone because a crate rename is a
directory rename, which is what this milestone already is.

#### `swish`: the shell has a name now (Chris, 2026-08-01)

`shell` is a category, not a name. `bash`, `zsh`, `fish` and `rc` are names; this project's most
demonstrable artifact was filed under the noun for what kind of thing it is.

**`capsh` was the obvious candidate and is unavailable.** Linux's libcap ships `capsh(1)`, a
"capability shell wrapper" for testing POSIX capabilities, which is adjacent enough that a reader who
knows Linux capabilities would assume ours is that tool.

**Why `swish` rather than something descriptive.** Shell names are identities rather than
descriptions: `fish` describes nothing and nobody minds. But this one happens to carry the thesis
anyway, which is the combination shell names almost never manage.

A swish is the basketball shot that goes through the net **touching nothing**. That is least
authority in one word: the command reaches exactly what it designated and nothing else. `wc
report.txt` touches `report.txt` and not one thing more, and it does so structurally rather than by
a check that could be wrong.

It also reads as a shell on sight, because the `sh` is built in, the same trick `bash` plays with a
pun.

**`sheesh` was considered and set aside on two grounds**, both recorded because they are the kind of
thing that is obvious only once said. It carries a timestamp: the word spiked as a meme around
2020-21, where `bash` and `fish` are era-neutral, and this project expects to be shown off years from
now. And *sheesh* is an interjection of **exasperation**, while this shell's most characteristic
behaviour is **refusing things** by design. The name and the experience would have pointed the same
direction, and "the shell that says no" reading as a complaint is a risk a name should not carry for
free. `swish` inverts both: a precision word on a precision property.

Two wrinkles, neither disqualifying: Sweden's mobile payment system is called Swish (different
domain, no confusion in a terminal), and `swish` contains "wish", which is faintly the wrong idea for
a system where you do not ask for authority, you hold it.

#### The three that violate it

| Now | Should be | Severity |
|---|---|---|
| `fs-server/`, package `fs-server` | `fs_server/`, package `fs_server` | consistent with itself, inconsistent with the other 37 crates |
| `tools/redoxfs-host/`, package `redoxfs-host` | `tools/redoxfs_host/`, package `redoxfs_host` | same |
| `user-std/`, package **`hellostd`** | one name, spelled once | **the real defect** |

**`user-std` is the one worth doing even if the others are deferred.** The directory says one thing,
the package says another, and the package name is squished besides. Neither name describes what is
in it: `user-std/src/main.rs` is "the std proof (milestone 27): an ordinary Rust program, no
`no_std`, running on the native capability ABI", which is one of this project's better
demonstrations and is currently filed under a name that suggests a hello-world.

#### Why this is its own milestone rather than part of 61

Milestone 61 is already moving about 532 tokens plus eight programs, and a directory rename touches
roughly forty files by path. Two renames in flight would collide in `notes/`, `DECISIONS.md` and
`kernel/src/user.rs`, which is exactly the avoidable collision CLAUDE.md has three rules about. **This
starts after 61 lands.**

#### BUGS

- **A hyphenated package name is not wrong in the wider ecosystem**, and that is the argument against
  doing this at all. `wasm-bindgen` and `tracing-subscriber` are ordinary, Cargo normalises a hyphen
  to an underscore for `use`, and nothing is broken today. The case for the change is internal
  consistency (37 crates against 3) rather than correctness, and it should be weighed as such.
- **`target/` and `targets/` sit next to each other** and mean unrelated things: build output, and the
  custom target JSON specs (`aarch64-unknown-cricker.json`). Nothing enforces the distinction and one
  is gitignored while the other is tracked. Worth folding in.

#### `exerciser`, not `demo` (Chris, 2026-08-01)

**"Exercise" is this tree's own verb**, 130 uses across it, and these programs use it about
themselves: "exercises the capability-shaped contract", "exercises the platform", "every line
exercises a PAL surface". `demo` was never the word they reached for.

It is also **real systems vocabulary** rather than a coinage: memory exercisers, bus exercisers and
disk exercisers have meant "a program that puts a subsystem through its paces" for decades, which
puts it in the guard-rail category of terms a reader already knows.

And it is more precise. A demo *shows something off*; an exerciser *puts it under load and sees
whether it holds*. `allocator_exerciser` does interleaved allocation and free in arbitrary order,
drop-and-reuse, and a final large allocation that must fit in pages already committed, proving freed
memory is genuinely reusable. Its own header calls that "the allocator **workload**".

It is an agent noun, so it joins `broker`, `spawner`, `painter`, `credentialer` and `benchmarker`,
which is where the noun rule points.

**This category is distinct from the `_test_client` trio and the distinction is real.** A client
exercises a **service contract from the outside**, with a server on the other end; that is what
`client` means in those names. An exerciser demonstrates a capability of the system in itself, with
no contract being probed from a client side. `std_test_program` was considered and rejected for
importing the clients' vocabulary into the wrong family.

#### Why the three clients carry `test` (Chris, 2026-08-01)

`fs_client`, `credentialer_client` and `socket_client` are **the names the real things will want**,
and the real things are coming: milestone 55 needs an actual credentialer client for SMB
authentication, milestone 54 needs actual socket clients, and any program that wants files is an FS
client. Giving those names to test programs squats them, and the bill arrives later as a rename or as
something worse like `real_fs_client`.

It also fails the tenet's own test. `fs_client` predicts "a client of the FS service", not "the
program the kernel spawns to prove the FS contract holds". The qualifier is the distinguishing fact,
not noise.

I argued the opposite first, for consistency with two names already recorded here. That was
consistency in the wrong direction: three names consistently squatting the good ones.

**`witness` was the alternative and is not a coinage**, which is worth recording since I first called
it insider vocabulary and was wrong. It is standard in proof theory (the concrete object
demonstrating an existential claim), in model checking (a counterexample trace, the world Kani and
CBMC already live in here), and in cryptography (the zero-knowledge witness, Bitcoin's SegWit). The
tree already uses it: "the extended-attribute witness", "witness pages". It was set aside because
`client` carries real information about what the program *is* that `witness` does not, and because
this project's stated audience arrives from Linux rather than from formal methods.

#### Raise `NAME_LEN` FIRST, because one rename now depends on it

**DONE, 2026-08-01, ahead of the rename and on its own merits.** `NAME_LEN` is 32, `ENTRY_LEN` 40,
`DIR_BLOCKS` 6, `MAX_FILES` 76 (up from 63), and the magic is `CRKR0002`. `os_primitives_benchmarker`
fits with seven bytes to spare, so the rename below is unblocked. **One thing in the paragraphs below
was wrong and is worth reading before trusting them:** the kernel-stack cost had already been
retired, because `Fs` stopped holding a fixed entry array when the FS-server stack bug was fixed, so
the raise was much cheaper than the trade described here. The measured numbers and the reasoning are
in [notes/crickerfs.md](../notes/crickerfs.md). The paragraphs are kept as written because the
decision to do this first, rather than under pressure from a name, is the part that generalises.

`crickerfs` caps archive names at 24 bytes, and **three naming decisions have crowded it while a
fourth exceeds it**: `fs_subtree_caretaker` at 20, `sub_server_supervisor` at 21, and
`os_primitives_benchmarker` at **25, which does not fit at all**.

That makes this a **prerequisite rather than a tidy-up**, and the ordering matters: raise the cap on
its own merits, with the costs below written down, and *then* land the rename. Choosing a worse name
to fit a limit, or raising a limit because a name demanded it, are both the wrong way round. Three bytes of headroom is not a
budget, it is a trap, and discovering it a third time as a build error during an unrelated change is
the expensive way to find out.

It is a real trade rather than a free win. `NAME_LEN` sits inside `ENTRY_LEN = 32`, so widening it
costs directory entries per block (`MAX_FILES` is 63 at `DIR_BLOCKS = 4`) and it costs **kernel
stack**, because `Fs` holds `entries` as a fixed array that is a stack local in the boot and spawn
paths. The FS server was once found to have died 528 bytes short of stack, so this is not headroom to
spend casually. There is no data migration, because every image regenerates from the crate.

Do it here, with the numbers written down, rather than under pressure from a name that will not fit.

**Effort: small**, and almost entirely mechanical, but it touches paths in `script/`, `xtask`,
`deny.toml`, CI, and a long tail of notes.

### 64. Enough `std` to run somebody else's crate

**Status: NOT-STARTED.** Raised 2026-08-01, from a question with a number behind it: does milestone
27 mean ordinary Rust programs run here?

#### What 27 actually delivered, and where it stops

`std` on the native ABI is **BUILT**, and the proof program is real: `println!`, `Vec`, `String`,
`Instant`, `SystemTime` and `std::random` all work through the PAL in `patches/std-cricker/`.

The bound is in the PAL's own answers:

| module | functions | answering `Unsupported` |
|---|---|---|
| `time` | 8 | 0 |
| `stdio` | 5 | 3 |
| `thread` | 6 | **4** |
| `fs` | 54 | **32** |

`std::fs` has the metadata surface (`size`, `perm`, `modified`, `is_dir`, `read`, `write`, `append`,
`truncate`) and answers `Unsupported` for most of the rest. **That is honest rather than broken**
(§42: declare what you offer), and it is exactly what milestone 27's own text claims: it widens real
workloads to *"most of crates.io **that stays off fs and threads**"*. The qualifier is doing the work
in that sentence, and this milestone is about removing it.

#### Why now rather than at 27

The pieces that were missing then exist now. The FS service and its wire contract (§27), the three
caretakers and their verb table (§56), extended attributes (§54), and `fs_test_client`'s worked grant
path all landed after 27 did. `std::fs` could not have been backed by a capability-shaped filesystem
that did not yet exist.

And it is on the critical path in a way the roadmap does not currently say: **milestone 55 wants
Samba-shaped code**, and nothing realistic in that space stays off `fs` and threads.

#### How to scope it, which is the whole method

**Do not fill in functions by guessing which matter.** Pick real crates, build them, and let the
failures name the work. The gap that matters is the one a chosen dependency actually hits, and a PAL
completed by inspection would be a large amount of code justified by nobody's use.

Candidate probes, roughly in order of how much they would teach:

- a pure-computation crate with no IO, to establish the floor,
- a serialization crate, which pulls in `alloc` patterns and trait-heavy generics,
- something that opens a file by path, which is where **the capability question bites**: `File::open`
  takes a path and this system has no ambient authority, so either the PAL resolves against a
  granted directory or the call must keep answering honestly,
- something that spawns a thread, which is the other half.

**The `File::open` question is a design fork, not an implementation task**, and it should be raised
before code is written. §50 chose `bind` over stored paths and §48 settled resolution; how a
`std::fs::File::open("config.toml")` finds its directory capability, or refuses to, is the same
question one layer up. It may be that the honest answer is a program namespace (milestone 47's `PATH`
analysis) rather than a PAL trick.

#### The relationship with milestone 47, in both directions

**64 needs 47, in tiers rather than all at once.**

- **Tier one, a bare name against one granted directory**, needs nothing from 47's remaining work.
  `File::open("config.toml")` where the program holds a directory capability resolves the way
  `fs_test_client` and the caretakers already resolve names, on machinery that exists: §27's
  contract, §47's rights ladder, §56's verb table.
- **Tier two, anything that traverses**, needs a namespace to resolve *against*, and that is 47's
  unbuilt half. `Path::new("assets").join("x.png")`, an absolute path, or a program wanting two
  directories all land here.

So 64 can start and get a useful distance before it blocks. It will block **sooner than tier one
suggests**, because real crates rarely open a bare name in a single directory; they join paths.

**And 47 may need 64 more than the reverse.** `bind` is a decided mechanism with no forcing use case:
§50 records it as unbuilt, needing "a mount table per process and resolution through it", and nothing
in the shell strictly requires one. A `std` program calling `File::open` with a path is a **concrete
demand for exactly that machinery**. The same is true of `PATH`, where 47 concluded there is no search
because there is no ambient namespace to search, and that a program namespace **is** an endowment.
64 would be its first real customer.

**Sequencing that follows from this.** Run 64's measurement phase first and independently: pick the
probe crates, build them, let the failures name the work. It costs 47 nothing and produces the
evidence for how much namespace 64 actually needs, which is the question 47's remaining scope should
be sized against. **Then answer `File::open`'s resolution once, as a fork spanning both**, rather
than twice. Answered inside 64's PAL it will be a trick; answered as 47's namespace it is the design
both milestones already point at.

#### BUGS

- **"Runs unmodified" is the claim to be careful with.** A crate that compiles is not a crate that
  works, and a crate that works under one grant may fail under another, because on this system what a
  program can do depends on what it holds. The acceptance evidence has to be a crate doing its job
  with a stated endowment, not a green build.
- **The PAL patches std's own source**, so every function added here is more surface for
  `toolchain drift` to break against a future nightly. That is a real recurring cost and the reason
  to add only what a probe demands.
- **Threads open a scheduling question this project has not answered.** `std::thread::spawn` implies
  a thread the program owns; the kernel has TCBs and a budget model, and which of those a `std`
  thread is has never been decided.

**Effort: not estimated**, deliberately. The measurement is the first deliverable: pick the probes,
build them, and report what breaks.

### 65. A secrets service: hold the key, expose the operation, never the key

**Status: NOT-STARTED.** Raised 2026-08-01, from a question about MD4 and MD5 that turned out to be
asking something else.

#### The observation that reframes it

**NTLMv2 does not verify a presented secret.** The client never sends the password. The server takes
a challenge, computes `HMAC-MD5(NT hash, ...)` **itself**, and compares. So the NT hash is not a
verifier, it is **a key the server computes with**, and §54's shape (secret in, boolean out) does not
fit it at all.

The principle §54 states is still right: *hand out the operation, not the secret*. It is the
**operation** that was wrong.

#### What the service is

A process that **holds secrets and exposes keyed operations**, never the secrets:

| Secret kind | Operation exposed | Never exposed |
|---|---|---|
| Argon2id tag | `verify(presented) -> bool` | the tag |
| NT hash | `ntlm_response(challenge) -> response` | the hash |
| a future signing key | `sign(bytes)` | the key |

**The credentialer becomes one operation in this service**, not a separate thing. A second service
holding secrets is precisely what this design exists to avoid.

#### Why it is worth a milestone rather than a second operation on the credentialer

Because of what it does to the SMB server. **The NT hash never enters that address space.** The SMB
server holds an endpoint that computes responses; compromise it and an attacker can authenticate
sessions *while they hold the endpoint*, and cannot extract the hash, crack it offline, or carry it
anywhere else. Storing the hash in the SMB server offers none of that.

And **revocation already exists** (§32, §41): destroying the endpoint cuts a compromised server off.
A stored hash could never be taken back.

**Prior art, and it is strong.** This is what a TPM or an HSM *is*: hold the key, expose operations,
never emit the key. It is macOS Keychain's model and `systemd-creds`'s. Structurally it is
`libcasper` a third time, which is the convergence §31 already records: a process holding authority
its caller should not have, serving a narrow interface.

#### Secrets are scoped to resources, not identities

Chris's setup is the evidence: **each share has its own username and password.** That is a
credential per resource, which is a capability per resource, and it means this service **does not
depend on milestone 49's identity model**. Secrets are keyed by what they authenticate *to*, not by
who holds them.

That decoupling is deliberate and worth keeping: an identity model can arrive later and consume this
service, rather than this service having to wait for one.

It also bounds the damage from the NT hash's password-equivalence, structurally rather than by
policy. A leaked hash authenticates to **one share** and nothing else, because there is nothing else
it is the credential for. What normally makes password-equivalent storage dangerous is **reuse**, and
per-resource scoping cuts that at the root.

#### Dependencies

- **Entropy (§44, built).** Challenges, nonces and salts. Already wired.
- **Crypto as dependencies (§46).** `argon2` is in the tree; NTLM adds MD4, MD5 and HMAC-MD5, and
  §46's exposure argument applies to them exactly as it did to Argon2id.
- **Persistence, which is the real one.** The store is **memory only, provisioned at boot**
  (`notes/credentials.md`). A secrets service that survives a reboot needs the filesystem (§27), and
  that immediately raises secrets at rest, which Chris deprioritised for backup *data* but which is a
  different question for *keys*.
- **Revocation (§32, §41, built).**

#### Consumers

Milestone 55 (SMB needs `ntlm_response`, and is **blocked on this**), milestone 49 (login would use
`verify`), and anything later that signs.

#### BUGS

- **It does not protect against an attacker who holds the endpoint right now.** They can authenticate
  sessions for as long as they hold it. The claim is that compromise is *bounded and revocable*, not
  that the key is safe from a live intruder.
- **Shipping MD4 and MD5 is deliberate and is protocol compliance, not a security choice.** NTLMv2
  specifies them; implementing them says nothing about their strength, the way implementing DES to
  talk to old hardware would not. What matters is what is stored and what is claimed about it.
- **Three family members means at least three shares**, so multi-share is the deliverable rather than
  a later generalisation. A single-secret store would be discovered as wrong at the worst moment.

**Effort: not estimated.** The service shape is small; persistence and the at-rest question are not.

### 66. Vaultwarden: somebody else's real application, running here

**Status: NOT-STARTED**, and this is the **largest single item on this roadmap**. It is recorded as a
target rather than a plan, and its value today is that it converts "runs real workloads" from a claim
into a checklist.

#### Why this application

Vaultwarden is a Bitwarden-compatible server written in Rust: self-hosted, widely deployed, and the
kind of thing Chris actually runs. It is **not a benchmark or a demo**. Getting it working would mean
this system runs software written by people who have never heard of it, which is the difference §14
draws between a demonstrator and a curiosity.

It also lands on the same board as milestones 53 to 55. A VisionFive 2 serving the family's Time
Machine backups **and** their passwords is a home server, not an exhibit.

#### What is actually missing, measured

| Gap | State today |
|---|---|
| **TCP listen and accept** | **absent from the contract.** `socket_proto` has `OP_CONNECT`, `OP_SEND`, `OP_RECV`, `OP_CLOSE`. There is no way to be a server. |
| `std::thread` | 4 of 6 PAL functions answer `Unsupported` |
| `std::fs` | 32 of 54 answer `Unsupported` (milestone 64) |
| async runtime | none. Vaultwarden uses Rocket, which uses tokio: timers, wakers, and a reactor |
| TLS | none. `rustls` needs entropy (have it) and a large crypto surface |
| SQLite | a **C library**, so the §31 seam plus real filesystem locking |

**The listen/accept gap is the interesting one**, because it is a design question rather than missing
code. A listening socket is a *capability to accept connections on a port*, and `accept` mints a new
capability per connection. That is a genuinely new shape in this contract, and it is where the
capability model meets the server model for the first time.

#### Its relationship to the rest

- **Milestone 64** is the prerequisite and this is its extreme case. 64 measures with small probe
  crates; this is what the measurements are eventually for.
- **Milestone 65** is a different thing wearing a similar word, and conflating them would be a
  mistake worth naming: 65 is a secrets service **for the system** (keys the OS computes with);
  Vaultwarden is a secrets service **for a human** (passwords a person retrieves). Different layers,
  different threat models, no shared machinery.
- **Milestones 53 to 55** share the board and the thesis.

#### BUGS

- **This is a target, not a plan.** Every row in the table above is milestone-sized on its own, and
  several are unsequenced. Treating it as scheduled work would be dishonest about the distance.
- **"Runs Vaultwarden" is not one bit.** It could run with SQLite on a real filesystem and no TLS, or
  behind a TLS terminator, or single-threaded. **Which subset counts should be decided before the
  work starts**, or the goalposts will move to wherever the effort lands.
- **A capability system may not want to run it unmodified.** Vaultwarden expects ambient filesystem
  and network access. Running it here may mean granting it a directory and a listening socket and
  finding out what it does when it asks for more, which is a more interesting result than success.

**Effort: not estimated, and deliberately not.** The first honest deliverable is the sequence, not a
date.

### 67. `swish` the language: quoting, sequencing, and exit status

**Status: NOT-STARTED.** Raised 2026-08-02, from measuring `swish` against a minimal POSIX shell.

#### Where `swish` actually stands

It has `help`, `echo`, `caps` (the whole endowment, and a **preview** of what a command would grant),
`cd`, `pwd`, `ls`, `mkdir`, `rm`, program spawn with a file grant, `worker`, `budgeter --mem N`,
`date`, `wc`, **globbing**, **pipes**, and **redirection**. `ls | wc` and `ls > out.txt` run at a live
prompt on both ISAs.

**So it is an interactive shell without control flow.** The effort went into the
capability-interesting parts, composition and grants and navigation, which was the right order. What
is missing is the *scripting language*.

#### What this milestone covers, and what already has one

| Gap | Where it lives |
|---|---|
| **Quoting**: `"..."`, `'...'`, backslash | **here** |
| **Sequencing**: `;`, `&&`, `\|\|` | **here** |
| **Exit status**: `$?`, which `&&` needs | **here** |
| `>>` and `2>` | **here** (named unbuilt in `notes/pipes.md`) |
| Variables, assignment, `export` | milestone 47 (studied: "the same question wearing a string costume") |
| Job control: `&`, `jobs`, `fg`, `bg`, `wait`, `kill` | milestone 48 |
| Subshells, command substitution `$(...)` | milestone 52 |
| Scripts, `if`/`while`/`for`/`case`, functions | **nowhere yet, and deliberately** |

#### Quoting is the one that is not a convenience

**A filename with a space is currently unnameable.** That is a correctness gap in a shell whose whole
thesis is that *naming a resource is granting it*: a resource you cannot name is a resource you cannot
grant, so the gap lands squarely on the thing this shell exists to demonstrate.

It also interacts with globbing (§52's name sets) and with the grant planner: a quoted name must not
be glob-expanded, and an unquoted one must be. That is a parser change with a capability consequence,
which is why it belongs with the other two rather than being filed as polish.

#### Exit status is a capability question in disguise

`&&` needs to know whether the previous command succeeded. Programs already report through a result
endpoint, so the mechanism exists; what does not exist is `$?` at the prompt, or a decision about
**what a status means when the thing that failed was a refusal rather than an error**. `swish` refuses
constantly and by design (`Refusal::TooManyNames`, "you hold no such capability"), and whether a
refusal is a non-zero status or something else is a design fork, not an implementation detail.

#### BUGS

- **Scripting is not scoped here on purpose.** `if`/`while`/`for`/functions and reading a script file
  are a much larger thing, and this project has no story yet for what a script *is* when a program
  namespace is an endowment. Doing quoting and sequencing first is what makes that question
  answerable rather than theoretical.
- **The four gaps above are not independent.** `&&` needs exit status, and both want quoting to be
  settled first, or the parser gets rewritten twice.

**Effort: small to medium**, and mostly in `grant_plan`, which is host-testable, so most of it can be
proven in milliseconds without an emulator.

### The backup-server ladder (53 to 55), and why it is the right deliverable

Chris's goal, 2026-07-30: **the board should replace the drive hanging off his router as the Time
Machine target.** These three milestones are that goal decomposed honestly, and they are worth doing
for a reason beyond utility.

**It is a real workload with a real user.** Every other thing this project measures is a benchmark or
a test. This one gets used by people who did not write it, which changes what "works" means.

**And the stakes are exactly right, which matters more than they would be if they were higher**
(Chris, 2026-07-30). This is **not** his durable backup: **Borg handles offsite**, and the board's job
is protecting against short-term mistakes. So losing the whole thing costs the ability to undo a bad
afternoon, not any data. That is the ideal shape for a demonstrator target: **genuine use, tolerable
failure.** Putting an experimental capability microkernel in front of someone's only copy would be
reckless; putting it in front of their convenience layer is a real test with a bounded downside, and
the entry should not pretend otherwise to sound weightier.

**It still exercises crash consistency for real.** §34's RedoxFS conditions get tested against actual
power loss on actual hardware rather than a QEMU crash image, and correctness is still the goal. The
honest correction is only to the consequence of failure, not to the standard.

**And it is the best security claim the thesis can make, because backup servers hold everything.** On
a Linux box, Samba runs with broad authority over the machine. Here the file-serving component would
hold **one directory capability and one network endpoint and nothing else**, so a compromise reaches
the backup share and stops: not by policy, not by a hardening guide, but because no capability naming
anything else was ever given to it. That is worth more on a backup server than on almost any other
workload.

### 53. The board's own peripherals: network and storage on real silicon

**In brief.** Milestone 16a boots a VisionFive 2 (firmware contract, NS16550, PLIC, Sv39). It does not
give the board a network or a disk. Everything above needs both, and **this is where virtio stops
carrying us**: every driver we have talks to QEMU's paravirtual devices, and real silicon has none.

**What it needs.**

- **Ethernet.** The JH7110 uses a Synopsys DesignWare GMAC (`dwmac`). Our net_stack (smoltcp) is
  device-agnostic above the driver, so this is a driver, not a stack rewrite. Rule 2 applies: it takes
  a base address and knows nothing else.
- **Storage**, and there is a real choice here. The SD/eMMC controller is the simplest path; **NVMe
  over PCIe** is the better one, because §18's PCIe transport already exists and NVMe would give the
  backup target actual throughput. Deciding which comes first is a fork, and it should be decided on
  measurement of what the backup workload needs rather than on what is easiest.
- **Persistence proven the hard way.** RedoxFS on the real device, with crash consistency tested by
  **actually cutting power**, which is a test QEMU cannot run.

**The parity note this milestone must carry.** These drivers are board-specific and aarch64 has no
equivalent board yet, so rule 5's "a scope note records the gap and the plan" applies rather than its
"ships on every architecture". Say so explicitly; do not let it look like an oversight.

**Effort: not estimated.** Two device drivers against real hardware with no emulator to iterate
against is a different activity from everything done so far, and estimates calibrated on QEMU work do
not transfer.

### 54. A network file service a Mac can actually mount

**In brief.** The board serves files over a protocol macOS speaks natively, so it is useful before
Time Machine specifically is solved.

**The protocol choice is the whole decision, and it is not obvious.**

| Option | macOS support | Size | Note |
|---|---|---|---|
| **9P** | **None** | Small | Plan 9's protocol, closest to our model, and Chris cannot mount it. A demonstrator win with no user |
| **NFSv3** | Built in (`mount_nfs`) | Medium | RPC/XDR, mount protocol, portmapper. Usable immediately for general storage. **Not** a supported Time Machine target |
| **SMB3** | Built in | **Large** | **The one that is actually required**: the only path to Time Machine (milestone 55) |
| WebDAV | Built in | Small | HTTP-based, and not a Time Machine target |

**Chris's router already exposes SMB for Time Machine (2026-07-30), which settles this.** SMB is
required regardless, so NFSv3 would be work thrown away, and 9P would be a demonstrator exercise with
no user. **Do not build a second protocol just to have an easier first one.**

What survives is a better decomposition than "pick a protocol". **The file service already exists**:
`fs_proto` over RedoxFS, milestone 32. A network protocol is therefore an **adapter** that speaks the
wire on one side and `fs_proto` on the other, holding **one directory capability and one network
endpoint**. So this milestone is the adapter *pattern* plus whatever protocol milestone 55 needs, and
9P or NFSv3 become optional later adapters rather than prerequisites.

That framing sharpens the security claim rather than just simplifying the build. The SMB adapter is a
**protocol translator with no storage authority at all**: it cannot reach the block device, cannot
enumerate outside the share, and speaks to the FS server only through the same contract every other
client uses. A compromise yields the share's contents and nothing structural.

**The capability shape, whichever protocol wins.** The service holds the share's directory capability
and a network endpoint. It cannot enumerate outside the share because no capability reaches there;
milestone 47's `enumerate`/`open`/`create`/`remove` split is what expresses "this client may write
backups but not delete them", which is a genuinely useful thing to be able to say to a backup client.

**Effort: not estimated**, and it depends entirely on the protocol chosen.

### 55. Time Machine: SMB3 with Apple's extensions, and mDNS

**In brief.** The actual goal, and **probably the largest single piece of work in the project**. It is
recorded at full size deliberately, because the failure mode here is starting it while imagining it is
"a file server".

#### The reference implementation is known, and Chris supplied its exact configuration

**Chris's router is a GL.iNet GL-BE9300 (Flint 3) running OpenWrt, serving three family Time Machine
targets through Samba with `vfs_fruit` (2026-07-30).** So the reference is full Samba, not `ksmbd`,
and the working `[global]` stanza is on the record:

```
fruit:aapl = yes                 fruit:metadata = stream
fruit:time machine = yes         fruit:model = TimeCapsule
vfs objects = catia fruit streams_xattr
fruit:posix_rename = yes         fruit:nfs_aces = no
fruit:veto_appledouble = no      fruit:delete_empty_adfiles = yes
fruit:wipe_intentionally_left_blank_rfork = yes
```

That is a measured feature list rather than a guess, and it decodes into these requirements:

| Setting | What we must implement |
|---|---|
| `fruit:aapl = yes` | **The AAPL SMB2 create context.** The core of it: macOS negotiates Apple extensions on connect and will not accept the share without them |
| `fruit:time machine = yes`, `model = TimeCapsule` | Advertise the share as a Time Machine target and return the model string |
| `streams_xattr` + `metadata = stream` | **Alternate data streams**, for Finder metadata and resource forks. See below, this is the expensive one |
| `fruit:posix_rename = yes` | **Rename over an open file**, POSIX semantics |
| `catia` | Character mapping for names macOS permits and the backing filesystem does not |

#### The discovery that changes scope: we have no extended attributes at all

Verified, not assumed: **no xattr support in `fs_proto`, in the FS server, or in vendored RedoxFS.**
`streams_xattr` stores Apple metadata in NTFS-style alternate data streams backed by filesystem
xattrs, and we have neither layer.

**There is an escape, and it should be chosen deliberately rather than discovered late.** Samba's
`fruit:metadata` also accepts `netatalk`, which keeps the same metadata in **AppleDouble sidecar
files** (`._name`) needing no filesystem support whatsoever. Chris's router uses `stream` because ext4
has xattrs. So this is a **design choice between adding xattrs down the whole stack (protocol, FS
server, RedoxFS) and accepting sidecar files**, not the hard blocker it first appears to be.

#### `fruit:posix_rename` lands squarely on work already scoped

Rename over an open file, which is precisely the territory of §42 (a filesystem declares what it
offers and must be truthful) and milestone 47's `mv` section. Note the current state: **`fs_proto` has
no `RENAME` verb at all** and `rename` is `Unsupported` in the std PAL. So milestone 55 has a hard
dependency on that gap being closed, and §42's concurrency-versus-crash atomicity split is exactly the
distinction Time Machine's durability expectations will test.

#### Three users, and this is where the thesis gets a concrete demonstration

Chris's setup serves **graeme, corinne and chris**, one partition and one share each, and privacy
between family members rests on Samba correctly honouring a "Read-Write User = graeme" line in a
config file. A Samba bug, a misedit, or a path-traversal flaw crosses that boundary.

**Ours would be three adapter instances, each holding one directory capability**, and one adapter
**cannot name** another's partition. Not an ACL check that could be wrong: no capability, no path, no
way to express the request. That is the security claim of the whole project, stated in terms of
something Chris actually relies on, which makes it the best demonstration target on the roadmap.

It also means milestone 56's credential service holds **three identities**, not one, from the start.
(Built that way: the store's capacity is three, and the fourth `PUT` is refused with `FULL` rather
than silently replacing somebody, which is a thing the tests show.)

#### mDNS is required after all, measured 2026-07-30

I hoped this could be dropped, on the grounds that Chris adds the share manually and the SMB-side
`fruit:time machine = yes` might be what makes it acceptable. **Measured, and no**: `dns-sd -B
_adisk._tcp` on his network returns `GL-BE9300` in `local.`, so the router runs an mDNS responder and
advertises itself as a Time Machine target. The reference implementation does it, and the only way to
prove it *unnecessary* would be to disable it on a working family backup system, which is not a trade
worth making. **Assume required.**

So this milestone carries **two protocols**: SMB3 on TCP and mDNS/DNS-SD on UDP multicast (`5353`,
`224.0.0.251` / `ff02::fb`), the latter reusing the DNS wire format plus DNS-SD's PTR/SRV/TXT
convention and the probe-before-claim rules. **Check whether smoltcp gives us multicast group
membership** before estimating it.

**One structural detail from the measurement:** there is **one** `_adisk._tcp` instance for **three**
shares. The advertisement is per *server*, with the disks enumerated inside its TXT record
(`dk0=…`, `dk1=…`), not one announcement per share. Emitting three would be wrong.

Three service types are in scope: `_smb._tcp` (the server), `_adisk._tcp` (the Time Machine flags,
which is what populates the backup-disk list), and `_device-info._tcp` (the model string, where
`fruit:model = TimeCapsule` surfaces and which sets the icon macOS shows).

**Still to capture, and free:** `dns-sd -L GL-BE9300 _adisk._tcp local` prints the actual TXT keys and
flag values. Those bytes *are* the specification for what we must emit, and having the working ones
beats deriving them from the RFC.

#### The remaining scope risk is still worth measuring directly

**Chris's router serves Time Machine over SMB today (2026-07-30).** That is a working reference
implementation on his own network, so the requirement list below stops being something to guess at.
**The first task of this milestone needs no board and no code**: capture the SMB session between the
Mac and the router and read off the truth. The negotiated dialect, the capability bits, which create
contexts actually appear, what the mDNS records advertise, and which operations Time Machine really
issues. That converts this milestone's largest risk from unknown scope into a measured feature list,
and it is exactly the "measure, do not argue" rule applied to a requirement rather than a benchmark.

**Worth establishing what the router runs**, because it bounds the answer: if it is full Samba with
`vfs_fruit`, the reference is large; if it is **`ksmbd`** or another minimal server, then a much
smaller implementation is already known to satisfy Time Machine, and that is the target to match.

**What Time Machine over a network is believed to require** (from knowledge, *superseded by the
capture above* the moment it exists):

- **SMB3, not AFP.** Apple deprecated and removed AFP serving; SMB is the supported path.
- **Apple's SMB extensions**, the `AAPL` create context, which is what Samba implements as
  `vfs_fruit`. Without it macOS will mount the share but not accept it as a backup destination.
- **mDNS/Bonjour advertisement**, `_smb._tcp` plus `_adisk._tcp` carrying the Time Machine flags, or
  the share is not offered in the Time Machine UI. That is a second protocol (mDNS) on top of the
  first.
- **Durability semantics macOS trusts.** Time Machine writes a sparse bundle and depends on the server
  honouring flushes. This is the same clause §42 makes central, arriving as a compatibility
  requirement: a server that lies about durability produces backups that cannot be restored.

**Considered and rejected: porting Samba over the §31 C seam.** It is superficially the right move,
since we already confine a component we did not write (RedoxFS) and the seam exists for exactly this.
It does not survive contact: Samba assumes `fork`, threads, and an enormous POSIX surface, and
milestone 52 records that we have no `fork` and that getting one is not cheap. Worth stating, because
it is an honest limit of the C-seam story rather than a gap nobody noticed.

**The scoping decision to make first**, before any code: whether to implement the subset of SMB3 that
Time Machine needs, or a more general SMB3 server. The subset is much smaller and much less useful for
anything else; the general one is a project in its own right.

**Effort: not estimated, and deliberately so.** Anyone picking this up should re-scope it from scratch
against a verified requirement list rather than trusting this block.

### 56. Secrets, credentials, and the entropy to make them safe

**In brief.** Milestone 55 needs the Mac to authenticate, so it needs an identity, a secret, and
unguessable challenges. We had none of the three, and one of the gaps was a hard blocker rather than
a gap. **Prerequisite for 55; feeds milestone 49 (users, login, and attribution).** Chris's existing setup
serves **three** family members with separate passwords, so the credential service holds multiple
identities from the start rather than growing into that later.

**Status: both halves built** (entropy 2026-07-30, credentials 2026-07-31). All three gaps are
closed: unguessable bits come from a virtio-rng behind a capability, and an identity plus a secret
you can check and cannot read is a service with five kernel tests on both ISAs. **What remains is
the SMB-specific derivation** (the NT hash and HMAC-MD5, so the service can answer a challenge
without the adapter seeing the hash), and it is described at the end of this entry.

#### The entropy half: BUILT, 2026-07-30 (DECISIONS §44, notes/entropy.md)

The RNG used to be splitmix64 seeded from the virtual counter, predictable to anyone who could guess
boot-relative time, which blocked SMB authentication outright: an NTLMv2 server challenge that is
guessable is precomputable. That file has been **replaced, not patched**, exactly as its own last
paragraph said it should be.

What shipped: a **virtio-rng driver over both transports** (mmio and PCIe, §18's seam, one binary),
inside an **entropy service** that is the only thing in the system that can read the device. Clients
hold one endpoint that means *"you may obtain randomness"* and names no device, which is the fourth
appearance of attenuation by operation rather than by object. The service passes the device's bytes
through and computes nothing, because whitening without a one-way function is a reversible
permutation that obscures the claim rather than strengthening it. **The fork is settled**:
`std::random` improves transparently, split on std's own seam, so `SystemRng` (which promises
cryptographic strength) panics when the capability is absent while `HashMap`'s seed degrades to the
old stream and says so. Proven on aarch64 and riscv64, over both buses, plus a std program drawing
through the PAL.

What it does **not** promise: under QEMU the device is backed by the host's `/dev/urandom`, which is
a fact about the emulator. On real silicon the StarFive JH7110's TRNG is the candidate and **needs
verifying** before it is relied on, and there is no health test, so a device that started returning a
constant would be passed straight through. notes/entropy.md carries the full list.

#### The credential half: BUILT, 2026-07-31 (notes/credentials.md)

An identity, a secret, and a way to check the second against the first without ever being able to
read it. `crates/cred` (Argon2id, the store, constant-time verification), `crates/cred_proto` (the
wire contract), `user/src/credentialer.rs` (the service), `user/src/credentialer_test_client.rs` (its provisioner,
client, and attacker). Five kernel tests on both ISAs, 26 host tests, three Kani harnesses.

**The bearer-token problem below is answered, and the answer is sharper than "hand out the
operation".** Writing the store is not an operation at all, it is a **phase**: the service serves a
provision endpoint until `SEAL`, then deletes its receive end while the provisioner drops its send
end. Nothing in the system can name the object afterwards, so a client is not refused permission to
write the store; there is no object through which the request could travel. That shape was forced by
a real constraint (this kernel has one wait point, so a process serves one endpoint) and turned out
to be better than the guarded-opcode design it replaced.

**Argon2id, as a dependency, from RustCrypto** (§46's amendment: depend, do not vendor, because a
vendored copy is invisible to cargo-deny and crypto is what most needs to be visible to it). RFC
9106's and the reference implementation's known-answer vectors run against the version we link, which
is the whole point of depending. The exhaustive record-corruption test found a **debug-build overflow
panic inside argon2 0.5.3**: `Params::new` multiplies `p_cost * 8` before range-checking `p_cost`, so
`Cost::new` enforces the bounds before the value crosses the boundary.

Honest gaps, in full in notes/credentials.md: the cost parameters are **below OWASP's** (4 MiB rather
than 19, because the whole machine is 128 MiB of QEMU RAM); nothing survives a reboot; one verify
page means one client; there is no rate limit or lockout. And the one that matters for milestone 55,
below.

#### The thing we still do not have

~~**There is no crypto in the tree at all.**~~ There is now: RustCrypto's `argon2`, `blake2` and
`subtle`, via the credential half above, plus the precedent for how a crypto dependency enters (a
`deny.toml`-clean graph and the specification's own test vectors as tests). What remains is the SMB
side, and it is unchanged in substance: NTLMv2 needs MD4 (the NT hash) and HMAC-MD5; SMB3 signing
needs AES-CMAC; encryption needs AES-CCM or GCM; SMB 3.1.1 preauth integrity needs SHA-512.

**The credential service cannot serve NTLMv2 yet, and this is the next piece rather than a detail.**
NTLMv2's challenge-response requires the server to hold the **NT hash** and compute HMAC-MD5 over
it; an Argon2id tag cannot produce that, because the two are different functions of the same
password. So the store needs a second derivation and the service a second operation ("here is a
challenge, give me the response"), which is exactly the use-not-read shape already built and is not
code that exists. It also means shipping MD4 and MD5 on purpose. The credential primitive and the
SMB compatibility layer are separable, and only one of them requires choosing to ship a broken hash,
which is why the split fell here.

#### Identity lives at the boundary, and stops there

Milestone 49 records that identity is not authority here. SMB requires an identity, and the two
reconcile without compromise: **the adapter authenticates the client because the protocol demands it,
then uses the directory capability it already holds.** Identity never becomes ambient authority
inside the system, which is 49's login model exactly: authentication produces or permits the use of
capabilities rather than setting a field.

The consequence is worth stating plainly because it is the security claim: compromise the SMB adapter
and you get the share it holds. You do **not** get "authenticated as user X" with powers elsewhere,
because there is no elsewhere and no user X.

#### The hard part: a secret is a bearer token, a capability is an unforgeable reference

This is the genuinely new problem and it is a real tension in the model. Once a component can **read**
a password hash it holds it forever and can copy it anywhere; **knowledge cannot be revoked**. Every
other authority in this system can be.

**The answer is to hand out the operation, not the secret.** A credential service holds the NT hash
and computes the HMAC on request: the adapter sends a challenge and receives a response, and never
sees the hash. So the adapter holds a capability to **use** a credential, not to **read** one.

That is an improvement over the reference implementation rather than a reframing of it. In Samba,
`smbd` reads the password database directly, so compromising it leaks every hash: crackable offline,
reusable wherever the password was reused. Here a compromised adapter can use the credential while it
runs and cannot exfiltrate it, and revoking the capability ends the access.

**This is the third appearance of one pattern**, and it should be named as a principle rather than
rediscovered a fourth time: the NTP client that may *propose* a time but not *set* it (milestone 51),
the clock's read / set / propose ladder (§43), and now use-but-not-read. **Attenuation by operation,
not by object.**

**Built 2026-07-31, and the answer went one step further than this entry expected.** "Hand out the
operation, not the secret" is right for *reading*, and it is what the verify endpoint is. But
*writing* the store turned out not to need an attenuated operation at all: it is a **phase**, and
the phase ends. The provision endpoint is deleted at both ends at the seal, so there is no narrow
write operation to hand out and no wide one to withhold. The forcing constraint was that this kernel
has one wait point per process, which is the same wall the clock service hit (§43) and answered
differently; the answer here is better, because "the object no longer exists" is a stronger claim
than "the service checks". See notes/credentials.md.

#### Decisions to make before building

- **Take the crypto as a dependency, do not write it and do not vendor it** (§46, amended
  2026-07-31: vendoring is for what must be patched, and RustCrypto needs no patch; a vendored copy is
  also invisible to `cargo-deny`/`cargo-audit`, which is the one thing crypto most needs). Its crates are `no_std` and reviewed, and the
  supply-chain tooling from milestone 44 (`deny.toml`, `script/supply-chain`, `script/vendor-verify`)
  already exists for exactly this shape. Writing our own AES or SHA is a bad idea and the entry should
  say so rather than leaving it open. **Done for the KDF, 2026-07-31**: `argon2` 0.5.3 plus `blake2`,
  `subtle` and `zeroize`, `default-features = false`, nine crates, `deny.toml` clean unchanged. The
  discipline that came with it and should hold for the SMB primitives too: **the specification's own
  test vectors are tests**, because a dependency whose answers are never checked is one we have merely
  hoped about, and **the bounds get re-checked at our boundary**, because argon2's `Params::new`
  panics in a debug build on a large `p_cost` and a service a cost value can kill is a login outage.
- **We will be shipping known-broken primitives on purpose.** MD4 and MD5 are required by NTLMv2 for
  wire compatibility. Record that as a deliberate compatibility cost with its blast radius stated, not
  as an oversight, and keep them out of anything that is not SMB.
- **Secrets at rest are unsolved and should be scoped small.** Where does the hash live across
  reboots, and encrypted under what key? That is the same chicken-and-egg as milestone 51's NTS
  problem (certificates need time, time needs the network). The honest v1 is provisioned at boot and
  held only in memory; say so plainly rather than implying durability we do not have. **Still
  unsolved, and scoped exactly that small 2026-07-31**: the store is memory only and dies with the
  process. `cred::Record` has a versioned encoding with a round-trip test so the question has a
  starting point, and nothing writes one to a disk.
- ~~**Entropy is a capability**, and the service that holds it should be the only thing that can read
  the device. Whether `std::random` transparently improves or programs must ask for a real RNG is a
  design fork.~~ **Settled and built 2026-07-30**, DECISIONS §44: transparent, split on std's own
  `fill_bytes` / `hashmap_random_keys` seam, so the caller that promises cryptographic strength
  refuses rather than degrading. The service passes bytes through and does not pool or whiten.

**Sequencing.** Before milestone 55. **Both halves are done** as of 2026-07-31: entropy on 07-30, the
credential store and its service on 07-31. Each was worth doing on its own, and each was testable in
QEMU with no board.

**What is left of this milestone** is the SMB-facing derivation: the NT hash, HMAC-MD5, and a second
service operation that computes a challenge response without the adapter ever seeing the hash. That
is the use-not-read pattern already built, applied to a second secret, and it is the first place this
project chooses to ship a known-broken primitive. Secrets at rest remains unanswered and is not on
milestone 55's critical path, because provisioning at boot is enough to authenticate a Mac.

### 57. Partitioning and formatting a real drive, and extended attributes

**In brief.** Chris's router setup is `parted` then `mkfs.ext4` then three mounted partitions. We have
**no equivalent of the first step at all**, and the second only as a host tool. Plus the xattr gap
milestone 55 surfaced. **Nearly all of this is testable in QEMU against virtio-blk with no board**, so
it is schedulable before 2026-08-21 rather than waiting on hardware.

#### Extended attributes: decided in direction, open in mechanism

**Chris decided 2026-07-30: extended attributes, not AppleDouble sidecars**, on the grounds that we
will want them anyway. Agreed, and it does not reopen §34: that entry surveyed ext2, FAT32/exFAT,
littlefs, btrfs, ZFS and F2FS before choosing RedoxFS, and **xattrs were never the deciding axis**, so
the requirement adds a gap to fill rather than a comparison to redo. ext4 works on the router but
importing it means importing C, which §34 chose RedoxFS specifically to avoid, and there is no
`no_std` Rust ext4.

Verified: **RedoxFS has no xattr support.** **The fork is closed as of 2026-07-31: the layer**
(§34's amendment). Reversibility decides it: `fs_proto` hides which implementation was chosen, so
the format extension stays available later without any client changing. Attributes key on
`TreePtr<Node>`, so **rename is free and correct**, which sidecars get wrong.
Before designing the attribute layer, read `design/haiku-bfs-and-packages.md`: BFS made attributes
typed and indexed with live queries over them, and the point of knowing that is to avoid designing
something that **forecloses** indexing later, even though SMB only needs opaque blobs now.

**BUILT 2026-07-31: the layer, on both ISAs** (notes/xattr.md). Four verbs in `fs_proto`
(`GETXATTR`, `SETXATTR`, `LISTXATTR`, `REMOVEXATTR`) and a store the FS server keeps in a reserved
directory of the image, one blob per node, keyed on the `TreePtr` id. No new rung on the rights
ladder: reading an attribute takes what reading the file takes, changing one takes what writing
takes. Three limits with a reason each, and the third is load-bearing rather than arbitrary: sixteen
attributes of 255-byte names is **exactly one page**, which is why `LISTXATTR` needs no cursor and
therefore cannot be observed half-changed. Every ceiling refuses with its own errno (§42).

BFS is not foreclosed: every attribute carries a `u32` type code the layer stores, returns, and never
interprets, so an indexed store later is a change of implementation rather than a format migration
plus a wire break.

The three ways to get it wrong, all of which §34's amendment named in advance: the purge rides the
same transaction as the removal and asks the engine (`remove_node` answers `Some(id)` exactly when a
node's last link went), the store's name is unnameable and unlistable in every directory, and a
shrinking blob is truncated to length so the reader never walks records nobody wrote. The
rename-replacement case is the one removal the engine cannot report, and the server notices it.

**BUILT 2026-08-01: the recovery side, and two of the three named gaps closed.** `redoxfs_host
extract` puts the attributes back on the extracted files (`setxattr` on macOS, `lsetxattr` on Linux,
neither following a symlink), `ls` marks an entry that has them with `@`, and `xattr IMAGE PATH
[NAME]` renders or dumps them without extracting. The type code cannot come along, because no host
filesystem has a field for one, so each is named and counted and the raw store still comes out beside
the tree as its only home. Nothing about attributes can fail an extraction: a damaged blob, a name
Linux refuses for want of a `user.` prefix, a destination filesystem that holds none, each is
reported and walked past. The counts print even when zero, because "0 attributes reattached" is what
tells you the destination cannot hold them and a summary that hid the zero would read like a backup
that never had any (§42). The fixture is written by `fs_server::Server` itself, for the same reason
the tree fixture goes in through upstream's archiver.

The store directory now goes with the last attribute on the filesystem, which closes a limitation
recorded for a reason that was wrong: `remove_node` on a directory already refuses with `ENOTEMPTY`,
so the emptiness check is the engine's and costs no walk. It matters because `extract` copies the
store out, and a leftover empty `.cricker-attrs` would land in a recovered Documents folder. And
crash atomicity is measured rather than inherited: milestone 37's sweep now carries each name's
attributes in its state and four attribute operations in its workload, interleaved with a write to
the same file, so "the file and its metadata land together" is decided rather than argued.

**What was still not done here, and is now:** the caretakers (`fs_file_caretaker`,
`fs_subtree_caretaker`, `fs_nameset_caretaker`) answered `EOPNOTSUPP` to all four verbs rather than
forwarding, so a program behind a per-file grant could not reach its file's attributes. **Milestone
61 closed it**, and found the general defect underneath: nothing made a caretaker and the contract
agree, so a whole contract addition reached none of them and nothing failed.


- **Extend the on-disk format.** Correct, and atomic by construction since the metadata rides
  RedoxFS's own copy-on-write transaction. The cost is that §34 chose RedoxFS partly for being
  maintained upstream, pinned at 0.9.1 with a patch discipline that is currently two `Vec` imports; a
  format extension is a materially larger divergence that every future pin bump pays for. Upstreaming
  is the mitigation.
- **Layer xattrs in the FS server.** Normally dismissible, because on Linux anything can open the file
  directly and bypass the layer. **Here nothing can**: all access goes through `fs_proto`, so a layer
  above the filesystem is as authoritative as the filesystem. A genuine capability-system advantage.

**The check that decides it, and it is small: does RedoxFS let us group a file write and a metadata
write into one transaction?** If yes, layering is safe and much cheaper. If no, atomicity between a
file and its metadata cannot hold across a crash (§42's exact territory, and a rename must move both
together), and the format extension is the only correct answer. **Do this check before committing
either way.**

#### The tools, none of which exist

| Need | Status | Note |
|---|---|---|
| **GPT parsing** | **None** | Mandatory even if we never write one: you cannot find a partition on a real disk without reading the table |
| GPT writing | None | The `mkpart` equivalent. Protective MBR, header, entry array, two CRC32s, backup header at the last LBA |
| `mkfs` on the target | Host only | `redoxfs_host mkfs IMAGE SIZE_MIB` is a std host tool; the FS server is `no_std` |
| Block device enumeration | None | "What drives are attached", which is enumeration again and bounded by capabilities exactly as milestone 47's globbing and completion are |

#### Finding 2026-08-01: `mkfs` on the target is blocked on **entropy**, not on `std`

Investigated and measured, because "the FS server is `no_std` and the creation APIs are std-gated"
reads like a dead end and is not the real constraint.

`FileSystem::create` and `create_reserved` carry `#[cfg(feature = "std")]`, and so do the imports
they need and `Header::new`. Un-gating them is mechanical for all but **one** call:
`Header::new` stamps a fresh v4 UUID into the header with `uuid::Uuid::new_v4()`, which is
`getrandom`, which is the std path. The encryption branch wants randomness too (`Salt::new`,
`Key::new`), and that one does not matter here because this volume is deliberately unencrypted.

So the blocker is that **a filesystem needs a unique identifier and the engine has no source of
randomness in a `no_std` build.** cricker-os does: milestone 55's entropy service. The shape of the
fix is therefore small and upstreamable, and it is the shape upstream already uses one line away:
`create` takes `ctime` and `ctime_nsec` as *parameters* precisely because a `no_std` engine has no
clock. A `Header::new_with_uuid(size, uuid: [u8; 16])` does for randomness exactly what those
parameters do for time, and the caller (which has an entropy capability) supplies it.

**The same problem appears twice in this milestone, and has the same answer both times.**
notes/gpt.md already records that `crates/gpt` will not invent a partition GUID, for the identical
reason: "a GUID that is not random is not unique, this crate has no randomness, and inventing one
from a counter would be worse than refusing." Partitioning and formatting on the target are both
gated on plumbing the entropy service to the program that does them, and neither is gated on `std`.

This is a **decision for Chris**, because the fix is a divergence from the pin (`patches/README.md` records the
patch and how to submit it, which is the mitigation), and §46's rule is that taking one is a decision rather than a convenience. It is
also worth weighing against the pragmatic alternative: `redoxfs_host` on a Mac can partition and
format the drive today, which is what actually gets a disk ready for the board on 2026-08-21, and the
target-side version is then a capability demonstration rather than a prerequisite.

**GPT is a good crate to write.** Pure computation, well specified, so it is host-tested with tests in
milliseconds, and it has real Kani targets: CRC round-trip, primary and backup headers agreeing,
entry-array bounds, and refusing a table whose entries overlap.

**Built 2026-07-30: `crates/gpt`**, the parsing and writing halves both. Parse, validate (four CRC-32s,
the geometry, overlapping partitions, the protective MBR, the backup against the primary) and create,
with no I/O at all: the caller supplies blocks and receives blocks, so the whole thing is host-tested.
Seven Kani harnesses in `script/verify`. The claim that makes it credible is that it is tested against
**two real tables this project did not write**, from `sgdisk` and from macOS `diskutil`, committed as
fixtures; re-emitting `sgdisk`'s table reproduces its bytes exactly, and so does rebuilding it from
scratch. Two findings landed in notes/gpt.md: **macOS writes no GPT partition names at all**, so
nothing may identify a partition by its label, and the two tools disagree about the protective MBR's
CHS fields, which is why those are not validated. The cricker-os partition type GUID is DECISIONS §45.
What remains on this milestone is unchanged: the transaction check for xattrs, `mkfs` on the target,
block-device enumeration, and the host extraction tool.

#### The capability shape is the demonstration

Partitioning and `mkfs` are **destructive** and need authority over a *whole block device*. So the
tool holds one device capability and can destroy exactly that device and nothing else. Compare
`parted /dev/sda` as root, where a typo reaches any disk in the machine, and Chris's own instructions
carry a "confirm the target device path before proceeding" warning precisely because the tool cannot
enforce it. **Here the warning is structural**: the tool was handed one disk.

That also makes it a natural place for milestone 47's `enumerate` right to earn itself: listing
attached devices and holding one of them are different authorities.

#### Reading the drive from a MacBook or a Linux host: BUILT 2026-07-30

**The question that makes a backup credible rather than merely functional: the board is dead, can I
get my data?** Chris asked it, and the answer turns out to be that we disabled the feature.

**Correction to this section's original heading, which said "which upstream already solved".** It
half did. Upstream solved *mounting* (FUSE), and that is the path we deliberately do not take.
Nothing upstream ships extracts: `redoxfs-ar` is an archiver that only writes (and creates the
filesystem as it goes, so it cannot even be pointed at an existing image), `redoxfs-clone` copies an
image to another image, `redoxfs-mkfs` and `redoxfs-resize` are what their names say. The
extraction verbs did not exist and are now ours. See notes/host-recovery.md.

`vendor/redoxfs` already ships `src/mount/fuse.rs`, a `redoxfs` mount binary, and `redoxfs-ar`,
`redoxfs-clone`, `redoxfs-resize`. Upstream's default features are `["std", "log", "fuse"]`. Our host
tool depends on it with `default-features = false, features = ["std"]`, so **`fuse` is excluded by our
own choice** and re-enabling it is a feature flag plus the `fuser` dependency.

**What shipped**: `redoxfs_host ls IMAGE [PATH]`, `cat IMAGE PATH`, `extract IMAGE PATH DEST`, plus
`import IMAGE HOST_DIR` on the write side (upstream's own `redoxfs::archive`, which is what makes
the round-trip test read something our writer did not produce). Paths resolve from the image root and
`..` is refused, the same rule the FS server enforces on the wire. `fuse` is still off and `fuser` is
still not a dependency.

**Two things the build found that the plan did not predict.** First, the recovery reads must not
write to the image, and the engine makes that easy to get wrong twice: `FileSystem::open`'s
`cleanup` pass tidies allocations, and `Transaction::read_node` updates atime **only when the last
read was more than an hour ago**, which passes every test on a freshly made image and then dirties
the first real backup you touch. Read-only opens plus `read_node_inner` fix both, and the test hashes
the whole image across a read. Second, the operational rule can enforce itself: `Header::valid`
checks the format version first, so a mismatched reader sees no valid header anywhere and the engine
says ENOENT, which reads as "no such file or directory" about a disk you are holding. The tool now
reads the signature and version straight off the disk when an open fails and names the mismatch,
with a test that forges it.

Three paths, and they are not equally good:

| Path | Cost | Verdict |
|---|---|---|
| **Extend `tools/redoxfs_host` with `ls` / `cat` / `extract`** | Small; the engine already links there with `std` | **Do this first.** No FUSE, no kernel extension, no root, identical on macOS and Linux. The thing you want at 2am with a dead board. Check whether upstream's `redoxfs-ar` already covers it |
| **Linux mount via the `fuse` feature** | A feature flag | Nearly free, and upstream maintains it: it is how Redox developers work with images |
| **macOS mount via macFUSE** | A third-party system extension plus reduced security mode on Apple Silicon | Works, genuinely awkward. **Optional convenience, not the recovery story** |

**This removes the strongest argument for switching filesystems.** Interop was the one thing ext4
genuinely bought that RedoxFS appeared not to; it turns out RedoxFS buys it too, with a tool instead
of a kernel driver.

**The operational rule that follows: keep the recovery tool, or its exact source pin, with the
backup.** We are pinned at 0.9.1 (on-disk format version 8) and a reader must match the on-disk
format version. A backup readable only by software you no longer have is not a backup. Written up
with what the off-site copy has to carry in notes/host-recovery.md, which also draws the consequence
for future pin bumps: a bump to a different on-disk format strands every image already written, so it
is a migration, not an upgrade.

The same-engine objection is weaker than it looks and is recorded so nobody relitigates it: yes, the
reader shares any bug the writer has, but that is true of every filesystem (`e2fsprogs` shares lineage
with the kernel driver). The real risk is an *undocumented* format, and RedoxFS is open source with
upstream tooling.

#### Decided: no filesystem-level encryption on the backup volume

**Chris, 2026-07-30**: "If I'm struggling to get the data off, I'm not all that worried about somebody
else getting it." RedoxFS supports encryption (`src/key.rs`, and the read path calls `decrypt`), and
we are deliberately not using it here.

**It is the right call, and for a stronger reason than the one given.** Encryption belongs at the Time
Machine layer, and Chris's own setup instructions already offer it ("Optionally enable Encrypted
Backups"). The Mac encrypts before anything is sent, so **the server never holds plaintext**, recovery
uses the client's key rather than the server's, and filesystem encryption underneath would be
redundant while putting a key on the machine most likely to be compromised. It also strengthens
milestone 55's claim: a compromised SMB adapter leaks ciphertext.

Two consequences. The recovery tool needs **no key handling at all**, which is a real simplification.
And if Time Machine encryption *is* enabled, recovery then depends on that password, which relocates
the "can I get my data" risk rather than removing it, so the password belongs wherever the family's
other credentials live rather than only in one Keychain.

**Sequencing.** The GPT crate and the transaction check are independent of everything and can start
now. The host extraction tool was likewise independent and was the cheapest credibility win on this
milestone; **it is done** (2026-07-30), which is why the milestone's row now reads PARTIAL. `mkfs` on
the target wants the block-device path settled. Real drives arrive with milestone 53. **Effort: not estimated**, though the GPT crate alone looks like one lane on the history-calibrated
scale.


## The display ladder (recorded 2026-07-28, Chris's direction)

The stated destination: eventually, something like COSMIC driving a GPU for display. That
decomposes into rungs, each independently a demo, and the decomposition is what makes the ambition
honest. COSMIC's shape is Rust clients rendering into shared buffers, a compositor compositing
them to scanout, everything message-passing; cricker-os already has shared frames and endpoints,
so the *architecture* is aligned even where the drivers are mountains.

**Status (2026-07-30): rungs one and two are built, and rung one's deferred VT engine now is too.**
Rung one shipped its contract and its pixels, rung two shipped whole, and milestone 29's remaining
increment closed the gap with a bitmap font, a sans-IO VT engine, a display terminal that is a client
at both seams, and a confined virtio keyboard (DECISIONS §37, notes/glyphs.md). All on both ISAs, all
with the pictures verified from the host as well as the guest: `cargo xtask` now proves **three**
pictures over one boot, in order, and the text check's negative control is a screen with one letter
changed. Rung three is the next step and is where the parked competitor question below has to be
answered on purpose.

1. **Rung one: milestone 29** (promoted from optional). **Built**: a confined userspace virtio-gpu
   driver (`display`), a client that draws (`painter`), and the framebuffer contract between them
   (`crates/gfx_proto`, notes/framebuffer-contract.md, DECISIONS §29). The framebuffer is a bigger
   grant and never an exemption; the pixels are proved in the guest by two witnesses in two address
   spaces and from the host by comparing QEMU's `screendump` against the pattern definition.

   **Its deferred half is built too** (2026-07-30, DECISIONS §37, notes/glyphs.md): a public-domain
   8x8 bitmap font, a sans-IO VT engine, a display terminal, and a virtio keyboard. The deferral's
   premise held exactly as written: the contract carries pixels, not text, so the terminal arrived as
   another client and **neither `gfx_proto` nor `display` changed a line**, which the same binary then
   demonstrated a second time by being a compositor client with `window`'s authority. The VT engine's
   language is still an open choice, and notes/glyphs.md now prices libghostty-vt against a built
   Rust engine rather than against an estimate.
2. **Rung two: a compositor component (milestone 33). Built**, both ISAs: `compositor` multiplexing one
   screen among three mutually distrusting clients, each holding a capability to its own surface;
   software composition honouring a damage rectangle; input routed by capability using the terminal
   contract's `OP_BYTES` driver half, so a terminal drops in unchanged. No ambient display: window
   enumeration and screenshots are **read-only mappings**, not verbs, so a client that holds neither
   has nothing to call and nowhere to look. See notes/compositor.md and DECISIONS §33. The design's
   load-bearing idea, which was not the obvious one: the shared doorbell endpoint carries **no
   authority at all** (a shared endpoint has no sender identity, so anything named in a message would
   be forgeable), every per-client fact lives in per-client memory, and the compositor therefore
   contains no authorization code. Wayland's model is the prior art and this is the difference in kind
   from it: Wayland attaches client identity at the transport and decides in code, so its security is a
   property of that code.

   **The rung also found the one primitive this kernel lacks**, and it is recorded as a fork rather
   than built: there is no wait-any, and two threads cannot share an address space, so a process has
   exactly one blocking wait point. A component that must distinguish more than one *class* of sender
   must therefore be more than one process, or carry authority somewhere other than its messages. The
   compositor took the second road, and it turned out stronger; but with the primitive, per-client
   endpoints would give unforgeable identity for free (letting a bad damage rectangle be refused to its
   author rather than clipped), a screenshot could be a consistent served snapshot, and input delivery
   would stop being a blocking `CALL` into a client. DECISIONS §33 has the two candidate forms and
   their costs. **Architect's call.**
3. **Rung three: real applications.** iced's software-rendering path and cosmic-text on the
   milestone 27 std PAL. Something COSMIC-like appears here, before any GPU.
4. **Rung four: GPU acceleration via virtio-gpu 3D (milestone 34).** The Venus path (Vulkan over
   the virtio device, over the §18 PCIe transport): how every VM gets a GPU without a hardware
   driver, and what would give wgpu something real. A mountain, but a climbable one, priced as
   such.
5. **Rung five: struck.** A bare-metal driver for the VisionFive 2's BXE-4-32 3D core is a
   Linux-scale multi-year effort (loaded firmware, thin documentation, Mesa still maturing on
   Linux itself) that proves nothing rung four does not. The board's standalone-display story is
   the DC8200 framebuffer path instead: U-Boot's `simple-framebuffer` handoff first (zero display
   code), a mode-setting driver only if ever needed, serial input until a USB HID milestone earns
   its own number. The JH7110 has no IOMMU, so display DMA on that board is confined by software
   discipline, and the record will say so.

Governance, stated now so it is not smuggled later: rungs one and two are demonstrator work.
Rungs three and four reopen the parked competitor question below, which is the architect's call
to make consciously when rung two is real. **Rung two is now real, so that call is live**, and
milestone 33 deliberately stopped at its edge: no iced, no cosmic-text, no application work.

## The rival worth understanding, not building

eBPF is the strongest competing answer to the question this whole architecture asks: safe kernel
extension through *verification* rather than *isolation*, with no IPC cost. Worth reading as the
other fork. It does not undercut the thesis so much as relocate the cost: the eBPF verifier is itself
a large, subtle, repeatedly-CVE'd component, so "the verifier is the TCB" is its version of the
problem, not an escape from it. No milestone; a reading item.

### 68. Code-quality gates: one lint policy, and the lints that lost

**Status: PARTIAL.** Started and largely landed 2026-08-02, from an audit of what the tree checked
and what it did not. Two halves are deliberately unfinished and scoped below rather than rushed.

#### What landed

The tree had no `rustfmt.toml`, so import order was whatever each author typed, and lint selection
lived in 19 of 39 crates repeating one `[lints.rust]` table while the other 20 said nothing. Both are
now single decisions: `group_imports`/`imports_granularity` in `rustfmt.toml`, and
`[workspace.lints]` with a one-line opt-in per member.

Adopted: `cast_ptr_alignment`, `ptr_as_ptr`, `semicolon_if_nothing_returned`, `manual_let_else`,
`doc_markdown`. 1,221 warnings went to zero. Three new non-clippy gates joined `script/lint`:
**dependency direction** (nothing under `crates/` may depend on a binary, which would take it out of
the host tests and Kani while still building), **unused dependencies** (§46 with a gate), and
**spelling** over the prose.

#### The part worth carrying off: three lints were removed on the evidence

Each was enabled, measured against the real tree, and dropped, with the number recorded next to it
in `Cargo.toml` and `rustfmt.toml` rather than silently omitted.

- **`cast_possible_truncation`**: 199 of 497 hits are `u64`/`i64` to `usize`, warned about for
  32-bit-pointer targets. §19 names aarch64, riscv64 and x86_64, all 64-bit. Over half its output is
  about a platform that does not exist here, and clippy cannot be told otherwise.
- **`items_after_statements`**: all 43 hits are a `const` sitting beside its use, under the comment
  that explains it. Obeying it separates every one from its explanation.
- **`format_code_in_doc_comments`** (rustfmt): destroyed an authored alignment column inside
  `crates/gpt`'s module example, and emitted trailing whitespace into a doc comment.

`doc_markdown` is the same story with the opposite ending: 416 hits, about half wanting backticks
around `RedoxFS`, `PCIe` and `OpenSBI`, which are proper nouns that would then render as code a
reader could type. `clippy.toml`'s `doc-valid-idents` takes those; the other half were real.

The general rule, and the reason this milestone is worth a roadmap entry at all: **a lint that is
right in general can be wrong for a tree, and the way to find out is to run it and read the hits.**
Reasoning about a lint's description predicts none of these.

#### What is NOT done, with counts

Both remaining halves are real engineering, not mechanical, and a first attempt at automating one of
them was reverted for producing exactly the wrong artefact.

- **Doc examples.** 5 doctests in the whole host workspace became 23, and nine crates went from
  0.0% example coverage to somewhere between 2.4% and 25%. That is a real start and explicitly not
  the FreeBSD standard CLAUDE.md sets: **28 host crates still have no example at all.** The crates
  done first were the ones where an example carries an argument rather than a signature (`capability`
  showing that intersection is the only transfer operation, `measured_boot` showing that an
  unmeasured name fails CLOSED, `regions` showing the two refusals that are not about the budget).
  The ones left are mostly parsers that need a real fixture to demonstrate (`elf`, `dtb`,
  `crickerfs`, `gpt`), which is more work per example, not less valuable.
- **`missing_docs`** is still not adoptable, and the number says why: item coverage runs from
  **36.4%** (`socket_proto`) to 100%, with `pci` at 48.9% and `intrusive` at 50%. Adding it to
  `[workspace.lints]` is a commitment to write several hundred item docs first, which is §61's rule
  and not a formality.
- **Doc examples: 5 doctests in the entire host workspace**, and `rustdoc --show-coverage` reports
  0.0% examples on every crate sampled (`ipc` 94.4% of items documented, 0.0% examples; `capability`
  67.6%/0.0%). CLAUDE.md sets the FreeBSD standard explicitly ("a page without a worked example has
  not finished explaining itself"), so this is a stated commitment the tree does not meet. A doctest
  is documentation and a test at once, and `cargo test` already runs them, so the harness needs no
  work; only the examples are missing.

`missing_docs` belongs with the second of those (item coverage is 67–94% and no crate warns on it),
and is best done in the same pass as the examples rather than separately.

#### What closing the unsafe half taught

All 205 blocks are commented and `undocumented_unsafe_blocks` is in `[workspace.lints]`, so the
convention is now enforced rather than followed. The useful finding is what the sites turned out to
be, because it is not what the raw count suggested.

**Three quarters of them were genuinely uniform**, and the uniformity was a fact about the system
rather than an excuse:

- **58 panic-handler traps**, byte-identical `asm!("brk #0", options(nostack, nomem))` or its
  `ebreak` twin, in EL0 programs.
- **73 `invoke` syscalls.** `user_rt::invoke` is the only unsafe function in the EL0 runtime, and its
  contract is that there is no caller obligation: *"the kernel validates the capability and the
  method before acting; the caller is trusting the kernel, not the other way around."* An
  `unsafe { invoke(..) }` is unsafe because it is inline asm, not because a bad slot could break
  anything. **That is the capability model showing through the type system**, and it is why one
  sentence is honest at all 73 sites.

**The remaining quarter was the real work**, and each site's comment says something a reader could
not have guessed: intrusive-queue link ownership (including a drop-order fact, that a test's nodes
are declared before the queue so they outlive it); allocator alignment invariants; virtio ring
aliasing, where the read side is the driver's memory and the write side is a kernel-private shadow;
`env::set_var` in `xtask`, unsafe since edition 2024, sound only because the one thread it ever
spawns copies pipe bytes and never reads the environment.

**The test that decides whether a batch may share a comment** is whether the sentence is checkable
at each site. For a trap in a panic handler it is, because it is literally the same site 58 times.
For a test module's pointers it is not, which is what the reverted generic pass got wrong.

One regression is worth recording because nothing else would have caught it: adding an
`#[allow(clippy::cast_ptr_alignment)]` above an `unsafe` block silently **separated an existing
`SAFETY:` comment from its block**, and clippy then reported the block as undocumented. An attribute
between a comment and the item it describes breaks the association. The fix is ordering: attribute
first, then the comment, then the block.

### 69. Split `kernel/src/user.rs` by service

**Status: BUILT (2026-08-02), both ISAs.** Raised 2026-08-02, from a question about whether
thousand-line files are an antipattern in Rust. The general answer is no, and this file is the
exception that proves why.

All 46 top-level modules moved to `kernel/src/user/<name>.rs`; `user.rs` went from **15,499 lines to
1,993**, and the largest file left in the tree from that split is `user/tests.rs` at 2,306. Nothing
gained visibility: not one `pub`, `pub(crate)` or `use` was added or widened, which the section below
predicted and which was then checked mechanically rather than by eye. Re-inlining every new file back
into `user.rs` and running `rustfmt` over the result reproduces the pre-split file **byte for byte**,
so the only content change in the whole milestone is `rustfmt` reflowing about 90 lines that gained
four columns of width when they lost a level of indentation.

The declaration each module left behind keeps its own doc comment and its own `#[cfg]`/`#[cfg_attr]`
attributes, so `user.rs` reads as an annotated index of the services and `rustdoc` is unchanged.

#### Why file length is usually the wrong metric here

In Java the one-public-class-per-file rule is compiler-enforced; in Rails, autoloading maps paths to
constants. Both make size a proxy for "too many responsibilities". Rust has neither: a file **is** a
module, the module is the privacy boundary, and the standard library ships multi-thousand-line files
that are one coherent thing. This tree also comments far more heavily than production code by policy
and keeps unit tests in-file, which inflates counts for good reasons. `crates/glob` is 1,173 lines of
which **54% are tests**; `crates/calendar` is 1,578 at 43%. Shrinking either would make it worse.

#### What makes `user.rs` different

It is **15,499 lines holding 46 top-level modules**: roughly a dozen `pub mod *_service` blocks
(`console`, `virtio`, `fs`, `display`, `compositor`, `keyboard`, `clock`, `entropy`, `credential`,
`ntp`, `untyped`, `pipeline`) and roughly 34 `#[cfg(test)]` modules, interleaved. `fs_service` alone
is 1,217 lines and the top-level `tests` module is 2,320.

The test that matters is not the line count but this: **to change the NTP service you open the file
where the compositor lives.** That is ten responsibilities in one place, and no amount of Rust
module semantics makes it one.

#### Why this split is unusually cheap

The standard argument against splitting a Rust file is that it forces you to widen visibility: items
private to the module become `pub(crate)` merely to be reachable, and a long file is traded for a
leakier API.

**That argument does not apply, because the boundaries already exist as `mod` blocks.** A child
module can see its ancestors' private items whether it is written inline or in its own file, so

```rust
pub mod fs_service;          // was: pub mod fs_service { ... }
```

is semantically identical to what is there now: no visibility change, no API change, and `use
super::*` inside keeps working. This is a file move, not a refactor, which is what separates it from
the speculative restructuring CLAUDE.md warns against.

#### The one real cost, and the scheduling consequence

`user.rs` is the kernel's service-wiring file and nearly every milestone touches it, so the diff
conflicts with anything in flight. **Do it while the tree is quiet**, in one pass, and do not
interleave it with feature work. Splitting it across two lanes would be worse than not splitting it.

Suggested shape: `kernel/src/user/` with one file per service and each service's tests beside it,
leaving `user.rs` as the wiring that names them.

### 70. `swish`'s remaining logic in a crate, host-testable like its siblings

**Status: BUILT.** Raised 2026-08-02, and **the finding that prompted it was wrong**, which is
worth recording because the corrected version is a smaller and more honest milestone.

`crates/swish` holds the shell's logic and `user/src/swish.rs` keeps the IO, which took 354 lines out
of the program and bought 36 host tests (33 unit, 3 doctests) where there had been none. What lifted:
the routing of a typed line, the pattern-versus-text question, the expansion order, `echo`, and every
sentence the prompt prints (the refusals, the outcome, the endowment preview, the shell's own `caps`
table, the help). What did not, and why, is in notes/shell.md and in the crate's own `BUGS` section:
`builtin`, `dispatch_one`, `run`, `spawn` and `pipeline` are capability movement, and lifting them
would have needed the shell's IO restructured, which this milestone was scoped not to do.

#### The correction

`user/src/swish.rs` is 2,625 lines with **zero `#[cfg(test)]` blocks**, and that was first reported
as "the shell is untested". It is not. The shell is covered twice over:

- **~28 QEMU integration `test_case`s** across five kernel test modules (`shell_navigation_tests`,
  `pipeline_tests`, `redirection_tests`, `glob_grant_tests`, `rm_program_tests`), which spawn the
  real binary and drive it.
- **93 host unit tests in `crates/grant_plan`**, which already holds swish's parsing, navigation and
  grant-planning logic. `swish.rs` imports `grant_plan::{expand, line, nav}` and `line_editor::proto`
  rather than reimplementing any of it.

So 0% was a fact about one **file**, not about a component, and a file-level metric said something
false about the system. That is the general lesson: coverage measured per file counts where tests are
*written*, not what they *reach*.

#### The real gap, which is narrower

What is left in `swish.rs` is mostly IO glue, and some of it is logic that a host test could reach if
it were lifted: `builtin` and `dispatch`, `outcome`'s interpretation of a spawn result, `preview`'s
rendering of an endowment, `refuse`'s mapping from a `Refusal` to what the user reads, `print_num`.
Today every one of those is exercised only by booting QEMU, which is slow, coarse, and cannot easily
provoke the error paths.

CLAUDE.md already names the pattern this should follow: **a crate and a program may share a name, and
it says something when they do** (the crate is the logic, lifted so it is host-testable and
Kani-reachable; the program keeps the IO). `coremark`, `line_editor` and `compositor` are each that
pair. `swish` is the largest program that is not one.

#### Scope note

This is an incremental tidy of a working, tested component, not a fix for a defect. It should be
scheduled accordingly, and it should not grow into a rewrite of the shell. If lifting a function
needs the shell's IO restructured to accommodate it, that function stays where it is.

#### A gate blind spot found while raising this

Milestone 69's **table row said `NOT-STARTED` while its own detail block said `BUILT`** on `main`,
because the lane that built it updated one and not the other. `script/roadmap --check` did not catch
it: the check validates the status VOCABULARY and that every detail block has a table row, but never
that the two statuses AGREE. Corrected by hand on 2026-08-03.

This is the third such blind spot found in two days, and they are all the same shape: the gate
verifies that a thing is well-formed and never that it is right. `script/decisions --check` reports
"numbering clean" for a section filed in the wrong place, and nothing checks that a source path cited
in prose resolves to a file that exists (milestone 69 fixed 49 stale ones by hand). Each is a few
lines to add. None is written yet, and they are listed here rather than in a tracker so the next
person to touch these scripts finds them.

### 71. The thread-start fault: a user thread dispatched with `sepc` = 0

**Status: BUILT (2026-08-03), both ISAs.** Found, proved on the machine, and fixed. It was **frame placement**,
which is where this entry said to look first, and the mechanism is exact rather than plausible:
`current_sp()` is a real call at opt-level 0, so it returned `sp - 16` and put the frame at
`sp - 304` while `trap.s` builds an S-mode trap frame at `sp - 288`. Sixteen bytes apart, so the user
frame's `x[2]` sat on the trap frame's `x[0]` slot, which `trap_entry` writes as a literal zero. That
is why `user sp` read `0x0000000000000000` and not garbage. The frame now goes at
`top - size_of::<TrapFrame>()` on both ISAs, with the shallow TCB path handled by a reservation in
`user_entry_trampoline` rather than a moving target. See notes/riscv-port.md.

**The scope note below is upheld and one sentence of it is now wrong.** The fault does have a silent
face: the `sepc == 0` guard fires only when `t5` happened to be 0, and otherwise the thread `sret`s
to a garbage PC and dies quietly, so a lost-wakeup hang with no guard message really can be this bug.
But the specific hang on `reclaim_frees_a_started_then_exited_childs_regions` is **not**: that test's
child takes the TCB path, which runs with interrupts masked and cannot take the clobber, and the hang
reproduces with this fix in the tree (one run in four under host load, a recipe that did not exist
before). It is tracked as its own open item in notes/scheduler.md.

#### The evidence, which we now have because we instrumented instead of chasing

The fault was first seen on 2026-08-02 as a *hang*: a user thread whose first instruction fetch
faulted, so whatever it was meant to serve never answered, every waiter blocked, and the run died 60
seconds later in the lost-wakeup watchdog, arbitrarily far from the cause. It never reproduced
locally, in nine full runs across both ISAs. Rather than keep hunting it, `enter_user` got a guard
that converts the rare hang into a loud failure carrying its own evidence.

That guard fired on CI, on the milestone-70 branch:

```text
[PANIC] panicked at kernel/src/arch/riscv64/exceptions.rs:155:5:
thread 25769803877 on core 1 was dispatched to U-mode with sepc = 0 (user sp 0x0000000000000000).
Its context was never built, or was built and not seen by this core.
```

Three details do the work:

- **`user sp` is zero too.** This is not a bad entry point, it is an entire trap frame reading as
  zeros. Whatever `enter_user` is looking at, nobody wrote it.
- **Core 1**, a secondary rather than the boot core.
- The test was `a_user_program_reaches_el0_and_returns_twice`, which is one of the simplest
  user-entry paths in the suite.

#### Where to look first, and why it is probably not the obvious thing

The obvious reading of "built but not seen by this core" is a missing release/acquire pair between
the core that builds a context and the core that dispatches it (DECISIONS rule 4: assume weak
ordering). **That is probably not it**, and the reason is worth knowing before anyone spends a day
on barriers: the frame is built by the thread ON ITS OWN kernel stack, in `kernel/src/user.rs`
around the `current_kernel_stack_top()` call, and `enter_user` runs a few lines later on the same
core with no yield in between.

The likelier suspect is **where the frame is placed on riscv64**, which is already known to be
delicate and is already commented as such:

```rust
#[cfg(target_arch = "aarch64")]
let slot = top - size_of::<TrapFrame>() as u64;
#[cfg(target_arch = "riscv64")]
let slot = (crate::arch::current_sp().min(top) - size_of::<TrapFrame>() as u64) & !15;
```

aarch64 uses a **fixed** offset from the stack top. riscv64 computes the slot from the **live
`sp`**, because its TCB entry path is shallow enough that a frame at the top would overlap and
corrupt this function's own stack. The existing comment says exactly what that failure looks like:
"sending the sret to a garbage sepc". `sscratch` is then armed to `frame + size` so re-entries
rebuild at the same address.

So the question to answer first is **whether the address `frame.write` targets is always the address
`enter_user` and the trap path later read**, on every path that reaches user entry and at every
stack depth. A slot that moves with call depth is a slot that can be written in one place and read
in another, and an all-zero frame is what reading the wrong place looks like.

Every failure so far has been on riscv64. That is consistent with the placement hypothesis and
inconsistent with a generic ordering bug, which would be expected to show on aarch64 too.

#### What is already in place

- The `sepc == 0` guard in `kernel/src/arch/riscv64/exceptions.rs` and its `elr == 0` twin in the
  aarch64 file. Keep both; they are how this became findable.
- The frame's writability assertion, one check per exec.
- `script/cpu-matrix` uploads per-model logs as a CI artifact on failure, which is how the evidence
  above was recovered.

#### Scope note

Do not "fix" this by widening a deadline or re-running. Three CI failures on 2026-08-03 traced to
three different tests on three different CPU models, and only one of them announced itself as this
fault; the other two were a frame-leak wait and the lost-wakeup watchdog, which are what this bug
looks like when the guard does not happen to catch it first.

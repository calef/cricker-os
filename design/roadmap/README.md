# Post-v1 milestone roadmap

The eleven milestones in DECISIONS.md were the plan, and they are done; rows 1 to 11 record them,
backfilled 2026-08-03 from the first commits' history (milestone 76). The rest is the roadmap past
them. It began (see the git history of `design/roadmap.md`, this directory's predecessor) as an uncommitted `design/` proposal drawn from the
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

**The `Built` column is the date a milestone turned `BUILT`**, and it is empty for every other status.
It sits last on purpose: `script/roadmap`'s row parser anchors on the first three columns, so a column
appended at the end cannot break it, where one inserted in the middle would. A milestone that landed in
phases carries the date of the **last** phase, because that is when it became `BUILT` rather than
`PARTIAL`; milestone 27's row says 2026-07-29 (phase two) though its block opens with 2026-07-28, and
milestone 42's says 2026-08-02, when the third of three legs landed.

Where a date came from, because the sources are not equally good. Thirty-one rows take it from the
milestone's own file, which states it ("Built 2026-07-30, both ISAs"); milestones 1 to 11 are in that
count, taking it from the commits their backfill files cite, which is the same evidence one step
removed. The other thirteen state no landing date anywhere in their file and were derived from the git
history of `design/roadmap.md`, this directory's predecessor: the author date of the commit where the
row's status flipped. Nothing is guessed, and no row reads "unknown".

**Two of those thirteen are the weak ones, and they are 20 and 30.** The status column did not exist
until `0c97bd0` on 2026-07-30 introduced it; before that a row said "built" in prose, phrased a dozen
ways. Those two rows never said it in prose at all, so the earliest date the history can prove is the
day of the sweep, and the work almost certainly landed earlier. Their dates are an upper bound, not a
measurement. If either milestone's file ever gains a landing date, that date wins.

**Effort, calibrated from git history (2026-07-30), not guessed.** The milestone files give effort in
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

| #  | Status | Milestone | Why it matters (§14) | Built |
|----|--------|-----------|----------------------|------------|
| 1 | BUILT | [Boot to Rust on QEMU `virt`, and print to the PL011 UART](01-first-boot.md) | the first instruction; freestanding Rust, the linker script, and a test harness on day one | 2026-07-12 |
| 2 | BUILT | [Exception vectors, and a fault that tells you what it was](02-exception-vectors.md) | faults, interrupts, and syscalls are one mechanism; this is the plumbing for all three | 2026-07-13 |
| 3 | BUILT | [Hand out physical memory, and detect a smashed stack](03-frame-allocator.md) | where RAM actually comes from, and the allocator allocates itself first | 2026-07-13 |
| 4 | BUILT | [MMU on: page tables, the kernel heap, the high half](04-mmu-and-heap.md) | virtual memory with W^X and a guard page; the heap milestone 14 later removed on purpose | 2026-07-13 |
| 5 | BUILT | [The GIC and the timer: the kernel is preemptible](05-gic-and-timer.md) | the preemption source, and the locking discipline becomes load-bearing | 2026-07-13 |
| 6 | BUILT | [Threads, the context switch, and preemption](06-threads-and-preemption.md) | a hostile loop that never yields is preempted anyway, DECISIONS §5 made executable | 2026-07-14 |
| 7 | BUILT | [User mode: EL0, capabilities, the ELF loader, and IPC](07-user-mode.md) | the actual OS boundary, and the §10 decision made deliberately at the parked decision point | 2026-07-14 |
| 8 | BUILT | [The console driver leaves the kernel](08-console-leaves-the-kernel.md) | the microkernel thesis, executable: no user-reachable path touches kernel UART code | 2026-07-14 |
| 9 | BUILT | [A virtio-blk driver at EL0, and an interrupt becomes a message](09-virtio-blk-at-el0.md) | userspace drivers: MMIO by capability, IRQ as message, the kernel touches no DMA | 2026-07-14 |
| 10 | BUILT | [A shell at EL0, and processes spawned on command](10-shell-and-spawn.md) | proof the whole stack works: every keystroke is a conversation between processes | 2026-07-14 |
| 11 | BUILT | [Untyped memory, and the number that proves the kernel stops allocating](11-untyped-memory.md) | a process cannot make the kernel allocate, so it cannot exhaust it | 2026-07-15 |
| 12 | BUILT | [Call/Reply IPC: a one-shot reply capability](12-call-reply-ipc.md) | the IPC the TCB must get right | 2026-07-22 |
| 13 | BUILT | [Capability revocation + untyped reclamation](13-capability-revocation.md) | safe teardown, a TCB property | 2026-07-22 |
| 14 | BUILT | [Kernel objects from untyped: remove the kernel heap](14-kernel-objects-from-untyped.md) | removes the kernel heap: the prerequisite for "small enough to verify" | 2026-07-23 |
| 15 | BUILT | [Tagged address spaces (ASIDs)](15-asids.md) | a context switch stops flushing every translation | 2026-07-23 |
| 16 | PARTIAL | [Real hardware + IOMMU-backed driver isolation, **RISC-V first**](16-real-hardware-iommu.md) | isolation in hardware, under real workloads | |
| 17 | OPTIONAL | [Multikernel-leaning scheduler (research, optional)](17-multikernel-scheduler.md) | optional; not on the thesis path | |
| 18 | BUILT | [Verify the capability core, then spread inward](18-verify-capability-core.md) | the verification itself | 2026-07-23 |
| 19 | BUILT | [Run a real workload](19-real-workload.md) | the "runs real workloads" half of the thesis | 2026-07-25 |
| 20 | BUILT | [A portable HAL, proven on a second architecture](20-portable-hal.md) | the "portable verified core" claim | 2026-07-30 |
| 21 | BUILT | [Performance measurement: benchmarks with teeth](21-benchmarks.md) | perf claims become measurements, and regressions surface next to their cause | 2026-07-23 |
| 22 | BUILT | [Trusted init: verify it, and shrink what a broken one can do](22-trusted-init.md) | closes the thesis's own soft spot: init is the privileged *unverified* component | 2026-08-04 |
| 23 | PARTIAL | [A capability-routed component OS with live replacement](23-component-os-live-replacement.md) | the flagship payoff, and a product ambition | |
| 24 | OPTIONAL | [A second aarch64 *board*: Virtualization.framework (optional)](24-second-aarch64-board.md) | proves the `arch/` **board** boundary on a second machine of the same ISA; optional | |
| 25 | PARTIAL | [Cross-OS performance comparison (extends 21)](25-cross-os-comparison.md) | turns perf claims into cross-OS numbers | |
| 26 | BUILT | [Object revocation: tear a process back down](26-object-revocation.md) | the teardown half of "run real workloads": a process can be reaped, not just built | 2026-07-26 |
| 27 | BUILT | [Rust `std` on the native ABI](27-rust-std.md) | widens "runs real workloads" by orders of magnitude | 2026-07-29 |
| 28 | BUILT | [A solid terminal: the line discipline as a component](28-line-discipline.md) | a terminal with real behaviour, which 27's stdio semantics need | 2026-07-28 |
| 29 | BUILT | [A display terminal (framebuffer, virtio-gpu)](29-display-terminal.md) | the first pixels the demonstrator ever puts on a screen, and then the first letters | 2026-07-30 |
| 30 | BUILT | [The network stack as a confined component](30-network-stack.md) | the canonical microkernel component, and the one people ask about first | 2026-07-30 |
| 31 | PARTIAL | [A capability shell: designation is authorization](31-capability-shell.md) | no-ambient-authority made user-visible, at the one interface a human touches | |
| 32 | BUILT | [A real filesystem: RedoxFS behind a capability FS server](32-redoxfs-fs-server.md) | the flagship userspace-reuse story: a real filesystem we did not write, confined | 2026-07-29 |
| 33 | BUILT | [A compositor: one screen, mutually distrusting clients](33-compositor.md) | the canonical multiplexer of one device among mutually distrusting clients | 2026-07-29 |
| 34 | NOT-STARTED | [GPU acceleration via virtio-gpu 3D (the display ladder's rung four)](34-gpu-acceleration.md) | how every VM gets a GPU without a hardware driver | |
| 35 | BUILT | [Prove the DMA-confinement boundary (extends 18)](35-dma-confinement-proof.md) | closes the one isolation boundary we test instead of prove | 2026-07-29 |
| 36 | BUILT | [A foreign-language component, seam first (spike; feeds 29 and 23)](36-foreign-component.md) | the thesis in one assertion: unverified foreign code, confined and restarted | 2026-07-29 |
| 37 | BUILT | [Prove RedoxFS's crash consistency (DECISIONS §34, condition 1)](37-redoxfs-crash-consistency.md) | decides whether §34's "primary filesystem" label is earned | 2026-07-30 |
| 38 | NOT-STARTED | [Filesystem throughput, and the comparison (DECISIONS §34, condition 2; extends 21/25)](38-filesystem-throughput.md) | "primary filesystem" invites a comparison we cannot currently make | |
| 39 | RECORDED | [Repository structure for a loosely-coupled OS, and the road to a distribution](39-repository-structure.md) | the structure has to serve the thesis, and one constraint dominates | |
| 40 | NOT-STARTED | [Documentation as a system service: searchable, rendered, and installed by packages](40-documentation-service.md) | the OS explains itself, on itself | |
| 41 | BUILT | [Dead code: triage the suppressions, and un-blindfold the gate](41-dead-code-triage.md) | a `-D warnings` gate with holes in a third of the kernel is not a gate | 2026-07-30 |
| 42 | BUILT | [Supply chain and fuzzing in CI (extends the 2026-07-30 CI audit)](42-supply-chain-and-fuzzing.md) | we confine code we did not write, and the parsers that read what firmware and disks hand us are where a bound is a lie | 2026-08-02 |
| 43 | NOT-STARTED | [A second security audit, with a different lens](43-second-security-audit.md) | the attack surface roughly doubled after the first audit was written | |
| 44 | PARTIAL | [GitHub repository hardening: policy, private reporting, code scanning, pull requests](44-github-hardening.md) | a repository with a security thesis should be able to receive a report privately | |
| 45 | BUILT | [Triage the CodeQL code-scanning alerts, and decide what the tool is for](45-codeql-triage.md) | the alerts land on this project's most-used unsafe abstraction | 2026-07-30 |
| 46 | BUILT | [Rename the components for what they are, and write down the naming rules](46-component-renames.md) | a name is a claim, and `-d` claims something we rejected; conventions that matter get a checker, not a paragraph | 2026-07-30 |
| 47 | IN-PROGRESS | [Navigation and naming: cd, pwd, ls, mkdir, rm, paths, and environment](47-navigation-and-naming.md) | **divergence from Unix must be earned, never stylistic.** Keep the commands; change only what the capability model actually forces, and get one missing primitive right | |
| 48 | NOT-STARTED | [Job control: jobs, wait, kill, fg, bg, and a stopped state](48-job-control.md) | **most of it needs no new kernel surface**, and the tty's most tangled feature turns out to be a capability transfer | |
| 49 | NOT-STARTED | [Users, login, and attribution: what identity is for once it stops being authority](49-users-and-attribution.md) | three of Unix's four uses for a uid are already answered structurally; the fourth, **attribution, has no mechanism at all** | |
| 50 | PARTIAL | [Pipes and redirection: one sink protocol, and `\|` turns out to be an endpoint](50-pipes-and-redirection.md) | the sink contract is **built** (`crates/sink_proto`, notes/sink-protocol.md) and a program is proven indifferent to what its output slot holds; all four operators run at a real prompt on both ISAs. Remaining: buffering (measure a pipeline against a Unix pipe first), the terminal's own sink adapter, and `2>`, whose fork closed as a manifest declaration (§67) | |
| 51 | BUILT | [Wall-clock time, the `date` command, and an NTP service](51-wall-clock-time.md) | the machine knows what time it is: two RTC drivers, the clock service (§43), `crates/calendar`, `crates/ntp_proto`, `date`, and an NTP client holding **propose and not set**. `date` prints the time at the interactive prompt on both ISAs, the clock delegated read-only by both boot paths. Continuous polling waits on the timed-wait kernel fork, which the block records as tracked separately | | 2026-08-03 |
| 52 | RECORDED | [Subshells without `fork`, and what copying an endowment means](52-subshells.md) | `( ... )` is fork, we deliberately have no fork, and **capability duplication is not a total function** | |
| 53 | NOT-STARTED | [The board's own peripherals: network and storage on real silicon](53-board-peripherals.md) | 16a boots the board; this is what makes it able to *do* anything, and it is where virtio stops carrying us | |
| 54 | NOT-STARTED | [A network file service a Mac can actually mount](54-network-file-service.md) | the first real workload with a real user, and the security claim backup servers deserve | |
| 55 | NOT-STARTED | [Time Machine: SMB3 with Apple's extensions, and mDNS](55-time-machine.md) | **likely the largest single piece of work in the project**, and the one that must be scoped before it is started | |
| 56 | BUILT | [Secrets, credentials, and the entropy to make them safe](56-secrets-and-entropy.md) | **built 2026-08-01**: entropy (§44), the Argon2id crypto taken as a dependency per §46, and the credentialer, a store with no getter that verifies and never reads back (§54). The thesis-level gap it named, that *a secret is still a bearer token where a capability is an unforgeable reference*, is **milestone 65's** subject: hold the key, expose the operation | 2026-07-31 |
| 57 | BUILT | [Partitioning and formatting a real drive, and extended attributes](57-partitioning-and-xattrs.md) | you cannot find a partition without reading the table, and all of it is testable in QEMU before the board lands. Built: the host recovery tool (`ls`/`cat`/`extract`/`xattr`), `crates/gpt`, the **extended-attribute layer**, and (2026-08-03) **reading a real table on the target** plus **block-device enumeration**, which is a read-only roster page. What is left is the **write** half, and it is one decision rather than a task: partitioning and on-target `mkfs` both need randomness, and the `mkfs` half needs a new divergence from the RedoxFS pin | | 2026-08-03 |
| 58 | NOT-STARTED | [RISC-V TLB shootdown, and the flush that makes ASIDs pointless](58-riscv-tlb-shootdown.md) | every riscv context switch discards the whole TLB; the fix needs a **software** shootdown protocol, because `sfence.vma` does not broadcast | |
| 59 | BUILT | [The CPU-model matrix: stop testing against one generous emulator](59-cpu-model-matrix.md) | `-cpu rv64` enables nearly every ratified extension; the board is an RV64GC U74. `script/cpu-matrix` runs the riscv64 suite across five models and all 211 tests pass on every one, so we are already portable to the board's ISA. The ASID test written *for* the board is the gap no model can exercise | 2026-08-01 |
| 60 | BUILT | [ISA discovery: read the machine instead of assuming it](60-isa-discovery.md) | one `Isa` record per ISA, built at boot, printed at boot, in `crates/isa`. RISC-V parses the device tree (there is no `CPUID`) and keeps its `satp.ASID` probe; aarch64 decodes `MIDR_EL1` and `ID_AA64MMFR*`, because ARM never removed the CPU's self-description. **Four call sites vary, not the predicted five or six**, and two of the entry's four candidates dropped out. QEMU `virt` declares Sv57 while we run Sv39 | 2026-08-03 |
| 61 | BUILT | [The caretakers: one verb table, and names that say what you get](61-caretakers.md) | **built, both ISAs.** The rename landed first (532 tokens, not four filenames); `fs_proto::verb` is one row per opcode and a verb with no row is a compile error; all three caretakers forward the four extended-attribute verbs, proven by three witnesses each with a control that must fail | 2026-08-01 |
| 62 | NOT-STARTED | [Tests that assert on time: make a red run mean something](62-time-sensitive-tests.md) | ~19 bounded spins (`for _ in 0..N { yield_now() }`) and wall-clock assertions flake under load. Four separate lanes and the integrator hit them on 2026-08-01; the CPU matrix multiplies the exposure fivefold | |
| 63 | BUILT | [Directory and package names: one spelling per thing](63-name-spellings.md) | **built, both ISAs.** Eight crates, fourteen programs and modules, and the three violating directories renamed to the spellings settled in review; `fs-server` is `fs_server`, `user-std`/`hellostd` is `std_exerciser` twice, and the shell has a name (`swish`). Its tables keep the old spellings on purpose, because they are the record of the decision | 2026-08-01 |
| 64 | NOT-STARTED | [Enough `std` to run somebody else's crate](64-std-for-real-crates.md) | milestone 27 shipped the PAL; `fs` answers `Unsupported` in 32 of 54 functions and `thread` in 4 of 6. Measured against real crates.io dependencies rather than guessed at, because the gap that matters is the one a chosen crate actually hits | |
| 65 | NOT-STARTED | [A secrets service: hold the key, expose the operation, never the key](65-secrets-service.md) | NTLMv2 does not verify a presented secret, it **computes with a key**, so §54's verifier shape does not fit it. Generalises the credentialer into a software HSM. Blocks milestone 55 | |
| 66 | NOT-STARTED | [Vaultwarden: somebody else's real application, running here](66-vaultwarden.md) | the north star for "runs real workloads". Names the gaps concretely rather than aspirationally: no TCP **listen or accept** in the socket contract, threads mostly stubs, most of `std::fs` unsupported, no async runtime, no TLS, and SQLite is a C library. Largest single item on this roadmap | |
| 67 | NOT-STARTED | [`swish` the language: quoting, sequencing, and exit status](67-swish-language.md) | `swish` is an interactive shell without control flow. Quoting is the one that is a correctness gap rather than a convenience: **a filename with a space is currently unnameable** | |
| 68 | PARTIAL | [Code-quality gates: one lint policy, and the lints that lost](68-code-quality-gates.md) | Import order, `[workspace.lints]`, dependency direction, unused dependencies, spelling. Three lints were adopted, measured and **removed** on the evidence. `undocumented_unsafe_blocks` is now a GATE: all 205 undocumented blocks were read and commented. Doc examples went 5 -> 23 across nine crates, which is a start and not the standard; `missing_docs` is still not adoptable | |
| 69 | BUILT | [Split `kernel/src/user.rs` by service](69-split-user-rs.md) | 15,499 lines and **46 top-level modules** in one file: a dozen `*_service` modules and ~34 test modules. The split is nearly free because the boundaries are already `mod` blocks, so moving one to its own file changes no visibility and no API | 2026-08-02 |
| 70 | BUILT | [`swish`'s remaining logic in a crate, host-testable like its siblings](70-swish-logic-crate.md) | `coremark`, `line_editor` and `compositor` are each a crate holding the logic plus a program holding the IO. `swish` is the largest program that is not, so its dispatch, endowment preview and outcome handling are reachable only through QEMU | 2026-08-02 |
| 71 | BUILT | [The thread-start fault: a user thread dispatched with `sepc` = 0](71-thread-start-fault.md) | Frame placement, as this entry guessed. RISC-V put the frame 16 bytes under where `trap.s` builds an S-mode frame, so any interrupt in the window rewrote it and the user `sp` read the trap frame's hardwired-zero slot. Reproduced deterministically by widening the window; fixed by placing the frame at the stack top on both ISAs | 2026-08-03 |
| 72 | BUILT | [A lost wakeup that a hundred leaked threads may be causing](72-lost-wakeup.md) | Not the leak, and not RISC-V. One line of test code probed `reclaim_region(...).is_err()` on its own child's TCB region, which under §16 as amended **arms the kill**; the child was reaped before it could SEND. Widening the window reproduces it on aarch64 too, first run. The 101-thread accumulation is real, unrelated, and wants its own entry | 2026-08-03 |
| 73 | BUILT | [Name the aarch64 files aarch64, before x86_64 makes it worse](73-aarch64-file-names.md) | Five files carried a riscv name while their aarch64 twin carried none, so the unnamed one read as "the general case" and was not. Both sides now carry the ISA, and a sixth file the entry had missed (`qemu-virt-initrd.dtb`) came with them. `user/link.ld` is genuinely shared and was NOT renamed, nor was `riscv_virtio_tests.rs`, which has no twin. `crates/paging` moved OUT to milestone 77 | 2026-08-03 |
| 74 | NOT-STARTED | [Cycle counters: SBI PMU on RISC-V, `PMCCNTR_EL0` on aarch64](74-cycle-counters.md) | 16a's deliverable names "benches on real cycles via the SBI PMU extension" and **nothing implements it**, on either ISA. Both read a fixed-rate TIME counter today, not cycles. Gates milestone 25's `sel4bench`, which was deferred to hardware for exactly this | |
| 75 | NOT-STARTED | [Who may read the cycle counter, and by what authority](75-cycle-counter-authority.md) | Opening `PMCCNTR_EL0` to EL0 is not the same decision as opening `CNTVCT_EL0` was: it is **~160x finer** (~0.25 ns against ~41 ns), and the generic timer's coarseness was doing real security work. A capability is the answer this OS already has, and notes/abi.md anticipated it | |
| 76 | BUILT | [Split the roadmap: `design/roadmap/README.md` as index, one file per milestone](76-roadmap-split.md) | **built 2026-08-03, the day the single file (by then 6,200 lines) took nine entries and two more same-day PR conflicts.** The split is this directory, proven by byte-for-byte reassembly; the gate now checks index/file status agreement, one milestone per file, and every `milestone N` citation tree-wide (2,255, all resolving), with 1 to 11 backfilled from the first commits and the `n >= 12` floor gone | 2026-08-03 |
| 77 | NOT-STARTED | [`crates/paging`: a module per ISA, a type per page-table configuration](77-paging-configurations.md) | `Aarch64` names an ISA while describing a configuration, beside `Sv39` which names one properly. A second aarch64 configuration is expected, so the fix is room for siblings on both sides rather than a rename. **Waits for that configuration**, because it names the axis | |
| 78 | PARTIAL | [The load-sensitive assertions, and the three that measure the wrong thing](78-load-sensitive-assertions.md) | **Seven** distinct failures in one day on PRs that changed no code, two of them documentation only, and one reproduces off CI. Three report a NEGATIVE discrepancy, so they are not slow-machine timeouts. For the two that ARE timing, the answer is likely the icount instrument this project already owns, not a wider bound | |
| 79 | BUILT | [Miri over the host crates](79-miri.md) | The method is pure logic in host-testable crates, and Miri checks exactly those tests for the undefined behaviour Kani is not asked about and fuzzing cannot see. The pinned nightly already ships it. Weekly, not per-PR, because the cost is runtime | | 2026-08-03 |
| 80 | NOT-STARTED | [Loom: the hand-rolled atomic protocols, model-checked](80-loom.md) | TCG explores almost none of the orderings real silicon will, so an acquire/release mistake passes every gate this tree has and first appears on hardware. The board lands ~2026-08-21. One protocol as a pilot, then decide | |
| 81 | NOT-STARTED | [An HVF leg: the test suite on the physical core](81-hvf-leg.md) | `CRICKER_ACCEL=hvf` exists and bench uses it; nothing runs `script/test` there as a habit. GitHub's hosted runners cannot (no nested virtualization), so the leg rides `script/gates`: it runs wherever HVF exists and skips loudly where it does not. aarch64 only | |
| 82 | NOT-STARTED | [`unsafe_op_in_unsafe_fn`: the obligation moves inside the fn](82-unsafe-op-in-unsafe-fn.md) | 33 `unsafe fn`s get their bodies' unsafety for free, so one signature can hide several distinct invariants. Explicit interior blocks put milestone 68's SAFETY-comment lint on each one. A ratchet in §38's shape | |
| 83 | BUILT | [A mechanical rule-1 lint](83-rule-1-lint.md) | CLAUDE.md's first rule (architecture-specific code lives under `arch/`) is enforced by nothing, and one violation exists today: `user/tests.rs` reads `SPSel` by raw `asm!`. `script/lint` learns the grep; the violation moves | | 2026-08-03 |
| 84 | BUILT | [Stack high-water: measure kernel stack depth](84-stack-high-water.md) | The FS-server stack overflow already happened once, and nothing since bounds depth on any kernel stack. Paint at boot, read the mark at suite end, assert headroom. Works identically on every ISA and covers every path the suite takes | | 2026-08-03 |
| 85 | NOT-STARTED | [Mutation testing over the host crates](85-mutation-testing.md) | Coverage reports what ran; cargo-mutants reports whether a test would notice a change, which is the claim the suite actually makes. A weekly, time-boxed job with a recorded baseline, not a PR gate | |
| 86 | NOT-STARTED | [`time`: the shell times a command](86-time-command.md) | The second prefix-word command after `caps`, so the grammar is proven, and `date` already built the clock story. The design question is whose clock it is: the shell's, so a child that holds no clock capability can still be timed, which is the Unix behaviour and the leaning | |
| 87 | NOT-STARTED | [The x86_64 bare-metal machine](87-x86-machine.md) | Milestone 19's third ISA needs what milestone 16's second needed: a dedicated, brickable board, selected before the port so the requirements drive the purchase. Selected: a used OptiPlex 7050 Micro plus the C4PDJ serial module, ~$194 all-in; every new option cost $150-350 more at real prices. QEMU emulates `igb` and `e1000e`, not `igc`, so the 7050's I219 keeps the one-driver property with no caveats | |
| 88 | NOT-STARTED | [cricker-os on rented silicon: Oracle's free tier first, Graviton metal for the PMU](88-rented-silicon.md) | "Here is the image, rerun it on your own free account" is a credibility claim no desk machine can make, at $0 recurring. OCI's A1 VMs are KVM with virtio, which this tree already drives; the PMU stage stays Graviton `.metal` by the hour, unblocking milestone 25's deferred `sel4bench`. Costs a UEFI boot path and an ACPI front door, both shared with optional milestone 24 | |
| 89 | NOT-STARTED | [Scaleway EM-RV1: a second RISC-V implementation, rented](89-scaleway-em-rv1.md) | Real riscv64 silicon (T-Head TH1520, C910 cores) at EUR 0.042/hour, the vendor-quirk cousin of the cpu matrix's `thead-c906` model. A second implementation's answers to the questions QEMU cannot vary (the `satp.ASID` probe above all), independent of the VisionFive 2's arrival. Whether a custom kernel can boot there at all is the first fact to establish | |
| 90 | BUILT | [A guard page under the per-CPU secondary stacks](90-secondary-stack-guard.md) | Milestone 84's instrument found the asymmetry: the boot stack and every thread stack sit above a guard page, and the per-CPU secondaries sit above `.bss`, so a deep secondary silently corrupts kernel data. The high-water assertion is the only tripwire today, and it is `cfg(test)`: a release build has nothing. Move the stacks over a hole and prove the hole by walking the tables | | 2026-08-03 |
| 91 | NOT-STARTED | [A glossary, and every acronym linked to it](91-acronym-glossary.md) | Raised from the reader's chair: the acronyms are the hardest part of the docs. ~835 distinct all-caps tokens measured, IPC alone appearing 251 times with no reachable expansion. Every prose use links to an anchored entry (readers land mid-file); backticked tokens are code and exempt; a lint gate keeps new acronyms from arriving undefined | |
| 92 | NOT-STARTED | [Security audits as a mechanism: cadence, docs, and findings that become milestones](92-security-audit-cadence.md) | One audit happened and milestone 43 asks for a second; this is the machine that makes them routine. A recorded cadence with a drift-style overdue tripwire, a lens rotation, docs re-baselined in the same lane, and every finding ending fixed, minted as a milestone, or recorded-accepted; "noted" is not a state (§35's wallpaper rule, applied to audits) | |
| 93 | NOT-STARTED | [Documentation audits as a mechanism: the docs stay true to the tree](93-doc-audit-cadence.md) | 92's sibling for claim rot: four stale claims were found in one day, every one by accident. Sweeps read notes against the tree and the roadmap both (planned-as-built rots too), findings end in 92's three states, and each audit converts checkable claims into checked ones so the auditable surface shrinks instead of holding still | |
| 94 | NOT-STARTED | [The untracked-work sweep, and the convention that ends the category](94-untracked-work.md) | Work identified in evaporating media (lane reports, PR comments, commit messages): milestone 90 exists only because Chris caught 84's finding in a report. One sweep drains the backlog (11 TODO-class markers, 29 notes with deferral phrasing, as the floor); the conventions stop the refill: reports propose milestones or place BUGS records, the merge checklist enforces it, and a TODO cites its milestone or fails lint | |
| 95 | NOT-STARTED | [An unmap primitive, and the mappings init never lets go](95-unmap-primitive.md) | Milestone 22's largest residual: `build_child` maps every page it writes for a child and nothing in the ABI can unmap it, so init keeps a writable window onto every boot server for the life of the machine. A design fork before it is a task, and the cheapest fix may be a one-page loader rather than a new syscall | |
| 96 | NOT-STARTED | [One init: the spawn service written twice](96-one-init.md) | aarch64 boots `hello.rs`'s init role and riscv64 boots `system_initializer`, with ~140 near-identical lines of spawn service in each. A fix landing in one and not the other presents as a boot that reaches userspace and prints nothing, which has now cost three lanes an evening. Rule 7 says what two binaries share is a crate | |
| 97 | NOT-STARTED | [Citations that name what they cite](97-citations-that-name.md) | 23 sites cite "milestone 24" meaning DECISIONS §24, wrong at birth (the roadmap said Virtualization.framework on the day the first was written) and spread by copy-paste. Neither gate can see it, because both numbers resolve. The durable half is a lint that checks a citation's parenthetical name against the real title | |
| 98 | NOT-STARTED | [The scheduler that stopped scheduling: name what `SCHED` actually guards](98-scheduler-naming.md) | Asked as `sched` vs `scheduler`, answered by the code: §11 moved the run queue and `current` to per-CPU storage, so the `Scheduler` type now holds a thread table and an endpoint registry and schedules nothing. The module keeps `sched` (it really does schedule); the type and static want a name for what they guard. ~100 sites in one file, not 915 across 70 | |
| 99 | NOT-STARTED | [`git` on cricker-os: the tool this project is built with, hosting its own history](99-git-on-cricker.md) | A second real-workload target beside 66's Vaultwarden, chosen because local git needs no network, no threads, no async runtime and no SQLite: it is a filesystem, a hash, a compressor and a clock, which is what milestone 57 just finished building. First fork is gitoxide (Rust, rides the std PAL) versus C git (needs a libc, `fork`/`exec` this kernel lacks, and mmap); recommendation on the record is gitoxide | |

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
earns, and it re-touches the parked competitor story. The broad competitor ambition stays parked (see
[the competitor question](../competitor-question.md)).
Several milestones already have their design worked out; their files point at it.

**The Prior-art sections in the milestone files cover reuse too.** Before building, each milestone design answers
three questions against the ecosystems in notes/prior-art.md (Redox, rCore, Tock, Hubris, seL4,
Fuchsia): is there code to use, a design to copy, or a mistake to avoid? The build-vs-reuse call
gets recorded with its reason. The rule that decides it: the reuse boundary is the TCB boundary
(inside it, always build; userspace, actively prefer porting), and no reuse may widen the syscall
surface or smuggle in POSIX assumptions. notes/prior-art.md has the full argument.

The prose essays that lived in this file before the 2026-08-03 split are their own files now:
[the display ladder](../display-ladder.md), [the backup-server ladder](../backup-server-ladder.md),
[the competitor question](../competitor-question.md), and [the eBPF rival](../ebpf-rival.md).


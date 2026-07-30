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

| #  | Status | Milestone | What it delivers | Serves §14 by |
|----|--------|-----------|------------------|---------------|
| 12 | BUILT | Call/Reply IPC: a one-shot reply capability | Reply-to-caller as a kernel guarantee. **Built, §12.** | the IPC the TCB must get right |
| 13 | BUILT | Capability revocation + untyped reclamation | Unmap a page from every holder; reclaim a region safely. **Built (frame scope), §13.** | safe teardown, a TCB property |
| 26 | BUILT | Object revocation: tear a process back down | Reclaim the TCBs, address spaces, and endpoints a process built, and the regions behind them, so a workload that comes and goes can leave. **Built:** region-ownership + generational staleness (no CDT), `Untyped::SPLIT`/`DESTROY`, generational region slots (retires the 256-lifetime cap), endpoints (safe subset). Extends §13 from frames to objects; DECISIONS §16, notes/object-revocation.md | **the teardown half of "run real workloads":** a process can be reaped, not just built |
| 18 | BUILT | Verify the capability core, then spread inward | Machine-checked proofs of `caps`, then IPC, then MMU isolation | **the verification itself.** **Built:** `caps`, IPC (rendezvous + one-shot Reply), and the MMU isolation invariants are all proved |
| 14 | BUILT | Kernel objects from untyped: remove the kernel heap | Retype TCBs, endpoints, page tables; delete the kernel heap | **critical path:** a verifiable kernel cannot allocate. **Built:** the kernel has no allocator; see design/kernel-objects-from-untyped.md |
| 15 | BUILT | Tagged address spaces (ASIDs) | 16-bit ASIDs, generation/rollover; stop flushing the whole EL1 TLB per switch | perf the real-workload path needs on real silicon. **Built** (8-bit fixed bitmap, no rollover: milestone 14's bounds made generations unnecessary; notes/asids.md) |
| 21 | BUILT | Performance measurement: benchmarks with teeth | icount microbenchmarks + committed baseline that fails on regression; HVF-native runs for real magnitudes | perf claims become measurements; regressions surface next to their cause. **Built**; notes/benchmarks.md |
| 16 | PARTIAL | Real hardware + IOMMU-backed driver isolation, **RISC-V first** | **16a:** first silicon on a VisionFive 2-class board, whose firmware contract (OpenSBI, SBI HSM, NS16550, PLIC, Sv39) is exactly what the kernel already speaks. **16b:** IOMMU-backed DMA isolation against QEMU's emulation of the **ratified RISC-V IOMMU** (v1.0.1) first, over the §18 PCIe transport; silicon when a board ships it | isolation in hardware, under real workloads; the second ISA becomes the first silicon, and the IOMMU work stops waiting on a purchase |
| 19 | BUILT | Run a real workload | A native-ABI workload first; Linux-compat or VM hosting later | **the "runs real workloads" half** of the thesis. **Built:** granular verbs and userspace init (19d), init as the real boot path (19d.2c), dedicated binaries delivered as a crickerfs archive with a shared `user_rt` runtime (19f.1-6), the native ABI written down (19e/Decision 2, notes/abi.md, DECISIONS §15), and the first real workload, a CoreMark-derived compute program spawned against that ABI (19e). design/init-and-granular-spawn.md |
| 17 | OPTIONAL | Multikernel-leaning scheduler (research, optional) | Partition the shared thread table and endpoints | optional; not on the thesis path |
| 20 | BUILT | A portable HAL, proven on a second architecture | Make `arch/` a real HAL; bring up RISC-V then x86_64 | the "portable verified core" claim; reach the demonstrator earns |
| 24 | OPTIONAL | A second aarch64 *board*: Virtualization.framework (optional) | Boot under Apple's Virtualization.framework, not QEMU's `virt`: a virtio-console driver (VZ has no PL011), VZ's interrupt/memory layout and boot handoff, device discovery through the machine VZ presents | proves the `arch/` **board** boundary on a second machine of the *same* ISA (cheaper than 16's silicon, distinct from 20's second ISA), and lets cricker-os run under the same VMM as macOS/Linux guests. Optional; portability exercise, **not** a benchmarking prerequisite (guest-internal microbenchmarks are VMM-independent) |
| 27 | BUILT | Rust `std` on the native ABI | A custom target whose `std` builds: `Vec`, `String`, `println!`, `Instant`, allocation from the process's own untyped, stdio over the console endpoint, `fs`/`net` honestly `Unsupported` until capability-granted servers back them | **widens "runs real workloads" by orders of magnitude**: the pool of programs that build for cricker-os becomes "most Rust code that doesn't touch fs/net", and milestone 23's components become writable by people who are not kernel people. Grows toward general purpose (notes/why-not-general-purpose.md) without smuggling POSIX: the `sys` layer maps to capabilities directly, no fork, no open-by-path |
| 28 | BUILT | A solid terminal: the line discipline as a component | Line editing, history, ANSI in/out, control characters, and a written terminal contract, as a **swappable userspace component** between the input/console drivers and applications; Ctrl-C as a capability-routed interrupt to the foreground process, not a Unix signal. **Built, §21**: `termd` on both ISAs, a sans-IO engine (20 host tests), the contract in notes/terminal-contract.md, `shell_service` retired for userspace init; Ctrl-C routing **built** (two-tier, DECISIONS §24 amendment): a shared-flag cooperative tier and an `Untyped::DESTROY` forcible tier, shell-held, proven on both ISAs with `heeder`/`spinner`; the shell learns of `^C` through `termd`'s `OP_INTRCOUNT` | a terminal with real behavior is a far better "instance one" for milestone 23's live component replacement than the raw echo loop, and 27's stdio semantics need a terminal that has semantics. Serial, deliberately; the display terminal is 29, and they must not be confused |
| 29 | PARTIAL | A display terminal (framebuffer, virtio-gpu) | An on-device terminal: a userspace virtio-gpu driver (arriving over **PCIe**, which the §18 transport just made reachable), a framebuffer component, font rendering, and a VT state engine maintaining the grid; input from a virtio keyboard | the first pixels the demonstrator ever puts on a screen, and the strongest form of the milestone-23 claim if the VT engine is **libghostty-vt** (zero-dependency, no-libc, no-alloc, C ABI, Zig): a vendor component in a foreign language, capability-confined and hot-swappable. **Promoted from optional (2026-07-28): rung one of the display ladder (see "The display ladder" below), whose destination is a capability-routed compositor** |
| 30 | BUILT | The network stack as a confined component | A userspace **virtio-net** driver behind the DMA confinement (extended to multi-queue: RX means the device writes INTO driver memory), and the TCP/IP stack itself (`smoltcp`) as a swappable userspace component with a capability-shaped socket contract; backs `std::net` for 27 | **the canonical microkernel component**, the one people ask about first when a minimal kernel claims to stand next to Linux; and milestone 23's most convincing instance, hot-swapping a network stack under open connections. The reuse call is the plan's easiest: the thesis is the kernel confining the stack, not the stack |
| 31 | IN-PROGRESS | A capability shell: designation is authorization | The command line becomes a **grant expression**: naming a resource in a command IS the capability grant (`run wc report.txt` passes one readable file cap; `run wc` alone can read nothing, and the refusal is "no such capability", not EPERM); untyped budgets as first-class grants; a SHILL-style manifest per program checked at spawn; a `caps` command printing a process's whole endowment. **Phase 1 built, both ISAs**: `capsh` (host-tested parse + manifest + spawn protocol), the shell over the existing surface, `run --mem N` made real by the `budgeter` program (from the shell's own budget, via a `SEND_CAP`-to-init spawn protocol), manifest refusals and the "you hold no such capability" file refusal at the prompt, `caps`/`caps run ...` introspection. One kernel bug fix: `Untyped::SPLIT` now grants the child `GRANT` so a budget is delegable (DECISIONS §16 amendment). Notes: grant-expression.md, program-manifest.md. Per-file grants wait on milestone 32 | **no-ambient-authority made user-visible**: the inversion of Unix's model at the one interface a human touches. Milestone 23's component contract in embryo, met first at the shell |
| 32 | BUILT | A real filesystem: RedoxFS behind a capability FS server | A write-capable block path, an FS-server **component** whose handles are capabilities from birth (open-by-path exists only INSIDE the server, relative to a granted directory cap), and **RedoxFS** as the on-disk engine, ported behind its own `Disk` trait over blk IPC | the flagship **userspace-reuse** story the prior-art note predicted: a real CoW filesystem we did not write, running confined; and the thing 31's per-file grants point at |
| 33 | BUILT | A compositor: one screen, mutually distrusting clients | **Built (2026-07-29), both ISAs**, rung two of the display ladder: `compd` multiplexing one screen among three clients, each holding a capability to its own surface; software composition honouring a damage rectangle; input routed by capability over the terminal contract's `OP_BYTES`; enumeration and screenshots as read-only mappings rather than verbs. No new syscall and no new method. notes/compositor.md, DECISIONS §33 | **the canonical multiplexer of one device among distrusting clients**, and the thesis at its sharpest: a client is *proved* unable to reach its neighbour's pixels even when handed the exact address of them, and the compositor holds no authorization code because the authority is a mapping rather than a message. It also found the kernel's one missing primitive (no wait-any), recorded as a fork |
| 34 | NOT-STARTED | GPU acceleration via virtio-gpu 3D (the display ladder's rung four) | The **Venus** path: Vulkan commands serialized over the virtio-gpu device, arriving on the §18 PCIe transport, so the guest gets real GPU acceleration without owning a hardware driver. Needs the 3D context and command-submission side of virtio-gpu that rung one deliberately left alone (rung one sets up no cursor queue and no 3D context, keeping the §23 two-queue ceiling untouched), the confinement story extended to command-carried backing addresses (DECISIONS §30's residual gap: those are the addresses the descriptor validator structurally cannot see, and today only an IOMMU stops them), and something to consume it, which is what would give `wgpu` a real target | **how every VM gets a GPU without a hardware driver**, and the honest ceiling on the display ladder: rung five (a bare-metal driver for the VisionFive 2's BXE-4-32 3D core) is struck as a Linux-scale multi-year effort that proves nothing this does not. A mountain, priced as such, and it reopens the parked competitor question the ladder's governance note names as the architect's call |
| 25 | PARTIAL | Cross-OS performance comparison (extends 21) | EL0-measured primitive benchmarks (syscall, context switch, IPC, map, spawn) the lmbench way, so the numbers include the trap the kernel-side benchmarks skip; then line them up against lmbench (Linux, macOS guests) and `sel4bench` (seL4), at a matched virtualization tier, with release builds. Fold in the icount codegen-sensitivity fix. | **turns perf claims into cross-OS numbers**: where does a Rust capability microkernel stand next to Linux, macOS's XNU, and seL4 on the primitives that define an OS. **Largely done**: four EL0 primitives (null syscall, context switch, IPC, page map) on both instruments, a release build path, and the three-way comparison (cricker-os vs Linux-under-HVF vs native macOS) with cricker-os winning null/IPC ~5x. `spawn` landed too (its real prerequisite was never retype, which had already shipped, but **object revocation**, reclaiming a child's TCB/aspace/endpoint so a spawn loop can repeat; that shipped as its own milestone, notes/object-revocation.md, and the EL0 `lat_proc` bench, `spawn_el0`, is in the suite and the committed baseline). **Remaining**: only `sel4bench` (built and booting for qemu-arm-virt, but it times single ops via the PMU cycle counter, which neither QEMU-TCG nor Apple HVF provides, so it is **deferred to real hardware**, the milestone-16 machine, which has a real PMU; this validates our CNTVCT + long-loop design). notes/benchmarks.md |
| 22 | PARTIAL | Trusted init: verify it, and shrink what a broken one can do | Measured/secure boot that checks init before running it; reduce init's authority so a compromise is bounded | **closes the thesis's own soft spot:** init is the privileged *unverified* component the whole system is built by |
| 23 | NOT-STARTED | A capability-routed component OS with live replacement | Every userspace component (driver, server, app) is a swappable, vendor-shippable unit behind a stable contract; operators replace them live, no reboot. The console hot-swap is instance one; a durable queue-broker decouples component lifecycles (opt-in per channel, for latency) | **the flagship payoff and a product ambition:** competing vendor components, confined by the kernel and swapped live; the verified core is the one fixed thing |
| 35 | BUILT | Prove the DMA-confinement boundary (extends 18) | Extract the shadow-ring validator (`validate_and_shadow`) out of `kernel/src/virtio.rs` into a host-testable logic crate and machine-check it: no validated descriptor chain, in either direction and including indirect descriptors and multi-queue, can reference memory outside the driver's granted DMA region. Add the `Untyped::SPLIT` "never widens rights" harness (the one fresh-mint site the caps proof doesn't reach) and confirm the IOMMU domain builder's *maps-exactly-the-grant* property is proved, not just tested. | **closes the one isolation boundary we test instead of prove.** Every other confinement seam (caps, MMU, IPC, generational names) is Kani-proved for all inputs; DMA is attacker-tested only. It is also the boundary that makes "don't trust the driver" true, so the proof belongs here, not on the confined component. **Load-bearing for 16a:** the VisionFive 2 has no IOMMU, so on first silicon this validator is the *sole* DMA confinement, not defence in depth |
| 36 | BUILT | A foreign-language component, seam first (spike; feeds 29 and 23) | Prove the FFI seam end to end with a *minimal* C component before committing to a large one: bare-metal clang for both bare targets in the build, a Rust `user_rt` shell that holds every capability and does every syscall while the C code gets plain buffers over the C ABI (so the §4 surface does not widen), and only the handful of libc symbols the component actually needs, with `malloc` on milestone 27's untyped-backed `GlobalAlloc`. The deliverable that matters is one test: a deliberate out-of-bounds write in the C code faults the process, touches nothing outside its grant, and its supervisor restarts it. **Built, DECISIONS §31, both ISAs**: clang capability-checked for both backends from one compiler (Apple's is rejected: no RISC-V), `cshim` holds every capability so the C holds none, the libc turned out to be **two** symbols not five (`compiler_builtins` already supplies the rest), and two witnesses prove the confinement (a read-only page that is the *same physical frame*, and a different frame at the same virtual address). notes/c-seam.md | **the thesis in one assertion.** Memory-unsafe foreign code is not a dilution of "a verified core that confines unverified workloads", it is the strongest available demonstration of it: the more unverified the component, the more the confinement has to prove. It also de-risks 29's libghostty-vt rung and 23's vendor-component claim *before* we owe anything to another project's toolchain or API churn |
| 37 | NOT-STARTED | Prove RedoxFS's crash consistency (DECISIONS §34, condition 1) | Inject the failure a copy-on-write filesystem exists to survive, and measure whether it does: torn writes (a block partially written), dropped writes (a write the device acknowledged and did not persist), and a kill mid-transaction, then reopen with the same `cleanup: true` header-ring replay the FS server always mounts with, and assert the filesystem is consistent and every acknowledged write is either wholly present or wholly absent. The seam is `IpcDisk` and the block server, which sit between the engine and the device and can drop or truncate a write deliberately; the sans-IO core already runs on the host against a real image, so most of this is host-testable in milliseconds and only the device-level kill needs QEMU. Includes the negative control that makes the rest mean anything: the injector must be shown to actually corrupt something when the replay is disabled | **the condition that decides whether §34's label is earned.** Crash consistency is RedoxFS's central selling point and the reason it beat ext2, and we currently assert it on the strength of the upstream design description rather than any measurement. That is a claim of exactly the kind this project's rules forbid, and it is the first thing a skeptic asks a filesystem. Until it passes, the docs say "designed for crash consistency" and never "crash consistent". Note this is a gap in **our harness, not in RedoxFS**: no candidate engine's crash consistency is tested here, so switching engines would not address it |
| 38 | NOT-STARTED | Filesystem throughput, and the comparison (DECISIONS §34, condition 2; extends 21/25) | Sequential and random read/write throughput through the confined FS server, against ext4 on Linux and APFS on macOS at a matched virtualization tier, the way milestone 25 did the primitives. Requires deciding what is honestly comparable: our reads are device-latency-dominated (`fs_read` is ~204 us/read under HVF, and `relay_rtt` puts the isolation tax a thousand times below that), so the interesting question is whether the userspace-server architecture costs throughput once the device dominates, which is a claim a microkernel skeptic will press | **"primary filesystem" invites a comparison we cannot currently make.** We have the per-request numbers and the isolation tax, and no MB/s figure at all. Milestone 21's rule is measure rather than argue, and 25 already established that the honest way to do this is EL0-measured against real systems rather than self-reported. This is where the "userspace servers are too slow" objection gets an answer or a concession |
| 39 | RECORDED | Repository structure for a loosely-coupled OS, and the road to a distribution | **Analysis recorded, no decision taken.** The tree is a monorepo for a deliberately loosely-coupled system, and it is straining in measurable ways: `user/` is 28 binaries and 9,324 lines in one crate that is also a shared library, `fs-server/` has already escaped into its own workspace for real dependency reasons, `crates/` conflates kernel proof crates with wire contracts and userspace runtime so the boundary a third party cares about is invisible, and every crate is version 0.1.0. Four options are written up with their trade-offs (restructure in place; multiple workspaces in one repo; split repos; monorepo plus a later distribution *manifest* repo), along with a naming argument (**components** and **services**, never "daemons", because a Unix daemon is defined by the ambient authority this OS does not have) and the observation that milestone 31's program manifest plus §22's measured-boot hashing are already three quarters of a package format | **the structure has to serve the thesis, and one constraint dominates.** A single `script/test` proving the whole system on both ISAs is this project's credibility mechanism and what makes rule 5 a gate rather than an aspiration; splitting repos trades that for decoupling nothing external needs yet. Recommendation recorded (monorepo now, distribution as a separate manifest repo, executed as multiple workspaces, not before 23 forces it) so the eventual decision starts from evidence rather than from taste |
| 40 | NOT-STARTED | Documentation as a system service: searchable, rendered, and installed by packages | Markdown authored, **rendered** for display rather than shown raw, searchable locally, and installed by the package that owns it. Reuse `pulldown-cmark` for parsing (CommonMark is a fiddly spec worth taking from someone else) and write the ANSI renderer against `linedisc`'s contract, because `termimad`/`mdcat` sit on `crossterm` and assume a POSIX terminal we do not have. Phase 1 is a terminal viewer and pager, phase 2 a host-built inverted index shipped as a per-package shard, phase 3 a graphical viewer riding the display ladder. Two constraints found while scoping: **`readdir` refuses and the §27 contract has no such verb**, so nothing can walk a tree for documents, and **font rendering is still milestone 29's remaining increment**, so the terminal comes first | **the OS explains itself, on itself.** The project's whole argument is already markdown (DECISIONS, thirty-plus notes, this roadmap), so a capability-confined viewer serving them is a better milestone-23 demonstration than another synthetic test and costs the documentation nothing. The missing `readdir` turns out to be a feature: **enumeration is authority**, so indexing at package-build time is both the way around the gap and the more honest shape, which is the same answer `apropos` reached for a different reason. And `doc notes/ipc-naming.md` granting exactly one readable file is milestone 31's designation-is-authorization made into something a person uses |
| 41 | NOT-STARTED | Dead code: triage the suppressions, and un-blindfold the gate | Triage all **79** `allow(dead_code)`/`allow(unused)` suppressions in the tree, delete what is dead, and replace the module-wide ones with per-item allows that carry a reason. Three distinct classes, only one of which is tidying. (1) **The gate is blindfolded over 5,831 lines**: six files carry module-wide `#![allow(dead_code)]`, including `sched.rs` (3,166 lines) and `arch/aarch64/mmu.rs` (1,275), so `-D warnings` cannot see dead code in the two largest and most security-relevant files in the kernel. (2) **Suppressions whose own comments name milestones that have since shipped**, e.g. `cpu.rs`'s "by the scheduler in step 3" and `smp.rs`'s "by spawn's placement policy" (both landed as §28), `cap.rs`'s "in 9b", `interrupts.rs`'s "milestone 5's first non-test caller", and two in `mmu.rs` pointing at milestone 8's in-kernel console, which §21 moved to userspace. Each is either now-used (delete the attribute) or genuinely dead (delete the code); either way the comment is false. (3) **Superseded demo payloads** in `user.rs`, which say so themselves ("7c handed the demo over to the real ELF"). Ends with a lint gate refusing new module-wide suppressions, the same shape as the conflict-marker and roadmap checks | **a `-D warnings` gate with holes in a third of the kernel is a gate that reports success it has not earned**, which is the same class of problem as the four-times-corrected §27 record and the contradicted `fs_read` comment: the tooling said fine while nobody was looking. It also protects a real asset, since this codebase's unusually heavy commenting is only valuable while the comments are true, and a suppression citing a milestone that shipped weeks ago actively misleads. **Explicitly NOT in scope:** hardware register definitions (`gic.rs`, `timer.rs`, `semihosting.rs`, `mmu.rs` field encodings) where a complete definition is the point, and deliberate diagnostics (`VERIFY_WRITES`, `second_mount`) that encode measurements which killed hypotheses. Those keep their allows and gain a stated reason, which is the difference between a suppression and a decision |
| 42 | NOT-STARTED | Supply chain and fuzzing in CI (extends the 2026-07-30 CI audit) | Three things CI does not do. **Advisories and licences**: no `cargo-audit`/`cargo-deny`, so a published advisory against a dependency is invisible, and licence obligations go unrecorded, which stops being cosmetic the moment milestone 39's distribution exists. **Vendored integrity**: `vendor/redoxfs` is pinned at 0.9.1 with a `patches/` discipline and *nothing verifies the tree equals upstream-plus-our-patches*. **Fuzzing the parse surface**: Kani proves `elf`, `dtb` and `crickerfs` under *chosen bounds*, and a fuzzer explores byte sequences past those bounds and finds panics rather than property violations, which is complementary rather than redundant. Several crates are unproved entirely and take attacker-shaped input: the `fs_proto`/`gfx_proto`/`linedisc` decoders, `capsh` (which parses the human's command line), `compose` (clipping arithmetic, where its own note says off-by-one is the classic bug), and `measure`, the SHA-256 behind the measured-boot trust root | **the thesis is confining code we did not write, so not knowing when that code has a published advisory is an odd blind spot**, and milestone 32's flagship claim ("a real filesystem we did not write") is only as good as our ability to say what we are actually running. Fuzzing is the honest complement to bounded model checking: Kani answers "is the property true inside these bounds", a fuzzer answers "does anything crash outside them", and the project currently only asks the first |
| 43 | NOT-STARTED | A second security audit, with a different lens | The first audit (notes/arch-audit.md) read the **assembly and arch layer** and found three real bugs: the `eret`/`sret` privilege-escalation staging race, a stale `tp` on S-mode trap return corrupting cross-hart per-CPU data, and the PLIC's lock-free read-modify-write. A second pass should deliberately NOT re-read that, and should take the surface that has appeared since. Headline lens: **time-of-check to time-of-use across shared pages.** Every service contract now moves bulk data through a page shared with the client (blk, file, gfx, compose, linedisc, netd), so a server that validates a length or an offset from the request word and *then* reads the page has a double-fetch window a malicious client controls; 19 files touch that pattern. Further lenses: integer overflow in the wire's size and offset arithmetic (`fs_proto` packs a 40-bit length, and `TRUNCATE` takes a size in the second word); capability lifetime races between revocation and an in-flight use, now that generational names, `Untyped::DESTROY` and `Endpoint::REAP` all reclaim; and a census of the **804** `unsafe` occurrences, triaging which carry a stated safety argument | **the attack surface roughly doubled after the first audit was written**: the compositor's shared surfaces, the C seam, the reap right, `std::fs`/`std::net`, and the FS service all arrived afterwards. The first audit's value came from reading for a *pattern* rather than waiting for a failure (it found the PLIC race that way), so the return on a second pass depends entirely on choosing a lens the first one did not use. Double-fetch is that lens: it is invisible to every gate we run, because both the check and the use are individually correct |
| 44 | NOT-STARTED | GitHub repository hardening: policy, private reporting, code scanning, pull requests | Four items, and they split into files we can commit and settings someone with admin has to toggle. **Files:** a `SECURITY.md` policy stating what is in scope (the kernel's confinement boundaries) and what is not (a demonstrator running under QEMU is not a production system), and a code-scanning workflow. **Settings:** private vulnerability reporting, and a ruleset requiring pull requests into `main`. Note the plumbing for the last one already exists, since CI runs on `pull_request`; what is missing is the branch protection that makes it mandatory. One thing to check rather than assume: **CodeQL's Rust support** has been moving through preview, so confirm its current state before committing to it; if it is not ready, the practical scanners are the clippy gate we already run, `cargo-audit`/`cargo-deny` from milestone 42, and a SARIF upload from whatever does work | **a public repository with a security thesis should be able to receive a security report privately**, which today it cannot. The pull-request item also changes how this project is built: work currently lands by merging feature branches into `main` locally, and requiring PRs would put every merge behind the same gate rather than trusting the person merging, which is the discipline that caught the reap flake and the conflict markers only because I happened to run the gates by hand |
| 45 | IN-PROGRESS | Triage the CodeQL code-scanning alerts, and decide what the tool is for | Nine alerts on first run. Seven (`actions/missing-workflow-permissions`) were fixed immediately by giving every workflow an explicit least-privilege `permissions: contents: read`, which is the right call for this repo specifically: a project whose thesis is that a component holds the authority its job needs and nothing more has no business letting its CI token default to write access it never uses. **The two that remain are high severity and need judgement, not configuration**: `rust/access-invalid-pointer` at `crates/intrusive/src/lib.rs:93` and `:109`, the raw-pointer dereferences in the intrusive wait-queue's `push_back` and `pop_front`. Both already carry `SAFETY` comments citing the queue's caller contract, and `intrusive` is one of the 13 Kani-proved crates, so the question is precisely what CodeQL sees that Kani does not: Kani proves the pure logic under chosen bounds, while the pointer validity here rests on a *caller* contract enforced by convention rather than by the type system. Decide per alert whether it is a true positive worth restructuring for, or a false positive to dismiss **with a written reason**; then set the standing policy for how alerts get triaged, since an alert list nobody dispositions decays into wallpaper | **the alerts land exactly where this project's most-used unsafe abstraction lives**, so the answer is worth having either way: either the wait queue's contract can be made structural rather than documented, which is a real improvement to the code every blocked thread passes through, or we write down why it cannot be and what upholds it instead. Also forces the meta-decision milestone 44 left open, now that scanning is actually running: a scanner whose findings are never dispositioned is worse than none, because it manufactures the appearance of review |

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

### 16. Real hardware + IOMMU-backed driver isolation (recast 2026-07-27: RISC-V first)

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
   hand-written in `crates/measure`, one implementation shared by the build and the kernel. Fails
   closed both ways: wrong bytes halt, and an *unmeasured* program halts too (an empty trust root
   vouches for nothing). Both ISAs. The **signature** variant (update init without rebuilding the
   kernel, at the cost of Ed25519 in the TCB and a key-custody question) is recorded in DECISIONS §26's
   phase B block as a follow-up, not built. See notes/trusted-init.md.
2. **Shrink the blast radius. (Phase B.2, BUILT 2026-07-29; the interactive boot's migration is the
   remaining increment.)** Reduce what a compromised init can do: hand most process-construction to
   smaller, less-privileged sub-servers, so init's own authority is minimal and short-lived (build the
   first servers, then drop the untyped). The less init holds, the less a broken init costs. Built as a
   four-program tree (`rootsup`, `spawner`, `subsup`, `flaky`): the spawner holds one program image and
   a `WRITE`-only budget (not the archive, so it can build exactly one program), the supervisor holds
   no memory at all and can only *ask*, and the root deletes its untyped once both are running. Proven
   on both ISAs by authority rather than timing: after the handoff, retyping a page or a kernel object
   from init fails with `NoSuchSlot`, and a faulting sub-server is reaped and restarted by its own
   supervisor. `sysinit` and `hello`'s init role still hold their budgets for life (they remain the
   shell's spawn service); migrating that hand-validated boot path is the next increment. Two design
   forks found and reported rather than built through (a reap-only right, and turning a tid into a
   handle). See DECISIONS §26's phase B.2 block and notes/trusted-init.md.

   **Both of those forks are now closed (DECISIONS §32, BUILT 2026-07-29, both ISAs).** Reaping moved
   off `Untyped::DESTROY`, which needs `WRITE` on the region and therefore the same right that *builds*
   a process from it, onto `Endpoint::REAP` on the supervision endpoint. Authorization needed no new
   bookkeeping: §26 already records `Thread::fault_ep` and the kernel already stamps the tid, so the
   check is that the named thread's recorded endpoint *is* the one being invoked. The tid-to-handle fork
   is closed for this case by the same move, because the tid is authorized relative to the endpoint it
   arrived on rather than being a global handle. The measured payoff: **`subsup` now holds nothing but
   endpoints**, since the phase B.2 proxy that had to ask `spawner` to reap is no longer needed. The
   measured limit: milestone 36's `cwarden` still holds a construction budget because it is *also* the
   builder, which shows the bundling was two things and only one of them was the reap. `REAP` refuses a
   live thread on purpose, so a **hung** child still cannot be restarted; that is the watchdog case and
   it belongs to 23. Two Kani harnesses in `crates/caps` cover the authorization invariant. See
   notes/supervision.md.
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

**Status (2026-07-29), with milestone 35 done.** The proved set is now broad: 13 crates, ~60
harnesses, covering `caps`, `ipc` (rendezvous, one-shot reply, the collected-sender path), the MMU
codec on *both* formats (`paging`: VMSAv8-64 and Sv39, level-walk and leaf permission separation),
generational names (`slots`: a removed name never resolves again), frame allocation, region
split/destroy arithmetic, ELF parsing, the device-tree reader, ASID allocation, PCI decode, and now
the DMA-confinement validator (`dma_validate`) and the IOMMU domain's page set (`paging::domain`), both
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
   host-testable crate (the way `caps`, `ipc`, and `paging` were carved out), then prove the core
   property: no validated descriptor chain can reference memory outside the driver's granted
   region. Cover **both directions** (TX device-reads and RX device-writes-into-driver-memory,
   the milestone 30 addition), **indirect descriptors** (the escape the attacker suite already
   probes), and **multi-queue** (per-queue block isolation, also milestone 30). The kernel keeps
   calling the proved logic; the extraction must not change behaviour, held against the green
   attacker suite.
2. **The `Untyped::SPLIT` rights harness.** SPLIT mints a child budget at `untyped_cap_rights`, a
   fresh-mint site *outside* `caps::derive`, so the existing "derive never widens rights" proof
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

1. **The validator** is `crates/dma_validate`, host-testable pure logic the kernel's
   `validate_and_shadow` calls; **seven** Kani harnesses prove no descriptor the walk shadows escapes
   the granted region or is indirect, covering both directions (symbolic flags include the RX
   device-writable bit), indirect descriptors, chains including cycles, **ring-index wraparound through
   `u16` and outer-loop termination**, overflowing address arithmetic, multi-queue block isolation, the
   oversized-batch bound, and the mutated-after-validation (TOCTOU) case. The QEMU attacker suite
   (DMA-escape and indirect-escape, both ISAs, both transports) is unchanged and green, so the
   extraction is faithful. The ring layout constants moved *into* the crate with the kernel aliasing
   them, because a proof about a copy of the layout proves nothing about the layout that runs.
2. **`split_never_widens_rights`** in `crates/caps` proves the `Untyped::SPLIT` mint (routed through
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
it confines. Likewise the userspace-only crates (`uheap`, `capsh`, `linedisc`) and scheduler
placement policy stay host-tested; a bad placement is a performance bug, not a safety hole.

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

### 27. Rust `std` on the native ABI

**Built 2026-07-28, both ISAs green; phase two complete 2026-07-29.** std's platform layer runs
directly on the capability ABI (Hermit's shape); a real std program (`Vec`, `String`, `HashMap`,
`println!`, `Instant`) is spawned and checked byte for byte on aarch64 and riscv64. Phase two bound
**`std::net`** to netd's socket contract and **`std::fs`** to the §27 FS service, so the same binary
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
feeds 23 directly. Effort L. Off the thesis path, like 20 was: a reach the demonstrator earns.

### 28. A solid terminal: the line discipline as a component

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
counter-design. Effort M.

### 29. A display terminal: framebuffer, virtio-gpu, and a foreign component

**Increment one built (2026-07-29, both ISAs): the first pixels, and the framebuffer seam.** A
confined userspace virtio-gpu driver (`gpud`) drives the control queue through the proved validator
over the §18 PCIe transport on both `virt` boards, behind the IOMMU; a *separate* client (`painter`)
holds only an endpoint and a shared surface and draws a coordinate-derived pattern into it. Two
witnesses in two address spaces digest the result against a value the kernel computes itself, so the
**framebuffer** is proven byte for byte; and the **scanout** is proven too, from the host, by driving
QEMU's monitor beside the suite and comparing a `screendump` PPM against the same pattern definition
pixel for pixel (both ISAs, with a negative control on the checker). The memory decision
generalized to a rule (a framebuffer is a bigger grant, never an exemption) and the GPU's own
confinement hazard (backing addresses ride in a command payload the transport validator cannot see, so
the IOMMU is the barrier) is proved by an attacker test. DECISIONS §29,
notes/framebuffer-contract.md. **Still to come in this milestone:** font rendering, the VT state
engine, scrollback, and virtio-input, all of which arrive as clients of the contract rung one drew;
the VT engine's language remains an open question.

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
a reach in the 24 spirit. Effort L.

### 30. The network stack as a confined component

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
seL4's netstack componentization, Fuchsia's Netstack3 (Rust, capability-routed, the closest
cousin), and Plan 9's /net as the counter-design (per-connection filesystem, everything a
file). Testing is cheap: QEMU's user-mode networking NATs the guest with zero host setup.

**Sequencing.** After the PCIe transport (done); the multi-queue confinement is the
prerequisite piece and worth building first as its own tested step. Feeds 23 and 27. Effort L.

### 31. A capability shell: designation is authorization

**Phase 1 built (both ISAs).** The command line is a grant expression: `capsh` (a host-tested crate)
parses it and checks it against a per-program manifest; the shell holds its own untyped budget and
delegates from it. `run --mem N budgeter` splits N pages off the shell's budget and delegates the
untyped to init, which endows the child; the budgeter maps them and reports the count (15 of 16, the
rest paid for page tables), proving the grant is real, not parsed-and-ignored. Manifest mismatches
and a `file:PATH` designator ("you hold no such capability") are refused at the prompt; `caps` and
`caps run ...` print a process's whole endowment. One kernel change: `Untyped::SPLIT` grants the
child `GRANT` so an untyped is delegable (DECISIONS §16 amendment), which the headline feature
required and no other object type lacked. Per-file grants wait on milestone 32's FS server, and the
`file:PATH` grammar is designed so they slot in with no change. Notes: grant-expression.md,
program-manifest.md.

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
layer); sits behind 28's terminal contract. Effort M.

### 32. A real filesystem: RedoxFS behind a capability FS server

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
hand off across a live swap, the hardest handoff case yet named), 27 (`std::fs`). Effort L.

### 36. A foreign-language component, seam first (spike; feeds 29 and 23)

**DONE 2026-07-29**, both ISAs, in QEMU. DECISIONS §31; concept note notes/c-seam.md.

All four deliverables landed as specified, and the two that produced findings are worth reading before
the next foreign component:

1. **Toolchain.** `user/build.rs` compiles `user/c/cseam.c` with a clang resolved from a candidate list
   and *capability-checked* (`-print-targets` must list both aarch64 and riscv64), object handed to the
   linker for the `cshim` binary only. One compiler for both ISAs is §19 applied to the toolchain, which
   means **Apple's clang is rejected on purpose** (no RISC-V backend) even though it would compile the
   aarch64 half. `script/bootstrap` grew `brew install llvm` / `apt-get install clang`, and the CI
   clippy job grew the same, since it clippies `user`.
2. **Linkage.** `cshim` (Rust) holds every capability and makes every syscall; the C gets `(u8*, usize)`
   and returns a scalar. The syscall surface did not change, and could not have: the C cannot name a
   capability slot.
3. **libc.** The object demands five symbols; the linker demands **two** (`malloc`, `free`), because
   `compiler_builtins` already supplies `memcpy`/`memset`/`strlen` weakly for bare targets. **Do not
   shim the other three:** the obvious Rust `memcpy` is `copy_nonoverlapping`, which lowers to a call to
   `memcpy`, so it calls itself, and the symptom is a store fault at `sp` that reads like a stack-size
   problem at any stack size. `malloc` is milestone 27's untyped heap on the instance's own region, so
   one `DESTROY` reclaims it.
4. **The test.** `c_seam_tests`, both ISAs: two out-of-bounds writes (one byte past into a read-only
   page that is the *same physical frame* the warden holds read/write; one page past into an address the
   component has no mapping for and the warden does), both fault at exactly the address the C computed,
   both leave a position-derived witness pattern intact byte for byte, and the third instance does real
   C work whose output is checked against an independent Rust computation. The control that makes it
   mean anything: each bug stores *inside* its grant first, and that store must be visible.

**The fork this fed, stated concretely.** The warden is builder, supervisor, and checker in one process,
because reaping needs `WRITE` on the region and `WRITE` is also what builds one. **A supervisor needs
exactly `DESTROY` on one region it did not create**, and nothing narrower exists. Milestone 22 phase
B.2's IPC proxy is the workaround that exists today; this spike deliberately did not use it, so the
requirement is visible in one program instead of hidden behind a hop.

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
   untyped-backed `GlobalAlloc` (`crates/uheap` plus `user_rt::heap`).
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
component at, and before committing to libghostty-vt. Effort S to M. The whole value is that it is
cheap and it fails early: if the toolchain, the shim, or the confinement story has a problem, we
find it with a throwaway component rather than half way into a port.

### 41. Dead code: triage the suppressions, and un-blindfold the gate

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

- `kernel/src/cpu.rs:243` — "used by the tests now, and by the scheduler in step 3". Step 3 shipped as §28.
- `kernel/src/smp.rs:64` — "used by the SMP tests now, and by spawn's placement policy when it...". Also §28.
- `kernel/src/cap.rs:130` — "first used by the virtio driver setup in 9b".
- `kernel/src/arch/aarch64/interrupts.rs:63` — "milestone 5's first non-test caller".
- `kernel/src/arch/aarch64/mmu.rs:647` and `:660` — both point at milestone 8's *in-kernel* console, which §21 moved into userspace and retired.

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
it when no other lane is open, or accept the rebases. Effort M, mostly reading.

### 40. Documentation as a system service: searchable, rendered, and installed by packages

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
2. **There is no font rendering yet.** Milestone 29 shipped pixels, and glyphs plus the VT engine are
   its remaining increment. So a *graphical* documentation browser cannot be first; the terminal can.

#### Reuse: take the parser, write the renderer

CommonMark is a fiddly specification with a large conformance suite, and parsing it is exactly the
kind of work worth taking from someone else. Rendering to *our* terminal contract is ours and small.
That split is the reuse judgment, and it is the same one milestone 32 made about RedoxFS.

| Piece | Option | Judgment |
|---|---|---|
| Parse | **`pulldown-cmark`** (pure Rust, CommonMark, event-stream API, few dependencies) | **Take it.** The event stream is the right shape for a renderer that emits ANSI. Milestone 27's `std` is what makes this buildable at all. |
| Parse | `comrak` (GFM: tables, strikethrough, footnotes) | Consider later if GFM tables matter; more dependencies. |
| Render | `termimad`, `mdcat` | **Do not take.** Both sit on `crossterm`, which assumes a POSIX terminal (termios, ioctl). Porting that is more work than emitting ANSI against `linedisc`'s contract, which we own and already speak (§21). |
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

- **Phase 1, the terminal viewer.** `pulldown-cmark` to an ANSI renderer over `termd`'s contract:
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
later. Effort S to M for phase 1, M for phases 2 and 3 together.

### 39. Repository structure for a loosely-coupled OS, and the road to a distribution

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
  also a library: `virtio`, `vnet`, `netproto`, `suptree` and `cseam` are shared modules sitting
  beside the programs that consume them. So no component can express "I need the virtio driver bits
  but not the network stack", every component rebuilds when any shared module changes, and no
  component can take a dependency without handing it to all 28.
- **One component has already escaped, for real reasons.** `fs-server/` is its own workspace with its
  own `Cargo.lock`, because RedoxFS's default features pull `fuser` (whose build script panics on
  macOS) and its core wants `std` under test. Milestone 36 did the same to the toolchain by requiring
  a cross-capable clang. Two instances is a pattern: the first components with genuine dependency
  needs of their own had to leave.
- **`crates/` conflates three audiences with different rules**, so the boundary a third party would
  care about is invisible: kernel proof crates (`caps`, `paging`, `frames`, `regions`, `slots`,
  `asid`, `intrusive`, `dtb`, `elf`, `dma_validate`, `measure`, `uheap`, Kani-proved and nobody
  else's business), wire contracts (`fs_proto`, `gfx_proto`, `linedisc`, `compose`, `abi`, the
  **only** things an external component needs), and userspace runtime (`user_rt`, `capsh`,
  `crickerfs`, `pci`).
- **Every crate is `version = "0.1.0"`.** Correct for internal crates, fatal for a published
  contract, and contracts are exactly what milestone 23's live replacement makes into a compatibility
  surface.
- **Not everything in `user/` is a service.** `heeder`, `spinner`, `flaky`, `allocdemo`, `worker`,
  `builder`, `coremark`, `elbench` are fixtures and benchmarks. Mixing them with `netd`, `gpud`,
  `compd` and `termd` is much of why the directory reads as shapeless.

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

"Server" stays a fine role word inside a component (`fsserver` serves the FS service). "Daemon" gets
dropped.

#### The four options

| | Shape | Buys | Costs |
|---|---|---|---|
| **A** | One workspace, restructured directories (`kernel/`, `components/`, `contracts/`, `runtime/`, `fixtures/`, `tools/`) | Legibility, cheapest | Does not fix per-component dependencies unless each component also becomes its own crate, which is the actual work |
| **B** | One repo, multiple workspaces (generalize what `fs-server/` already does, driven by `xtask --manifest-path`) | Real dependency isolation; a component can use `std` or a foreign toolchain without infecting the kernel build | More lock files, slower cold builds, more complex xtask |
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

#### The cheap first move, which commits to none of the four

**Split `user/` three ways**: `components/` for the services, `fixtures/` for the test programs, and
lift `virtio`, `vnet`, `netproto`, `suptree` into `runtime/` crates. That ends the
crate-is-both-a-program-collection-and-a-library problem, makes dependencies expressible, and leaves
the gate untouched.

**Whichever option is chosen, do the move as one mechanical commit with the pairing audited.**
Renaming directories touches `xtask`'s `--bin` lists and the initrd packing, and a union merge in
exactly that code dropped a `--bin` flag on 2026-07-29 and duplicated a loop header the same day. It
must not be folded into feature work.

## The display ladder (recorded 2026-07-28, Chris's direction)

The stated destination: eventually, something like COSMIC driving a GPU for display. That
decomposes into rungs, each independently a demo, and the decomposition is what makes the ambition
honest. COSMIC's shape is Rust clients rendering into shared buffers, a compositor compositing
them to scanout, everything message-passing; cricker-os already has shared frames and endpoints,
so the *architecture* is aligned even where the drivers are mountains.

**Status (2026-07-29): rungs one and two are built.** Rung one shipped as specified minus the VT
engine (which it deliberately deferred as a *client* of its contract), rung two shipped whole, both on
both ISAs, both with the pixels verified from the host as well as the guest. Rung three is the next
step and is where the parked competitor question below has to be answered on purpose.

1. **Rung one: milestone 29** (promoted from optional). **Built**: a confined userspace virtio-gpu
   driver (`gpud`), a client that draws (`painter`), and the framebuffer contract between them
   (`crates/gfx_proto`, notes/framebuffer-contract.md, DECISIONS §29). The framebuffer is a bigger
   grant and never an exemption; the pixels are proved in the guest by two witnesses in two address
   spaces and from the host by comparing QEMU's `screendump` against the pattern definition. Font
   rendering and the VT state engine were deferred on purpose: the contract carries pixels, not text,
   so a terminal arrives above it as another client (and the VT engine's language, libghostty-vt in
   Zig or `vte` in Rust, stays an open choice).
2. **Rung two: a compositor component (milestone 33). Built**, both ISAs: `compd` multiplexing one
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

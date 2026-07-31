# Session handoff (2026-07-29)

A restart point, written so a fresh session resumes without re-deriving state. Delete or
overwrite once its contents are stale; this is a working note, not a permanent record. The
permanent records are DECISIONS.md, design/roadmap.md, and the notes/ they point to.

## Why this exists

The session that wrote this ran a large parallel push (agents in worktrees, one milestone
each, merged to main per proven piece). It is being restarted to pick up **Opus 5** for the
agent fleet: Opus 5 shipped 2026-07-24, after that session launched, so its Agent-tool model
map was stale and its `opus` alias resolved to Opus 4.8. A fresh session should have Opus 5
available. Re-point new agents at it.

## What landed this push (all on main, green both ISAs)

- **Milestone 32 (RedoxFS FS service):** capability-shaped contract, open-by-path only inside
  the server against a granted directory cap. DECISIONS §27, notes/fs-server.md. Read path
  proven; the interrupt-driven completion path is fixed (see IRQ below). **The write path was
  confirmed end to end on 2026-07-29** through `std::fs`, so the old allocator-commit blocker is
  retired; see item 3 below.
- **§28 SMP placement:** two-choice spawn placement, message-shaped idle stealing, wake split
  (IPC rendezvous local, device-IRQ load-aware). DECISIONS §28 + implementation amendment,
  notes/scheduler.md. Exposed and fixed a **latent RISC-V switch bug** (stale `tp` restored
  from another hart's frame on S-mode trap-return; aarch64 immune because its per-CPU pointer
  is a system register). Added the **per-progress watchdog heartbeat** (any non-idle thread =
  progress; catches lost-wakeup deadlock, not busy livelock which the leaked-spinner test and
  the qemu-bounded ceiling cover; ratified 2026-07-29).
- **Interrupt delivery:** the block server now WAITs on completion instead of polling (the
  "WAIT hangs" belief was a misdiagnosis); GIC + PLIC device-line affinity, both ISAs.
- **netstack smoltcp-timer fix:** poll retransmit/ACK timers instead of blocking only on the NIC
  IRQ; closed the RISC-V std_net mutual-idle deadlock SMP timing exposed. std_net completes
  both ISAs (~300s aarch64, under the heartbeat on riscv).
- **Milestone 35 (prove the DMA boundary):** crates/dma_validate proves, for every input, that
  no descriptor chain escapes the driver's grant (TX/RX, indirect, multi-queue, TOCTOU). Plus
  the `Untyped::SPLIT` never-widens harness (authority-never-widens now proved at every mint
  site). IOMMU maps-exactly-the-grant recorded as a Verus target, not forced under Kani.
- **Benchmark follow-up:** `relay_rtt` (the confined-intermediary tax, ~2x a bare IPC RTT,
  gated both ISAs); `smp_throughput` (compute ~3.5x on 4 cores, the §28 win; IPC pipelines
  anti-scale under HVF, a documented virtualization artifact); HVF `--real` refreshed and made
  per-core by default. notes/benchmarks.md.

## Decision trail (this session)

DECISIONS §24 (two-tier `^C`, shell-held; `Tcb::SUSPEND` deferred with triggers), §25 (socket
ids now, minted endpoints tracked), §26 (fault endpoint, five sub-decisions), §27 (FS
service), §28 (SMP placement) + amendment, §16 amendment (SPLIT rights inheritance). Milestone
35 added. Read those sections for the reasoning; each records the fork and why.

## Wave-3: what's next, roughly in order

1. **`^C` implementation** (§24 decided, not built): two-tier, shell-held. lineedit detects `^C`,
   shell holds the interrupt endpoint, cooperative then forcible (DESTROY force-kill). Ready to
   schedule; needs no new kernel surface.
2. ~~**Milestone 22 phase B** (trusted init).~~ **DONE 2026-07-29** (DECISIONS §26's phase B.1 and
   B.2 blocks, notes/trusted-init.md). B.1: the build hashes the boot program and the kernel refuses
   to enter anything else (SHA-256 in `crates/measure`, digest compiled into the kernel image, fails
   closed on a *missing* measurement too). B.2: a four-program tree where construction moves to a
   sub-server holding one program image, the supervisor holds no memory at all, and init deletes its
   budget; proven by authority on both ISAs. **Leftovers:** the interactive boot's init (`sysinit`,
   `hello`'s init role) still holds its budget for life because it is the shell's spawn service, and
   migrating that hand-validated path is the next increment; two design forks are recorded for Chris
   (a reap-only right so a root supervisor can recover without regaining construction authority, and
   turning a fault message's tid into a handle a builder holds). This work also found and fixed a
   real pre-existing race in the exception-return path on both ISAs (notes/exceptions.md).
3. ~~**Milestone 27 phase 2 completion:** std::fs binding to the FS server.~~ **DONE 2026-07-29**
   (DECISIONS §22 phase-two amendment, notes/std.md): `std::fs` binds to the §27 contract through a
   directory capability at slot 4, escapes refused as un-nameable, `Unsupported` without the grant.
   It also settled the question this note left open: the on-device write path **works**, and the
   recorded "loops in RedoxFS's allocator commit" blocker was stale (§27 amended, host-tool reopen of
   the image now in the gate). What is left there is a contract gap, no `CREATE`/`TRUNCATE` verb, so
   `std::fs::write` stays Unsupported; that is a decision for Chris, not a bug.
4. **Milestone 31 phase 2:** per-file grants pointing at FS-server directory caps.
5. **Milestone 23** (the flagship): capability-routed component OS with live replacement. All
   prerequisites now exist (revocation, supervision, dedicated binaries, components with real
   state: netstack under open connections, the FS server). Console hot-swap is instance one.
6. **The display ladder** (roadmap "The display ladder"): 29 (VT terminal over virtio-gpu) ->
   33 (compositor component) -> apps on the std PAL -> 34 (virtio-gpu 3D). Chris's stated
   destination is "something like COSMIC driving a GPU." Rung 5 (bare-metal BXE 3D) is struck.

## Standing defaults (nod-or-veto; currently in force)

- **SMMU/IOMMU fronts the TCG test machine only, not the HVF benchmark path.** Correctness is
  proven under TCG; SMMU-alongside-HVF is fragile. Under HVF, PCIe DMA runs unconfined.
- **VisionFive 2 board purchase** is deferred; reminder armed for when milestone 23 wraps (it
  unblocks 16a first silicon + the milestone-25 sel4bench real-PMU leftover). Spec:
  v1.3B, 8GB, with a UART module. The board has **no IOMMU**, so milestone 35's software
  validator is the *sole* DMA confinement on first silicon, sequence 35 with/before 16a.
- Optional/parked: 17 (multikernel), 20 x86_64, 24 (Virtualization.framework). Chris declined
  x86_64 as background work while tokens were the constraint; the 5x budget bump later relaxed
  that, revisit if desired.

## Atom OS (peer project) note

fpedrolucas95/Atom is a capability microkernel in the same space, further along on visible
breadth (browser/JS/TLS, compositor, audio), behind on assurance (partial capability
enforcement, CI security gate removed, cert-less TLS). Apache-2.0. Its userspace is not
reusable for us (ABI-locked / POSIX-shaped / upstream-vendored); the one clean lesson is its
compositor windowing protocol as prior art for milestone 33. A design-fork response to its
three questions was drafted (scratchpad) if Chris chooses to engage.

## Mechanics for the fresh session

- Agents run in git worktrees under `.claude/worktrees/` (gitignored). Durable work is on
  origin; in-flight worktree commits are not, so push branches before any restart.
- Merge discipline: merge per proven piece, run the gates yourself before pushing main
  (`script/test` both ISAs, `script/verify` if a proof-crate changed, `script/lint`,
  `script/fmt`, `script/bench --check` [+ `--riscv --check`]). Kani/`verify` is the slow gate;
  skip it only when no proof-relevant crate changed.
- Lane discipline when running concurrent kernel agents: keep IRQ-routing, context-switch, and
  verification-crate work in disjoint files; tell each agent which files the others own.

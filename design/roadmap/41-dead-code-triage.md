# 41. Dead code: triage the suppressions, and un-blindfold the gate

**Status: BUILT.**

**In brief.** Triage all **79** `allow(dead_code)`/`allow(unused)` suppressions in the tree, delete what is dead, and replace the module-wide ones with per-item allows that carry a reason. Three distinct classes, only one of which is tidying. (1) **The gate is blindfolded over 5,831 lines**: six files carry module-wide `#![allow(dead_code)]`, including `sched.rs` (3,166 lines) and `arch/aarch64/mmu.rs` (1,275), so `-D warnings` cannot see dead code in the two largest and most security-relevant files in the kernel. (2) **Suppressions whose own comments name milestones that have since shipped**, e.g. `cpu.rs`'s "by the scheduler in step 3" and `smp.rs`'s "by spawn's placement policy" (both landed as §28), `cap.rs`'s "in 9b", `interrupts.rs`'s "milestone 5's first non-test caller", and two in `mmu.rs` pointing at milestone 8's in-kernel console, which §21 moved to userspace. Each is either now-used (delete the attribute) or genuinely dead (delete the code); either way the comment is false. (3) **Superseded demo payloads** in `user.rs`, which say so themselves ("7c handed the demo over to the real ELF"). Ends with a lint gate refusing new module-wide suppressions, the same shape as the conflict-marker and roadmap checks

**Why it matters.** **a `-D warnings` gate with holes in a third of the kernel is a gate that reports success it has not earned**, which is the same class of problem as the four-times-corrected §27 record and the contradicted `fs_read` comment: the tooling said fine while nobody was looking. It also protects a real asset, since this codebase's unusually heavy commenting is only valuable while the comments are true, and a suppression citing a milestone that shipped weeks ago actively misleads. **Explicitly NOT in scope:** hardware register definitions (`gic.rs`, `timer.rs`, `semihosting.rs`, `mmu.rs` field encodings) where a complete definition is the point, and deliberate diagnostics (`VERIFY_WRITES`, `second_mount`) that encode measurements which killed hypotheses. Those keep their allows and gain a stated reason, which is the difference between a suppression and a decision

## Built 2026-07-30. The rule is DECISIONS §38, and the ratchet is in `script/lint`.

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

## Deliverable

Triage all 79 suppressions; delete what is dead; convert the module-wide ones into per-item allows that
each carry a reason; and finish with a `script/lint` check that refuses a new module-wide
`#![allow(dead_code)]`, the same shape as the conflict-marker and roadmap-status gates. The point of
that last step is that this is a ratchet: without it the file-level suppression comes back the first
time someone finds it inconvenient.

## Explicitly not in scope

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

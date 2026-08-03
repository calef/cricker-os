# Auditing the hand-written architecture assembly

Done 2026-07-29, prompted by the exception-return race that milestone 22 phase B.2 turned up (the
full account of that bug is in [exceptions.md](exceptions.md); the fix is commit `9368657`). That
bug was a privilege escalation reachable from a normal boot, it failed about one suite run in four,
and it had been sitting in `RESTORE_CONTEXT` since milestone 7. So the question this note answers is
not "was that bug fixed" but **"how many more of its siblings are there?"**

## Why this is the code that needs auditing by hand

The Kani proofs cover the pure-logic crates: `paging`, `frames`, `slab`, `elf`, `capability`, the
allocators, the ring validators. That is deliberate and it is most of the interesting logic. But it
leaves the hand-written architecture assembly as **the least-verified code in the trusted computing
base**, and milestone 20 (design/roadmap/20-portable-hal.md) says so out loud. There is no tool in this project that
can prove `vectors.s` or `trap.s`.

That matters more than the line count suggests, because the assembly is where the invariants are
*hardware* invariants: single-copy registers, masking state, translation regimes. A Rust type error
is caught; a two-instruction window in which `SPSR_EL1` holds one thing and `ELR_EL1` holds another
is caught by nobody. The whole arch tree is about 6,200 lines. An audit by reading is affordable, and
in the absence of a prover it is the compensating control. This note is that audit, so the reasoning
survives and the next person does not have to redo it from scratch.

## The bug class, stated generally

> Kernel code that stages state in single-copy hardware registers, or in any per-CPU location that
> the exception path itself clobbers, across more than one instruction, while an exception,
> interrupt, or preemption can land in the middle.

The archetype, and what made the original bug rare enough to hide: **a path that enters a
save/restore sequence from ordinary kernel code with interrupts enabled**, rather than from a trap
that had already masked them. On a normal trap return the window is closed for free, because taking
the exception set `PSTATE.DAIF` (or cleared `sstatus.SIE`). It is the *first-entry* paths,
`enter_userspace` and `user_return`, that branch into the restore with interrupts live.

Which is why the interesting question for every candidate below is (d): is it reachable, or is it a
window that exists in the instruction stream and that nothing can ever land in?

## What was audited

Everything under `kernel/src/arch/`, read in full, both ISAs:

| File | aarch64 | riscv64 |
|---|---|---|
| Exception/trap vector and restore | `vectors.s` | `trap.s` |
| Trap frame, dispatch, fault handling | `exceptions.rs` | `exceptions.rs` |
| Context switch and the two trampolines | `context.s`, `context.rs` | `context.s`, `context.rs` |
| Boot and secondary bring-up | `boot.s`, `image_header.s` | `boot.s` |
| MMU, TTBR/SATP, ASID, TLB | `mmu.rs` | `mmu.rs` |
| Interrupt masking primitives | `interrupts.rs` | `interrupts.rs` |
| Interrupt-controller adapter | `irq.rs` | `irq.rs` |
| Timer | `timer.rs` | `timer.rs` |
| Per-CPU pointer, SMP calls, barriers | `mod.rs` | `mod.rs` |
| IOMMU / DMA domains | `iommu.rs` | `iommu.rs` |
| Host exit | `semihosting.rs` | `semihosting.rs` |

Plus the code on the other side of the seam that the class reaches into: `drivers/gic.rs`,
`drivers/plic.rs`, the switch path in `sched.rs` (`schedule`, `finish_switch`), `cpu.rs`'s per-CPU
plumbing, `smp.rs`'s `secondary_main`, and `user.rs`'s trap-frame placement (read only; milestone 29
owns that file).

Two structural facts fell out of the sweep and are worth recording because they bound the whole
search:

- **There is exactly one `eret` in the kernel and exactly one `sret`**, both inside the restore
  sequence the fix hardened. There is no second way out of the kernel to audit.
- **There are exactly two places that fabricate a trap frame**, `TrapFrame::for_user_entry` on each
  ISA. Every other frame is written by the trap entry itself.

## Findings

### 1. The RISC-V interrupt mask in `trap_return` is not self-sufficient (safe today, hardened)

**(a) The window.** `trap_return` masks interrupts with `csrci sstatus, 2`, then stages `sepc`, then
executes `csrw sstatus, t0` with the frame's saved status word, then ~30 loads, then `sret`.

**(b) What can land in it.** On RISC-V, `sstatus` is one register holding *both* the staged
return fields (`SPP`, `SPIE`) and the **live** interrupt-enable bit (`SIE`). So `csrw sstatus, t0`
writes the frame's `SIE` into the live bit. If a frame carried `SIE = 1`, interrupts would be back on
for the ~32 instructions before the `sret`, with `sepc` already staged, and any interrupt in there
would clobber `sepc` and `sstatus` exactly as in the original bug. The `csrci` would have bought
nothing.

**(c) The corrupted state.** Identical to the original: the nested handler's own `trap_return`
restores the S-mode values, our `sret` then runs with `sepc` pointing into the middle of
`trap_return` and `SPP = 1`, so it returns to S-mode at that address and spins there. A U-mode thread
never enters U-mode at all.

**(d) Reachable? No, and that is the finding.** It is closed by an invariant, not by the
instruction: every trap frame carries `SIE = 0`. Real traps get it from the hardware, which clears
`SIE` on trap entry (moving it to `SPIE`) before `trap_entry` reads the register. The one fabricated
frame, `TrapFrame::for_user_entry`, composes `SPIE | UXL_64` and happens not to set `SIE`.

So the code is correct. What was wrong was the **record**: the `RESTORE_CONTEXT` comment says the
mask lives in the macro "so that any future path reaching here with interrupts on is covered by
construction," and `trap_return`'s comment points at it. That claim is true on aarch64, where the
staged `SPSR_EL1` is a physically different register from the live PSTATE and nothing before the
`eret` can re-enable interrupts. **It is false on RISC-V**, and a comment that invites you to read it
across the two ISAs is worse than no comment, because the next person to add a frame-fabrication
site would trust it.

Fixed by making the invariant checked rather than hoped for: a `const _: () = assert!(...)` next to
`for_user_entry` that fails the build if a fabricated frame ever sets `SIE`, and a comment in
`trap.s` that states the invariant, names its two sources, and says plainly that the aarch64
"by construction" claim does not transfer. Zero instructions added; nothing on the hot path moved.

The general lesson generalizes past this file, and it is the one worth keeping: **when the same bug
is fixed on two architectures, check whether the fix is load-bearing for the same reason on both.**
Here it was a complete fix on one ISA and a partial one on the other, and the symmetry of the two
patches concealed that.

### 2. RISC-V `trap_entry` parks a user-controlled value in `sscratch` for ~50 instructions

**(a) The window.** `trap_entry` opens with `csrrw t0, sscratch, t0`: `t0` becomes `&TrapStash` and
**`sscratch` is left holding the interrupted `t0`**. It stays that way for the whole frame build,
about fifty instructions, until the `csrw sscratch, t0` that restores it.

**(b) What can land in it.** Not an interrupt: the hardware cleared `SIE` on the way in. Only a
*synchronous* fault, and the only faultable instructions in the window are the ~35 stores building
the frame on the kernel stack. So: a kernel-stack overflow, or a bad `stash.kernel_sp`.

**(c) The corrupted state, and this is the part worth knowing.** The nested `trap_entry` executes its
own `csrrw t0, sscratch, t0` and gets **the interrupted `t0` as its `&TrapStash`**. It then does
`sd t1, 16(t0)` and `sd sp, 24(t0)`. If the outer trap came from U-mode, `t0` is a register the user
program chose, so the kernel performs two 8-byte stores to a **user-chosen address**. That is a
memory-safety failure mode, not merely a lost register.

**(d) Reachable?** Only behind a kernel-stack overflow, which is already a fatal kernel bug and which
the guard page is there to catch. A user program cannot choose the kernel's stack depth at the moment
it traps; it would need the kernel to be independently near the end of a stack. So: not reachable
from userspace on its own, and correctly classified as a **double-fault hardening** issue rather than
a live vulnerability. Recorded, not fixed, because the fix is a protocol change rather than a closed
window. Options, if it is ever worth doing:

1. **Park the interrupted `t0` in the stash instead of in `sscratch`.** Add a `scratch2` word and
   restore `sscratch` after four instructions, none of which can fault (they all target the
   always-mapped stash), instead of after fifty. Costs one extra store and one extra load per trap on
   the hottest path in the kernel, so it would move both icount baselines.
2. **A separate per-hart trap stack**, entered when `SPP = 0`, which is what a kernel that wants to
   survive its own stack overflow eventually needs anyway. Larger, and it belongs with whatever
   milestone takes stack-overflow recovery seriously rather than being smuggled in here.
3. **Leave it, and say so**, which is what this note does. aarch64 has the same class of problem in
   the same place (a fault inside `SAVE_CONTEXT` loses `ELR_EL1`/`SPSR_EL1` before they reach the
   frame) and it is unrecoverable there too; every kernel has a double-fault story, and ours is
   currently "report and halt."

### 3. The PLIC's enable bits are a lock-free read-modify-write over shared MMIO (CLOSED)

Not a state-staging bug, but it is the interrupt-controller interleaving the audit went looking for,
and it is a parity gap.

**(a) The window.** `drivers/plic.rs`:

```rust
pub fn enable(source: u32, context: usize)  { ... write(word, read(word) | bit); }
pub fn disable(source: u32, context: usize) { ... write(word, read(word) & !bit); }
```

Two MMIO accesses, no lock. `word` is one 32-bit register carrying the enable bits for **32 sources**
of one PLIC context. These are the only two MMIO read-modify-write sites in the driver and arch tree.

**(b) What can land in it.** Another hart executing `enable` or `disable` for a *different* source in
the same 32-source group on the same context. `arch::irq::enable` is called from kernel-thread
context with interrupts **enabled** (the driver-spawn paths in `user.rs`), while
`plic::disable(source, ctx)` is called from another hart's external-interrupt handler. Nothing
serializes them.

**(c) The corrupted state.** A lost update, either direction. A lost *enable* leaves a device source
masked forever, so its driver blocks on an interrupt that never arrives. A lost *disable* leaves a
level-triggered source enabled after its handler masked it, so the line re-fires the instant
interrupts reopen and that hart drowns in an interrupt storm. Both are liveness failures; neither is
a privilege or memory-safety failure.

**(d) Reachable? Yes.** `target_context` spreads sources round-robin over the online harts, and with
up to nine sources (virtio-mmio 1..8, the UART at 10) over four harts several sources share a context
and therefore share the enable word. The window is two MMIO accesses wide and the enables happen a
handful of times per boot, which is consistent with never having observed it.

**Why aarch64 does not have this.** The GIC's `ISENABLER` / `ICENABLER` are write-1-to-set and
write-1-to-clear, so enabling one line is a single store that cannot disturb its neighbours: the
architecture gives you atomicity for free. And `drivers/gic.rs` takes a lock on top of that anyway.
The PLIC has plain read/write enable bits, which forces the read-modify-write, and the RISC-V driver
never grew the lock the GIC has. Under rule 5 that asymmetry is the bug, independent of how likely it
is to fire.

#### Closed

Reported first, then fixed on review, as execution inside decided architecture rather than a design
fork: §9 already establishes `IrqSafeMutex` plus a rank order, `drivers/gic.rs` already takes a lock
for this exact operation, and 16b added `rank::IOMMU` without escalating, so a rank is precedented.

`enable` and `disable` now share one helper that holds an `IrqSafeMutex` across the read-modify-write.
Three things were decided along the way and are worth keeping:

**It must be `IrqSafeMutex`, and that is not belt-and-braces.** `disable` is called *from the
external-interrupt handler* and `enable` from thread context with interrupts on. A plain spinlock
would let a thread take the word on hart H, take an external interrupt on H, and spin in the handler
forever on a lock only the interrupted code can release. One hart, no SMP needed, permanent. This is
§9's opening paragraph, arrived at in a new place.

**The rank is `rank::IRQ_CONTROLLER`, which used to be `rank::GIC`.** Renamed rather than duplicated.
The two drivers are mutually exclusive at *compile* time (`drivers/mod.rs` gates each to its ISA), so
they are not two locks needing an order between them; they are one lock role with two
implementations, and two names at rank 20 would invite the reader to work out a precedence that does
not exist. (INBOX/MAPPINGS/KMEM share rank 59 for the opposite reason: those coexist and are declared
never-nested.) The placement is a leaf with slack in both directions: the body is two MMIO accesses
and no calls, so nothing is taken beneath it, and it is always taken holding nothing, because the
handler holds nothing by §9's record-and-defer rule and every `enable` caller has already dropped
`SCHED` (`bind_irq` and `create_endpoint` take and release it first).

**Scoped to the one register that needs it**, verified register by register rather than assumed. The
per-source priority word is unshared; the threshold write is a whole-word store of the constant `0`,
so it is idempotent rather than an RMW, even though the affinity work made it reachable cross-hart;
and claim/complete is per-context and therefore hart-local, so it stays lock-free. It also does not
share a word with the enable bits (the 0x2000 and 0x20_0000 blocks), so serializing one did not drag
in the other. Both icount baselines are byte-identical, which is the confirmation that nothing hot
was locked.

**On proving it, honestly.** There is no test here that would have caught the original bug, and the
test module says so. The window is two MMIO accesses; widening it to catch the race would mean
shipping instrumentation inside the critical section and then testing the instrumented version, and a
loop of two harts hammering the bits passes with the lock and passes without it. What is pinned
instead is the half a test can reach: that the read-modify-write preserves the neighbours sharing its
word, which is exactly the invariant a lost update violates, and which is the regression *this
change* risked by folding both directions into one helper. It fails on demand (drop the `read` and it
reports "the read-modify-write dropped it"). A second test pins the irqsave/irqrestore behaviour at
the two real call-site shapes, because turning the fix into a hang is the more likely way to get this
wrong later. The serialization itself is attested by the suite it runs inside rather than by an
assertion: every riscv virtio test calls `enable` from thread context and `disable` from the handler,
so a misplaced rank would panic with LOCK ORDER VIOLATION and a non-IRQ-safe lock would hang.

## Candidates cleared, and why each is safe

Recorded because "we looked and it is fine" is the other half of an audit, and because each of these
is a place a future change could break something.

**aarch64 `RESTORE_CONTEXT` after the fix: safe by construction, claim verified.** The only writes
between `msr daifset, #0xf` and the `eret` are to `SPSR_EL1`, `ELR_EL1`, `SP_EL0`, and the general
registers. None of them can alter live PSTATE, so interrupts stay masked for the whole sequence
regardless of what the frame contains. (`msr daifset` needs no `isb`; the masking is architecturally
effective for subsequent instructions, which is why Linux's `local_daif_mask` is the same bare
instruction.) This is the claim finding 1 shows does *not* hold on RISC-V.

**Nothing stages `SPSR_EL1`/`ELR_EL1` or `sepc`/`sstatus` outside the restore macro.** One `eret`,
one `sret`, both inside it. Two frame-fabrication sites, both in Rust, both audited.

**`SP_EL0` in `RESTORE_CONTEXT`.** Written early, alongside `SPSR`. Harmless: the kernel runs at EL1
with `SPSel = 1`, so `SP_EL0` is dead to it, and vector slots 0..3 ("Current EL, SP_EL0") are
unreachable.

**The context switch, both ISAs.** `switch_to` saves only callee-saved registers and swaps `sp`, and
`schedule()` masks interrupts across the whole decision *and* the switch, so there is no window for
the timer to decide twice. The two trampolines deliberately do **not** unmask before
`finish_switch`, which is a previously-found bug with its own account in
[threads.md](threads.md); both files still carry the comment explaining why, and both are still
correct.

**The RISC-V `tp` migration hazard, already fixed this session, verified.** `trap_return` restores
`x4` (`tp`) from the frame **only** for a return to U-mode, and keeps the live value for a return to
S-mode, because a preempted kernel thread can migrate harts and its frame's `tp` names the hart it
left. The conditional reads `t0`, which still holds the frame's `sstatus` from the CSR write above,
and tests `SPP`. Correct. aarch64 cannot have this bug: its per-CPU pointer is `TPIDR_EL1`, a system
register the frame never carries, which is why `percpu_matches_hart` is a constant `true` there.

Sweeping for the same shape more generally, **nothing else round-trips a hart-local value through a
saved frame.** `switch_to` preserves `tp`/`TPIDR_EL1` across a migration by not touching them;
`finish_switch` re-reads `cpu::current()` after the switch rather than using a value cached before
it; `this_s_context()` is computed from `cpu::id()` inside the masked handler and used only there.

**`set_percpu` on RISC-V: a real window, unreachable.** It writes `tp`, then the stash's `percpu`,
then points `sscratch` at the stash. In between, `tp` names the new block while `sscratch` still
names the old one (or is 0 on the very first call, which would make the trap entry store through a
null `&TrapStash`). It is unreachable because of where it is called from: `cpu::init_this_cpu` is the
**first** thing `kernel_main` and `secondary_main` do, before the trap vector is installed
(`arch::init()` is the next step) and long before that hart enables interrupts. Nothing can trap
there. Worth knowing that the precondition is stronger than the one `cpu.rs` documents ("before that
core takes its first lock"): the real requirement is *before that hart can take a trap at all*. CPU
hotplug, or any future re-pointing of a live hart's per-CPU block, would make this reachable and
catastrophic.

**MMU enable and the TTBR/SATP switches.** `boot.s` (both ISAs) and `mmu::install` run on a hart that
has no trap vector yet and no interrupts, so the MMU-enable sequences are uninterruptible by
construction. `set_ttbr0` is a single register write bracketed by `dsb`/`isb`: there is no staging to
interrupt, and an exception landing mid-sequence is itself context-synchronizing while the trailing
barrier still executes on resume. RISC-V's `write_satp` (`csrw satp; sfence.vma`) is likewise safe
even when interrupted, because every process root carries the kernel high half (`share_kernel_half`),
so the handler translates fine, and the `sfence.vma` simply executes when the instruction stream
resumes. `init_secondary`'s `TTBR1_EL1.set_baddr` is a read-modify-write, but of a **per-core**
register on a hart that has not enabled interrupts yet.

**Barriers where a system-register write must take effect before a dependent instruction.** Checked
each one: `VBAR_EL1` + `isb` (present, and `exceptions.md` already explains why); `SCTLR_EL1`
MMU-enable + `isb`; `TCR`/`MAIR`/`TTBR1` + `isb` before the TLB work; `set_ttbr0`'s `dsb`/`isb`;
`satp` + `sfence.vma`; `sync_icache`'s clean/invalidate/barrier sequence on aarch64 and `fence.i` on
RISC-V. Two that legitimately need none: `CNTKCTL_EL1` (its dependent instruction is a userspace
counter read, after an `eret`, which is context-synchronizing) and `csrw stvec` (an ordinary CSR
write, effective in program order).

**`PAR_EL1` in `translate_as_el0`.** A genuine single-copy staging window, `at s1e0r` then `mrs
par_el1`, and it is **already masked**, with a comment naming this exact class. It is also
`#[allow(dead_code)]` with only test callers today. This is the precedent that shows the discipline
predates the bug; the gap was that `RESTORE_CONTEXT` never got the same treatment.

**GIC acknowledge/EOI pairing.** Cannot cross cores: `IAR` and `EOIR` live in the **banked** per-core
CPU interface, so each core acknowledges and completes its own. Within a core the handler runs with
interrupts masked, and the EOI is written before the deferred `schedule()`, which is what keeps the
GIC from refusing equal-or-lower-priority interrupts to the thread we switch to.

**PLIC claim/complete pairing.** Cannot cross harts either, and this was worth checking explicitly
because the path was recently context-parameterized. The handler computes `ctx` once from
`this_s_context()` (`2 * cpu::id() + 1`), and claim, `disable`, and complete all use that same local.
No `schedule()` intervenes: `sched::irq_notify` never blocks or switches, it takes `SCHED`, signals
the endpoint, and at most sends a reschedule IPI to a remote hart. The deferred `schedule()` is at
the bottom of the dispatch, after complete. So the hart that claims is the hart that completes.

**The timers.** aarch64's `CNTV_CVAL_EL0`/`CNTV_CTL_EL0` are per-core, and `rearm`'s
read-read-write runs inside the handler with interrupts masked. `CNTKCTL_EL1`'s read-modify-write is
per-core and boot-only. RISC-V arms through an SBI `ecall`, which traps to **M-mode** and so touches
`mepc`/`mstatus`, never the `sepc`/`sstatus` pair the S-mode return path stages. The same is true of
the other SBI calls (`sbi_send_ipi`, `sbi_remote_sfence_vma`, `psci_cpu_on`): none of them can
disturb S-mode return state, which is worth knowing because an `ecall` in the middle of `trap_return`
would otherwise be exactly the bug.

**Secondary bring-up, both ISAs.** Each hart runs its own prologue on its own stack with interrupts
masked and, on RISC-V, before `stvec` exists. The one genuinely shared structure it touches, the
online mask and count, is published with `Release` after everything else is set up, and the
`ONLINE_MASK` bit is set before the count so a TLB shootdown that runs the instant the hart is
counted also targets it.

**The cross-core inbox.** The one place a core reaches into another core's `PerCpu` is
`cpu::inbox_of`, which is a real `IrqSafeMutex`. Everything else in `PerCpu` (`switched_from`,
`need_resched`, `runq_len`, `rng`) is touched by its owner, with the relaxed atomics serving as
interior mutability through a shared static rather than as cross-core synchronization.

## The honest summary

Three findings from a full read of both ISAs' arch trees, and their dispositions:

| | What | Disposition |
|---|---|---|
| 1 | The RISC-V mask was safe by an unstated invariant, not by construction, and the comment claimed otherwise | **Fixed**, and the invariant is now a compile-time assertion |
| 2 | `trap_entry` leaves a user-controlled value in `sscratch` across the faultable frame stores | **Left documented**, on review: paying hot-path cost to harden behind a kernel-stack overflow that is already fatal is a bad trade |
| 3 | The PLIC's enable-bit read-modify-write is unserialized, a reachable liveness bug and a parity gap | **Fixed** on review, with an `IrqSafeMutex` at the GIC's rank |

Nothing found was a live privilege or memory-safety hole; the two that were are the two already
fixed. Finding 3 is the one worth remembering for its shape rather than its severity: it was not a
missing barrier or a mis-ordered instruction, it was **one ISA quietly getting a guarantee from its
hardware that the other does not**, and the RISC-V driver having been written as if it did. That is
the same shape as finding 1, and both are the shape rule 5 exists to catch.

That is a reassuring result, and it should be read with its limit attached: **an audit by reading
finds what the reader thinks to look for.** The original bug was found by a failure, not by
inspection, and it had survived every previous reading of that file. Until something can prove
assembly, the honest position is that `vectors.s` and `trap.s` are trusted rather than verified, and
notes like this one are how we pay for that.

---

*See also [exceptions.md](exceptions.md) for the bug that prompted this, [threads.md](threads.md) for
the switch-path invariants, [locking.md](locking.md) and DECISIONS §9 for the masking discipline, and
[verification.md](verification.md) for what the proofs do and do not cover.*

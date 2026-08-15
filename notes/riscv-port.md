# Porting nife to RISC-V (the second architecture)

The point of a second port is not to reach RISC-V. It is to **prove rule #1**: that all
architecture-specific code lives under `kernel/src/arch/`, an assumption maintained on faith since
milestone 1. A genuinely different ISA is the first real test of it. RISC-V (rv64, `qemu-system-riscv64
-machine virt`) is chosen over x86_64 because it is clean-different rather than legacy-different: it
exercises the HAL abstraction (a different trap, paging, interrupt, and firmware model) without the
real-mode / GDT / IDT / APIC / UEFI tax that would make most of an x86 port about x86 plumbing, not
about the abstraction. See the reasoning in the roadmap (milestone 20). x86_64 is the third port.

## The arch boundary RISC-V must satisfy

`arch/mod.rs` dispatches by `#[cfg(target_arch)]` and re-exports the arch module flat as `crate::arch`.
A RISC-V arch adds `#[cfg(target_arch = "riscv64")] mod riscv64; pub use riscv64::*;`. The contract the
rest of the kernel calls through `crate::arch`:

- **Top-level:** `set_percpu`/`percpu` (the per-CPU pointer), `psci_cpu_on` (start a secondary),
  `init`, `halt`, `wait_for_interrupt`, `dma_wmb`.
- **`mmu`:** `KERNEL_VA_BASE`, `phys_to_virt`/`virt_to_phys`, `init`/`init_secondary`, the user-mapping
  surface (`map_current_user_page/frame`, `unmap_user_at`, `translate_at`, `map_page`/`unmap_page`,
  `activate_user`/`deactivate_user`, `switch_user_root`, `reserved_root`, `flush_asid`, `flush_tlb`,
  `user_can_read`/`write`, `current_user_root`, `translate`/`translate_user`, `is_enabled`), and the
  `VIRTIO_MMIO_BASE`/`SIZE`/`IRQ_BASE` consts.
- **`interrupts`:** `enabled`/`disable`/`restore`/`enable`.
- **`timer`:** `TIMER_INTID`, `TICK_HZ`, `init`, `tick`, `ticks`, `now`, `frequency`, `uptime_ms`,
  `spin_for`, `interval`, `missed_ticks`.
- **`exceptions`:** the `TrapFrame` type `syscall::dispatch` consumes, the fault stat statics, `init`.
- **`semihosting`:** `EXIT_SUCCESS`/`EXIT_FAILURE`, `exit` (the test-harness exit).
- **Assembly symbols:** `switch_to`, `thread_trampoline`, `user_entry_trampoline`, `secondary_boot`.

## The two HAL leaks the port exposes (and must fix)

The port is worth doing precisely because it finds where the abstraction leaked. Two are already known:

1. **`thread::Context` is aarch64-register-shaped in *portable* `thread.rs`.** ~~It names `x19`..`x30`,
   the aarch64 callee-saved set, and is a contract with `context.s`.~~ **Closed (commit `fdc4376`),
   before RISC-V was started, as an aarch64-only refactor proved against the green baseline.** The
   deeper leak was not the field names but the two construction sites, which encoded the register
   *mapping* (`x19`=closure/entry, `x20`=shim/user-sp, `x21`..`x23`=args, `x30`=trampoline). The
   struct, the `switch_to`/trampoline externs, and the frame construction now live in
   `arch/aarch64/context.rs` behind two intent-named constructors, `Context::for_kernel_thread(closure_at,
   call_shim)` and `Context::for_user_thread(entry, user_sp, args)`; the fields are private. `thread.rs`
   names no register. RISC-V implements the same two constructors with its own set (`s0`..`s11` + `ra`,
   `a0`/`a1` for args) and `thread.rs` does not change.

2. **The `paging` crate encoded the aarch64 descriptor format.** **Closed (the trait option, calef's
   call; DECISIONS §17).** `Flags` was aarch64 descriptor bits (`AF`, `SH`, `AP_*`, `PXN`, `UXN`, `NG`,
   MAIR index) and the walk assumed 4 levels. Now: `Flags` is a format-neutral capability set (same
   constructor/predicate API), a `PageFormat` trait captures the seam (`LEVELS`, the half split, and
   is-present / extract-address / encode-table / encode-and-decode-leaf), the `Mapper` walk is written
   once and generic over `F: PageFormat`, and `Aarch64` (4-level) and `Sv39` (3-level) each implement
   it. The walk is proved once; each format carries its own Kani proofs of index-in-bounds,
   address/permission separation, and the half split, so **RISC-V paging inherits the same formal
   verification aarch64 has**. Base Sv39 has no device-memory PTE bit, so `CAP_DEVICE` rides in an RSW
   software bit to keep the round-trip exact. Portable code names the running format as
   `arch::mmu::Format`. aarch64 stayed green throughout (116 kernel tests, 37 paging host tests, both
   `cargo clippy` passes clean). The Sv39 format is defined and proved; **wiring the RISC-V `mmu.rs` to
   use it (real kernel tables + `satp` + the high-half) is the remaining MMU-step work.**

3. **`user.rs` embedded the entire userspace-entry mechanism (the headline leak).** **Closed.** This
   was the big one the compile step surfaced, larger and more delicate than the other two because it
   is the privilege boundary. The nominally-portable `user.rs` directly:
   - constructs an aarch64 `TrapFrame` (`elr`/`spsr`/`sp_el0`, a 31-entry `x`) to drop to EL0, and
     defines the `SPSR_EL0T` constant and the `enter_userspace` extern;
   - carries `sync_icache` with `dc cvau`/`ic ivau`/`dsb`/`isb` inline `asm!` (RISC-V wants a single
     `fence.i`);
   - embeds a corpus of **hand-written aarch64 user programs** (`USER_HELLO`, the hostile program) via
     `global_asm!` right in the file, a standing rule-1 violation (`asm!`/`global_asm!` outside
     `arch/`) that predates the port.

   The fix was a set of arch seams mirroring how leak #1 was closed: a `TrapFrame::for_user_entry(entry,
   user_sp, args)` constructor in `arch`, an `arch::sync_icache(va, len)` (aarch64 `dc cvau`/`ic ivau`;
   RISC-V `fence.i`), an `arch::current_sp()` (a fourth small leak: `stack.rs` read `sp` with inline
   `asm!`), the `SPSR`/`enter_userspace` items relocated under `arch/aarch64` behind `arch::enter_user`,
   and the embedded aarch64 programs (plus their test and boot-tour consumers) gated to aarch64. RISC-V
   reaches U-mode through the ELF-load path, not the hand-written programs. `user.rs` now names no
   register and no arch instruction.

   **A sharp lesson from the extraction, recorded because it will bite again:** the user-entry
   `TrapFrame` is written onto the *top of the caller's own kernel stack*, overlapping the caller's live
   call frames, and is intact only until `enter_userspace` does `mov sp, x0` **provided nothing pushes
   onto the stack in between**. The pre-seam code satisfied this because the jump was a direct tail call
   from the frame-writing function. Wrapping it in an ordinary `arch::enter_user` function silently
   broke it: the wrapper's own call frame push corrupted the just-written frame (seen as a child thread
   getting `sp_el0 = 0`, then a translation fault). The exec path survived by luck of stack depth; the
   deeper TCB-child path did not. The fix is `#[inline(always)]` on `enter_user`, which is load-bearing,
   not cosmetic, and is commented as such at the definition. Only the full aarch64 test suite caught
   this; a compile-only check would have shipped it.

   **And the RISC-V answer to that lesson was wrong for a year (milestone 71, 2026-08-03).** The
   paragraph above says the frame goes at the top of the kernel stack. RISC-V did not do that. Its
   TCB entry path (`user_entry_trampoline` -> `user_thread_entry` -> `enter_frame`) is shallow enough
   that a frame at the top would have overlapped `enter_frame`'s own stack, so the port computed the
   address from the **live `sp`** instead:

   ```rust
   let slot = (crate::arch::current_sp().min(top) - size_of::<TrapFrame>() as u64) & !15;
   ```

   That trades a deterministic overlap for an intermittent one, and the intermittent one is worse
   because everything below `sp` belongs to somebody else. Two facets, both real:

   - `current_sp()` is a **real call at opt-level 0** with a 16-byte frame, so it returns `sp - 16`
     and the frame landed at `sp - 304`. `trap.s` builds an S-mode trap frame at `sp - 288`. The two
     differ by exactly 16 bytes, so the user frame's fields sat on the trap frame's fields shifted by
     two register slots:

     | user frame field | offset | reads, after a trap | value |
     |---|---|---|---|
     | `x[2]`, the user `sp` | +16 | trap `x[0]` | **literally 0**, `trap_entry` does `sd zero, 0*8(sp)` |
     | `sepc` | +256 | trap `x[30]` | `t5`, which is 0 only sometimes |
     | `sstatus` | +280 | trap `scause` | UXL = 0, an illegal U-mode XLEN |

   - any **call** from `enter_frame` after the frame is built lands in the same region. There are
     none in the shipped code, but the investigation's own instrumentation tripped over it twice:
     `core::hint::spin_loop()` and `ptr::write_volatile` are calls at opt-level 0, and their 48-byte
     frames landed on the user frame's `sepc` and `stval`.

   **The two faces of the fault, and why one guard cannot see both.** The `sepc == 0` guard in
   `enter_user` fires only in the subcase where `t5` happened to be 0. Every other time the thread
   `sret`s to a garbage PC with an illegal `sstatus`, dies on its first instruction, and never answers
   whoever was waiting on it. That is a lost-wakeup hang carrying **no guard message at all**, which
   is why a CI job can hang in the watchdog with the guard compiled in, live, and silent. A run that
   does not print the guard's message is not a run in which this did not happen.

   **Which threads were exposed, which is narrower than it looks.** The clobber needs an interrupt,
   and the two user-entry paths differ on that. A thread started through the TCB surface arrives at
   `user_entry_trampoline` from `schedule()`, which masked interrupts and does not restore them
   before the `sret`, and `user_thread_entry` never unmasks (unlike `thread_entry`, which does, right
   after `finish_switch`). So the TCB path ran `enter_frame` with `sstatus.SIE` clear and could not be
   hit. The **exec** path could: `spawn` makes an ordinary kernel thread, `thread_entry` enables
   interrupts, and that thread later becomes a user process through `run` -> `enter_frame`. Every
   `spawn_bare` subject and `spawn_init` is on that path.

   The two paths are not distinguishable from a panic message, and an early reading of this
   investigation got it wrong by trying: a TCB child configured at `CODE_VA` 0x40_0000 with
   `STACK_VA + FRAME_SIZE` and an exec child at an ELF base of 0x40_0000 with `USER_STACK_TOP` print
   **the same two numbers**. Read the path from the test, not from the entry and stack values.

   The fix is a **reservation, not a moving target**: `user_entry_trampoline` drops `sp` by a frame's
   worth before the first Rust frame exists, so the region is off-limits to the entry path by
   construction, and `enter_frame` uses `top - size_of::<TrapFrame>()` on both ISAs. aarch64 took the
   same reservation: its overlapping slots only happen to be dead by the time `frame.write` runs, and
   "happens to be dead" is not an invariant. `enter_frame` now asserts the slot is at or above the
   live `sp`, which the old placement failed on the very first user entry.

   It also unstranded two things. `user_pc` on RISC-V was a stub returning 0 **because** there was no
   fixed address to read; it is now the aarch64 twin. And on the exec path the old placement left the
   user thread with only the kernel stack that happened to lie below `enter_frame`'s `sp`, with
   everything `run` and `load` had consumed stranded above the frame and never reclaimed.

   How it was proved, since it had never reproduced locally in nine full runs: widening the window
   with a **call-free** spin between `frame.write` and `enter_user` reproduced it on the first run,
   deterministically, printing `sp 0x0 want 0x501000`. The same probe with the fix in place never
   fired.

   **BUGS.** This closes the `sepc == 0` fault. It does **not** close every lost-wakeup hang in the
   RISC-V suite, and one of them is on the record as surviving this reasoning: a documentation-only
   PR (#21, zero lines of code) hung the watchdog on
   `reclaim_frees_a_started_then_exited_childs_regions`, with the guard compiled in and silent. That
   test's child is started through the **TCB** path, which is the one that runs `enter_frame` with
   interrupts masked and therefore cannot take the clobber described above. Its child is
   `REPORT_STUB`, a `SEND` to an endpoint the test is `ipc_recv`ing on, so a hang there reads as an
   IPC rendezvous that missed rather than as a frame that was overwritten. What the section above
   does settle is the *inference*: a silent run is not a run in which the frame fault did not happen,
   because the guard sees only the `t5 == 0` subcase.

   **That hang is closed, and it was never a RISC-V bug** (milestone 72). One line of test code
   probed `reclaim_region(...).is_err()` on the child's own TCB region, which under DECISIONS §16 as
   amended *arms the kill* on the child; the child was then reaped at its next preemption, before it
   could `SEND`. Widening the window reproduces it on **aarch64** just as deterministically, which is
   the control that rules the ISA out. Full account in notes/scheduler.md. The dump that made it
   legible is still owed to `user_pc` no longer returning 0 here, which is the one thing this section
   can claim.

A related, smaller **ABI leak** the traps step resolves: `syscall.rs` reads the syscall number from
`frame.x[8]` and args from `frame.x[0..]`, the aarch64 `svc`+`x8` convention. RISC-V's `ecall` ABI
puts the number in `a7` and args in `a0`..`a5`; the RISC-V `TrapFrame` compiles today but the index
convention has to be reconciled when the trap dispatcher is real.

Finding these is the point; each gets pushed under `arch/`.

## RISC-V specifics (the clean-different)

- **Boot:** OpenSBI runs in M-mode and hands the kernel control in **S-mode**. No arm64 `Image` header;
  `-kernel <elf>` boots the ELF directly. The hart id arrives in `a0`, the DTB physical pointer in
  `a1` (aarch64 put the DTB in `x0` and needed the Image header to get QEMU to pass it at all).
- **Firmware ABI = SBI** (the PSCI analog): **HSM** (hart state management) `sbi_hart_start` for SMP
  bring-up (replaces `psci_cpu_on`); **TIME** for the timer; **DBCN**/legacy console for the earliest
  prints before the UART driver exists; **SRST** (system reset) for the test-harness exit (replaces
  ARM semihosting `exit`).
- **Per-CPU:** the `tp` register (thread pointer), the direct analog of `TPIDR_EL1`.
- **Traps:** a single `stvec` vector (vs aarch64's 16-slot `VBAR` table), with `scause` (cause),
  `stval` (faulting value), `sepc` (return PC). The `TrapFrame` holds the RISC-V GPRs. Interrupt vs
  exception is the top bit of `scause`; the syscall path is the `ecall` cause.
- **Interrupts:** masked via `sstatus.SIE` (vs `PSTATE.DAIF`); enabled per-source in `sie`, pending in
  `sip`. The external-interrupt controller is the **PLIC**; software/timer interrupts come from the
  **CLINT** (or the newer Sstc extension). Both sit under `drivers/`, like the GIC does today.
- **Timer:** the `time` CSR + `stimecmp` (Sstc extension) or CLINT `mtimecmp`, or SBI TIME. Replaces
  the ARM generic virtual timer (`CNTV_*`).

  **BUGS (fixed at milestone 19, and worth knowing before choosing SBI TIME on the next board):**
  SBI `set_timer` takes an absolute deadline but is **write-only**, unlike `CNTV_CVAL_EL0`, which can
  be read back. So the fixed deadline grid that keeps the tick rate honest has to be kept in
  software. It was not, from milestone 20 until milestone 19's test lane: the handler re-armed from
  `now()`, so the clock ran at **80 Hz against a configured 100 Hz** and nothing said so. See
  notes/riscv-arch-tests.md.
- **UART:** QEMU virt's console is an **NS16550** at `0x1000_0000`, not a PL011. A new
  `drivers/ns16550.rs` (the PL011 driver stays; this is a sibling, like a second board's UART).
- **Paging:** **Sv39** (three-level, 39-bit VA) to start, `satp` holding the root PPN + mode. The
  high-half direct map uses Sv39's top VA range (sign-extended); `KERNEL_VA_BASE` is chosen to fit it.

## The per-hart `tp` hazard: a preempted kernel thread must not carry its origin hart's pointer

`tp` being a *general register* (line above), not a system register, is the one place the "clean
different" bites. `TPIDR_EL1` is banked per core and no thread ever saves or restores it; a thread
that migrates from one aarch64 core to another simply reads whatever `TPIDR_EL1` the destination core
set at boot. RISC-V has no such register. The kernel per-CPU pointer lives in `tp`, and the trap
entry saves the full GPR set, `tp` included (`x[4]`), because a trap from U-mode arrives with the
*user's* `tp` and the frame has to preserve it.

That save is correct. The naive *restore* is not. `trap_return` originally reloaded `x[4]`
unconditionally, which is right for a return to U-mode (the user owns `tp`) and was harmless for a
kernel thread as long as a kernel thread always resumed on the hart it was preempted on. DECISIONS
§28 broke that assumption: idle harts now steal runnable threads, so a kernel thread preempted on
hart A (its frame holding `tp = &percpu[A]`) can resume on hart B. The unconditional restore then
loaded `&percpu[A]` while running on B, and every `cpu::current()` after that read **hart A's**
per-CPU block: its `current` tid, its `idle`, its run queue, its `held_rank`.

The failure was spectacular and non-deterministic. A migrated worker, thinking it was on A, would
call `exit()` and mark *hart A's* current thread (often A's idle) `Finished`; A's next `schedule()`
then panicked "the last thread exited". Other interleavings tripped the rank-`assert!` in
`sync.rs` (a bogus `held_rank` read from the wrong hart) or simply lost a thread and hung. Adding
tracing moved it, the classic weak-ordering-adjacent tell, though the root cause is not a missing
barrier: it is reading a hart-local register that had been restored from another hart's frame.

**Fix (in `trap.s`, the return path):** restore `x[4]` only for a return to U-mode. For an S-mode
(kernel) return, keep the live `tp`. It is already this hart's: `trap_entry` reloaded it from the
per-hart stash (`sscratch`), and `switch_to` never touches `tp`, so it survives a migration intact.
This is the exact mirror of `trap_entry`'s `ld tp, 8(t0)`. The regression test
(`smp::tests::a_migrated_kernel_thread_keeps_its_hart_pointer`) drives the steal-a-preempted-thread
interleaving and checks the invariant directly: `sscratch` names this hart's `TrapStash` and never
migrates, so `(sscratch → hart) == cpu::id()` must hold on every spin; the aarch64 twin of the check
is a constant `true`. The lesson generalizes: **anything the kernel keeps in a general register is
part of the thread context on RISC-V, and the trap path must decide per privilege level whether it
is the thread's or the hart's.** `tp` is the hart's.

## Build setup

- **Target:** `riscv64imac-unknown-none-elf`, the integer-only target, the analog of aarch64's
  `-softfloat` (no FP state in the kernel). It runs on QEMU virt's rv64gc CPU (imac is a subset).
- **Linker:** `link-riscv64.ld`. RAM base is `0x8000_0000`; OpenSBI loads the payload at `0x8020_0000`.
  It links **high and loads low** (`AT()`), like the aarch64 script: the kernel lives in the Sv39
  high half, and `boot.s` does the higher-half transition. (An earlier cut linked low / bare-mode for
  the first boot+console; that was replaced when high-half landed.)
- **Wiring:** `build.rs` selects the linker script by `CARGO_CFG_TARGET_ARCH`; `.cargo/config.toml`
  adds a `[target.riscv64imac-unknown-none-elf] runner` pointing at a new `scripts/qemu-runner-riscv64.sh`
  (`qemu-system-riscv64 -machine virt -bios default -kernel <elf> -smp N -serial stdio ...`); `xtask`'s
  hardcoded `TARGET` becomes arch-parameterized; `rust-toolchain.toml` lists the target.

## The incremental plan (each a provable, committable piece)

1. **Compiles for `riscv64`. Done.** The full arch contract stubbed (pure primitives real, every
   deferred piece a loud `unimplemented!()`), all four leaks resolved, the build wiring in place.
   `cargo build --target riscv64imac-unknown-none-elf` links a RISC-V ELF, and both `cargo clippy`
   passes (aarch64 and riscv64) are clean under `-D warnings`. aarch64 stays fully green: 116 kernel
   tests pass. This proves the boundary is complete: a second architecture compiles against the entire
   kernel with no change above `arch/` (the four leaks were the exceptions, now closed). What is *not*
   yet done is running: every `unimplemented!()` in `arch/riscv64` is real work for the steps below.
2. **Boots and prints. Done.** OpenSBI hands off in S-mode; `_start` sets `sp` and zeroes `.bss`;
   `kernel_main` sets the `tp` per-CPU register, brings up the NS16550 console (a plain-volatile byte
   UART, `drivers/ns16550.rs`, selected by a compile-time alias in `console.rs`), and prints a banner
   including the real device-tree pointer OpenSBI passed in `a1`. That one line exercises the whole
   chain: S-mode entry, stack, `.bss`, `tp`, `interrupts::disable` (the console lock uses it), the
   UART, and the DTB handoff. Then a clean `wfi` halt. The milestone-1 moment on a second ISA. A fifth
   leak surfaced and was deferred, not fixed: portable code (`sched`, `smp`, `syscall`, `user`) names
   `drivers::gic` directly, an interrupt-controller coupling that the traps step resolves with a PLIC
   abstraction; until then the GIC driver stays un-gated (it compiles on riscv and is simply dead).
3. **Traps. Done.** `stvec` + the `TrapFrame` + trap.s (save 36 registers, dispatch on `scause`,
   restore, `sret`). The syscall-ABI leak is resolved: `syscall.rs` reads the number and args through
   `TrapFrame::{syscall_nr, arg, set_arg}`, so `ecall`'s a7/a0..a5 map without portable code naming a
   register. Proven by a boot self-test: an `ebreak` is caught, `sepc` stepped past it, and `sret`
   returns. First cut is S-mode traps on the current stack; the `sscratch` stack switch for U-mode
   traps arrives with the user path.
4. **MMU (Sv39). Done (kernel side).** The paging-format work (leak #2), the **higher-half boot
   transition** (calef chose high-half, DECISIONS §17; proven by the banner's live code address
   `0xffffffc0_8020_xxxx`), the **fine-grained W^X kernel tables** (`mmu::init` via `Mapper<_, _,
   Sv39>`, replacing the coarse RWX boot table, `satp` switched live with the console surviving), and
   the **kernel mapping surface** (`map_page`/`unmap_page`/`translate`/`flush_tlb`, proven by a
   map/write/read/unmap self-test). **Remaining:** the *user*-mapping surface and per-process `satp`
   (the user path). The RISC-V single-`satp` model means every process root shares the kernel's
   high-half top-level entries; `reserved_root`/`switch_user_root` already implement the kernel-thread
   side of that.
5. **Timer + interrupts. Timer done.** SBI TIME `set_timer` + `sie.STIE`, the dispatcher routes
   `scause` = timer to `timer::tick`; proven by ~17 ticks in 0.2 s at 100 Hz. **Remaining:** the PLIC
   (external/device interrupts), which also resolves the `drivers::gic` leak; it is exercised by the
   userspace-driver path, so it lands with that.
6. **The capability core runs. Done (a user program runs at U-mode).** The scheduler and context
   switch run on RISC-V ("2 of 2 kernel threads ran"); the user-mapping surface and the single-`satp`
   model work (a process `satp` is installed, the kernel survives via `share_kernel_half`, a user page
   maps and translates); and **a hand-written RISC-V program runs at U-mode and makes syscalls**:
   "a program ran at U-mode and made 3 syscalls (yield/yield/exit via ecall)". That exercises the
   whole path: `enter_user`'s `sret`, the `sscratch` U-mode trap entry, `ecall` dispatch through the
   ABI accessors, and the return to U-mode, twice, then `exit`.

   **The last-mile bug and its root cause (a genuine HAL lesson): `tp` is a general register on
   RISC-V, not a system register.** aarch64 keeps the per-CPU pointer in `TPIDR_EL1`, a system
   register that survives an EL0 round trip untouched. RISC-V's `tp` (x4) is an ordinary GPR, so the
   user trap frame left it 0, the `sret` gave U-mode `tp = 0`, and the ecall trap handler ran with
   `tp = 0` and null-dereferenced `cpu::current()`. That panicked; the panic handler then re-panicked
   while *formatting* the message (a `core::fmt` path), recursing into itself and blowing the kernel
   stack, which is what presented as the mystery "store fault at the frame page" (the nested-fault
   cascade of the overflow, unrelated to the frame's mapping, which was fine all along). QEMU's
   `-d int` exception log pinned it: the recursion was in `rust_begin_unwind`, and a raw-UART dump of
   the panic location (bypassing `core::fmt`) named `cpu.rs:160`. Fix: `for_user_entry` carries the
   kernel `tp` in the frame, so it survives the round trip. **Follow-up (noted, not yet done):** that
   leaks the kernel per-CPU address into U-mode's `tp`; the leak-free fix restores `tp` in `trap.s`
   from a per-hart source (the standard sscratch-trapframe approach), also needed for SMP.

   **IPC/caps work too:** a second program is built from parts (a TCB, a code and stack mapping, an
   endpoint capability granted in slot 0), started, and it `SYS_INVOKE`s that cap to SEND a word home,
   which the kernel receives: "a U-mode process invoked a cap and SENT 0xc4". That is the whole
   capability boundary (cap lookup, rights check, IPC) from U-mode on RISC-V. One RISC-V-specific bug
   surfaced there: the TCB entry path is *shallow* (trampoline → `user_thread_entry` → `enter_frame`),
   so a trap frame placed at the very top of the kernel stack (fine for aarch64, whose entry paths are
   deep) overlapped and corrupted `enter_frame`'s own frame as `frame.write` ran, sending the `sret`
   to a garbage `sepc`. Fixed by placing the RISC-V trap frame just below the live `sp` (`sscratch`
   tracks it, so re-entries stay consistent).

   **Preemption works too, and it is the property that separates a kernel from a runtime.** The
   S-mode timer already fired every 10ms; the missing half was hanging a reschedule off it. Now
   `riscv_trap_dispatch` records the tick (`sched::on_tick`) and defers the switch to the trap tail,
   the same four lines as aarch64's `handle_irq`: `if take_need_resched() && is_running() {
   count_preemption(); schedule(); }`. Two threads whose entire body is a tight loop (no yield, no
   syscall, not even a call) both make progress: ~680k and ~666k iterations in 0.2s, 18 preemptions.
   Under any cooperative scheduler the first would own the CPU forever. This is the RISC-V half of
   DECISIONS §5, and it works for a U-mode thread and an S-mode kernel thread alike, because `trap.s`
   saved a full frame for whichever the timer interrupted, and `schedule()` returns through
   `trap_return` to exactly the instruction it left.

   **A real compiled ELF runs at U-mode too.** Every user program above is a hand-written
   machine-code blob; the `worker` program is a Rust binary compiled to a riscv64 ELF, delivered as
   the initrd (QEMU `-initrd`, read from `/chosen/linux,initrd-start` the same way Linux gets its
   initramfs), and run through the kernel's *real* ELF loader: `user::load` parses the file, builds an
   address space with each `PT_LOAD` segment mapped W^X at the VA it names, and maps a stack. The
   worker is granted WRITE on one endpoint (slot 0), started with an input in `a1`, squares it, and
   SENDs the answer home: "loaded a 13568-byte riscv ELF, ran worker(7) at U-mode, it sent 49". Three
   small pieces made this work, each closing an aarch64 assumption: `user_rt` (the userspace syscall
   runtime) grew a RISC-V ABI (`ecall`+`a7` beside `svc`+`x8`); the `elf` crate accepts the running
   kernel's machine (`EXPECTED_MACHINE`, cfg-selected, symmetric: each kernel refuses the other's
   ELF); and `worker.rs` arch-gated its one trap instruction (`ebreak` vs `brk`). The loader itself
   (`load`, `map_segments`) was already arch-neutral and needed no change. This is the first RISC-V
   userspace program the kernel did not hand-write.

   **And userspace init builds the system, from a richer initrd.** The single-ELF initrd became a
   nifefs archive holding two programs: `init` (the portable `builder`) and `worker`. The kernel
   (`riscv_initrd_demo`, a minimal `spawn_init` with no GIC/PL011/IRQ baggage) loads only `init`, maps
   the whole archive read-only into it, and grants it two capabilities: an untyped budget and a report
   endpoint. From those, `init` reads `worker` out of the archive by name, builds it as a child
   *entirely from its own budget* through the capability verbs (retype an address space, copy each
   segment into retyped frames and map them, retype a TCB, endow it, configure, start), hands the
   child a WRITE view of the report endpoint, and starts it with an input. The child squares it and
   SENDs `81` straight home; the kernel never parsed or mapped the worker. This exercised the *full*
   userspace capability syscall surface on RISC-V for the first time (untyped RETYPE/RETYPE_OBJ,
   aspace MAP_INTO, frame MAP, tcb CAP_INSERT/CONFIGURE/START), all across the `ecall` ABI. `builder`
   is fully portable; it also builds for aarch64, though aarch64's init is the PL011-wired role of
   `hello`. Build the archive with `cargo xtask initrd-riscv`.

   **And a real device interrupt flows through the PLIC.** A keystroke becomes a message: the
   NS16550 raises its receive line into the PLIC, the PLIC delivers a supervisor external interrupt
   (`scause` = 9), `riscv_trap_dispatch` claims it (acknowledging and masking that source), looks up
   the endpoint it is routed to, and notifies it; a kernel driver thread blocked there wakes, reads
   the byte, and re-arms the source. Boot shows "a keystroke reached a kernel driver through the
   PLIC: 0x41 ('A')". The shape is exactly aarch64's `handle_irq` (mask, notify, complete) and it
   reuses the arch-neutral routing (`bind_irq` / `irq_route` / `irq_notify`) unchanged. The PLIC's
   base comes from the device tree, which exposed one parser bug: `node_reg` only matched nodes at
   depth 2 (the aarch64 GIC is a direct child of the root), but the riscv PLIC is at
   `/soc/plic@c000000`, depth 3. `node_reg` now matches at any depth and decodes `reg` with the
   parent's cell counts. Two test caveats: pipe the byte *after* boot (the console clears the RX FIFO
   during init), and the demo driver runs in the kernel for now.

   **The driver moved to userspace, and the last leak closed.** First the demo driver became an
   unprivileged U-mode process (`user/src/driver.rs`): the kernel maps the NS16550 into it
   device-typed and grants it an `Irq` capability and a report endpoint, and it runs the seL4
   IRQHandler loop, `WAIT` on the cap (block for the interrupt as a message), read the byte through
   its own device mapping, `SEND` it, `ACK`. "an unprivileged userspace driver serviced the UART via
   its Irq cap and sent 0x5a". The kernel is never in the data path. That exercised the `Irq`
   capability `WAIT`/`ACK` from RISC-V U-mode for the first time, and the `ACK` re-arm is what forced
   the interrupt-controller seam into being.

   Then `arch::irq` was extracted with two working consumers behind it (the GIC and the PLIC), and
   `drivers::gic` was gated to aarch64. Every portable caller (`sched::place_on`, `smp`,
   `user::spawn_init`, `main::interrupts_init`, the `Irq` ACK) now names `arch::irq`, not a
   controller. The only code still naming `drivers::gic` is aarch64 arch code (rule #1 lets arch name
   its own driver) and the `cfg(test)` aarch64 IRQ tests. **That was the last HAL leak.** A new ISA is
   a new `arch/` directory, not a diff across the kernel, with zero exceptions in portable non-test
   code.

   **The port is complete.** Boot, MMU, traps, timer, scheduler, preemption, U-mode user programs,
   capability invocation, userspace-built processes (init loads and builds children), and device
   interrupts serviced by a userspace driver all run on RISC-V, the same portable core as aarch64.

The reward beyond the proof: when a real RISC-V board (a ~$70 StarFive VisionFive 2 / Milk-V Mars, or a
rented Graviton later) is on hand, the QEMU-virt work transfers, because real boards use the same
OpenSBI + S-mode + device-tree model. And "one verified capability microkernel, aarch64 and RISC-V" is
a stronger demonstrator line than any single board.

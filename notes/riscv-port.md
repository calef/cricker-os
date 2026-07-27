# Porting cricker-os to RISC-V (the second architecture)

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

2. **The `paging` crate encodes the aarch64 descriptor format.** It looks generic (page-table math)
   but its `Flags` are aarch64 descriptor bits (`AF`, `SH`, `AP_*`, `PXN`, `UXN`, `NG`, MAIR attr
   index) and it assumes 4 levels. RISC-V Sv39 is 3 levels (Sv48 is 4) with a different PTE layout
   (`V R W X U G A D` + PPN, no MAIR, no separate exec-never per privilege). Fix: a RISC-V page-table
   format. Either a sibling `paging_riscv` module or generalize `paging` behind a trait that both
   descriptor formats implement. The *arithmetic* (index extraction, the walk) is shared; the *bit
   layout* is not.

Expect the compile-for-riscv step to surface a few more (anything that quietly assumed a GIC-shaped
interrupt model, TTBR0/1, or ASIDs). Finding them is the point; each gets pushed under `arch/`.

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
- **UART:** QEMU virt's console is an **NS16550** at `0x1000_0000`, not a PL011. A new
  `drivers/ns16550.rs` (the PL011 driver stays; this is a sibling, like a second board's UART).
- **Paging:** **Sv39** (three-level, 39-bit VA) to start, `satp` holding the root PPN + mode. The
  high-half direct map uses Sv39's top VA range (sign-extended); `KERNEL_VA_BASE` is chosen to fit it.

## Build setup

- **Target:** `riscv64imac-unknown-none-elf`, the integer-only target, the analog of aarch64's
  `-softfloat` (no FP state in the kernel). It runs on QEMU virt's rv64gc CPU (imac is a subset).
- **Linker:** a RISC-V `link.ld`. RAM base is `0x8000_0000`; OpenSBI loads the payload at
  `0x8020_0000`. First cut can link low (physical, `satp=0` bare mode) so boot+console needs no MMU;
  the high-half kernel VA comes with the Sv39 step.
- **Wiring:** `build.rs` selects the linker script by `CARGO_CFG_TARGET_ARCH`; `.cargo/config.toml`
  adds a `[target.riscv64imac-unknown-none-elf] runner` pointing at a new `scripts/qemu-runner-riscv.sh`
  (`qemu-system-riscv64 -machine virt -bios default -kernel <elf> -smp N -serial stdio ...`); `xtask`'s
  hardcoded `TARGET` becomes arch-parameterized; `rust-toolchain.toml` lists the target.

## The incremental plan (each a provable, committable piece)

1. **Compiles for `riscv64`.** The full arch contract stubbed, the two leaks fixed (Context cfg'd, a
   paging path chosen). Proves the boundary is complete: a second arch slots in as stubs.
2. **Boots and prints.** Real S-mode `_start` (set `sp`, zero `.bss`), a 16550 UART driver, "hello from
   RISC-V" on the serial line. The milestone-1 moment on a second ISA.
3. **Traps.** `stvec` + the `TrapFrame` + a fault that prints instead of dying silently.
4. **MMU (Sv39).** `satp`, the direct map, the high-half, the paging-format work. This is the biggest
   single step (it settles the `paging` generalization).
5. **Timer + interrupts.** Sstc/CLINT + PLIC, preemption on RISC-V.
6. **The capability core runs.** `caps`, IPC, the scheduler, the same code as aarch64, on RISC-V. The
   payoff: one verified capability core, two ISAs, and rule #1 proven (or its leaks all closed).

The reward beyond the proof: when a real RISC-V board (a ~$70 StarFive VisionFive 2 / Milk-V Mars, or a
rented Graviton later) is on hand, the QEMU-virt work transfers, because real boards use the same
OpenSBI + S-mode + device-tree model. And "one verified capability microkernel, aarch64 and RISC-V" is
a stronger demonstrator line than any single board.

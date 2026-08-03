# The kernel's real entry on RISC-V, and the higher-half transition.
#
# OpenSBI (the -bios, in M-mode) hands the S-mode payload control at its PHYSICAL load address
# (0x8020_0000) per the Linux RISC-V boot protocol, with a0 = hart id and a1 = the device-tree
# pointer. The kernel is linked HIGH (link-riscv64.ld), so every absolute symbol is a high-half VA, but
# we are executing at low physical addresses with paging off. So, exactly like aarch64's boot.s:
#
#   1. use PC-relative addressing (`lla`) while paging is off, which yields correct PHYSICAL addresses
#      (the relative offset is the same whether symbols are linked high or low),
#   2. point satp at a boot table that maps the kernel both at its physical address (so this code
#      keeps executing across the `csrw satp`) and at its high-half alias,
#   3. jump to the high-half alias of ourselves, and only then touch absolute (high) symbols.
#
# This is the single sketchiest moment in the RISC-V port: if the boot table is wrong, the `csrw
# satp` fetches the next instruction through a broken mapping and the machine vanishes with no
# output. See notes/riscv-port.md and arch/riscv64/mmu.rs (BOOT_PAGE_TABLE).

.section ".text.boot", "ax"
.global _start
_start:
    # a0 = hart id, a1 = DTB (both must survive to the high half; we touch only t0-t2 below).

    # --- turn Sv39 on ---
    # satp = (mode=8 (Sv39) << 60) | (physical PPN of the boot table). `lla` gives the table's real
    # physical address because we are running physically.
    lla     t0, BOOT_PAGE_TABLE
    srli    t0, t0, 12              # t0 = PPN = phys >> 12
    li      t1, 8 << 60             # Sv39 mode in satp[63:60]
    or      t0, t0, t1

    sfence.vma                      # drop any stale entries before the switch
    csrw    satp, t0                # paging ON. The identity gigapage keeps this PC valid.
    sfence.vma                      # make the new mapping take effect

    # --- jump to the high-half alias of _start_high ---
    # `lla` still yields a physical address (PC is still low); add KERNEL_VA_BASE to get the high VA,
    # which now translates (via the kernel gigapage) back to this same physical code.
    lla     t0, _start_high
    li      t1, 0xffffffc000000000  # KERNEL_VA_BASE (must match link-riscv64.ld and mmu.rs)
    add     t0, t0, t1
    jr      t0

_start_high:
    # Now executing at a high VA. From here every absolute symbol resolves correctly.
    la      sp, __stack_top         # the high boot stack

    # Zero .bss by hand (it occupies no bytes in the ELF). Both bounds are 8-byte aligned.
    la      t0, __bss_start
    la      t1, __bss_end
1:  bgeu    t0, t1, 2f
    sd      zero, 0(t0)
    addi    t0, t0, 8
    j       1b
2:

    # Stash the boot hart id (still in a0 from OpenSBI) before a0 becomes the kernel_main argument.
    # .bss was just zeroed, so this survives; the kernel reads it to learn which physical hart it is.
    la      t0, BOOT_HARTID
    sd      a0, 0(t0)

    # kernel_main(dtb): move the DTB (still in a1 from OpenSBI) into the first argument.
    mv      a0, a1
    call    kernel_main

    # kernel_main is `-> !`. If it ever returns, stop rather than run off into whatever follows.
3:  wfi
    j       3b

# The secondary-hart entry (SMP). SBI HSM `sbi_hart_start` (arch::psci_cpu_on) starts a hart HERE, at
# this PHYSICAL address, with paging off, in S-mode, a0 = its hart id and a1 = the opaque word core 0
# passed (this hart's high-VA stack top). The page tables already exist (core 0 built BOOT_PAGE_TABLE
# and the fine map), so we only replay the satp/high-half transition, exactly like `_start`, then jump
# to `secondary_main`. It adopts the fine map, sets its own trap vector and per-CPU state, and idles.
.global secondary_boot
secondary_boot:
    # Turn Sv39 on via the boot table (same three steps as _start). a0 (hart id) and a1 (stack top)
    # must survive, so touch only t0/t1.
    lla     t0, BOOT_PAGE_TABLE
    srli    t0, t0, 12
    li      t1, 8 << 60
    or      t0, t0, t1
    sfence.vma
    csrw    satp, t0
    sfence.vma

    # Jump to the high-half alias of secondary_high.
    lla     t0, secondary_high
    li      t1, 0xffffffc000000000  # KERNEL_VA_BASE (must match link-riscv64.ld and mmu.rs)
    add     t0, t0, t1
    jr      t0

secondary_high:
    mv      sp, a1                  # the high-VA stack top core 0 passed as the opaque word
    # a0 still holds this hart's id; secondary_main(cpu_id) reads it as its first argument.
    call    secondary_main
    # secondary_main is `-> !`. If it ever returns, park rather than run on.
1:  wfi
    j       1b

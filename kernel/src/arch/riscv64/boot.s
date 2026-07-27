# The kernel's real entry on RISC-V. OpenSBI (the -bios, in M-mode) loads this S-mode payload at
# 0x8020_0000, the ELF entry address, and jumps here with:
#   a0 = this hart's id
#   a1 = the device-tree blob (physical pointer)
#
# Unlike aarch64, there is no Image header and no text_offset dance: QEMU boots the ELF directly and
# OpenSBI has already put us in S-mode. And unlike aarch64's boot.s, nothing here turns the MMU on:
# the kernel runs bare (satp = 0, virtual == physical) through boot and console. The Sv39 step adds
# the page tables and the high-half; until then `la` yields correct addresses because we are linked
# at the address we are loaded at. See notes/riscv-port.md and link-riscv.ld.
#
# UNPROVEN until the console step actually boots this. It assembles and links now.

.section ".text.boot", "ax"
.global _start
_start:
    # A boot stack. Linked low, loaded low, so the address `la` computes is right with no MMU.
    la      sp, __stack_top

    # Zero .bss by hand: it occupies no bytes in the ELF, so nothing loaded it. Both bounds are
    # 8-byte aligned (the linker script over-aligns to 4096), so store 8 at a time.
    la      t0, __bss_start
    la      t1, __bss_end
1:  bgeu    t0, t1, 2f
    sd      zero, 0(t0)
    addi    t0, t0, 8
    j       1b
2:

    # kernel_main(dtb: usize): the portable entry. It expects the device tree in the first argument
    # (a0), where OpenSBI handed us the hart id; the DTB is in a1, so move it over.
    mv      a0, a1
    call    kernel_main

    # kernel_main is `-> !`. If it ever returns, stop rather than execute whatever follows.
3:  wfi
    j       3b

# The secondary-hart entry (SMP). Referenced by smp.rs so it must link; the SBI-HSM bring-up that
# actually starts a hart here, sets its stack, and calls the portable `secondary_main` is the SMP
# step. For now a started hart would simply park.
.global secondary_boot
secondary_boot:
1:  wfi
    j       1b

// The kernel's real entry, reached by the `b _boot` in image_header.s.
//
// QEMU's `virt` machine hands us the CPU at EL1 with the MMU off, caches off, sp
// holding garbage, and (because we ship a flat arm64 Image) the device tree pointer
// in x0.
//
// Everything here has to be assembly, because no Rust function can run until we
// have a stack. See notes/stack.md and notes/reading-assembly.md.

.section ".text.boot", "ax"
.global _boot

_boot:
    // The Linux arm64 boot protocol puts a Device Tree Blob pointer in x0, and now
    // that we ship a flat Image with a proper header, QEMU actually gives us one.
    // Stash it before we clobber x0 with system-register reads.
    // See notes/boot-protocol.md.
    mov     x19, x0

    // Park every core but core 0. MPIDR_EL1's low byte is the core number.
    // (Single-core for now: DECISIONS.md §6.)
    mrs     x0, mpidr_el1
    and     x0, x0, #0xff
    cbnz    x0, park

    // Set up the stack. Nothing has done this for us; sp is garbage until this
    // instruction, and no Rust function can run before it. The stack grows DOWN
    // from __stack_top, which the linker script placed above the reserved space.
    ldr     x0, =__stack_top
    mov     sp, x0

    // Zero .bss by hand. It occupies no bytes in the ELF, so nobody loaded it,
    // and there is no C runtime here to clear it. The linker script guarantees
    // both bounds are 8-byte aligned, so we can store 8 bytes at a time.
    //
    //   str xzr, [x0], #8   ->  write 8 zero bytes at x0, THEN advance x0 by 8
    ldr     x0, =__bss_start
    ldr     x1, =__bss_end
1:  cmp     x0, x1
    b.hs    2f
    str     xzr, [x0], #8
    b       1b
2:

    // kernel_main(dtb: usize) -> !
    // x0 is the first argument by the aarch64 calling convention (AAPCS64).
    mov     x0, x19
    bl      kernel_main

    // kernel_main is `-> !`, so we never get here. Halt if we somehow do.
    b       park

park:
    wfe                     // "wait for event": sleep this core at low power
    b       park            // if something wakes it, go back to sleep

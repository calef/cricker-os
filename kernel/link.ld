/* cricker-os kernel image layout.
 *
 * QEMU's `virt` machine puts RAM at 0x4000_0000. Given `-kernel kernel.elf`, QEMU
 * reads our ELF program headers, copies each segment to the physical address named
 * there, sets PC to ENTRY, and lets go. That is the entire boot process.
 *
 * Nobody relocates us. Nobody gives us a stack. Nobody zeroes our .bss.
 * See notes/linker-scripts.md and notes/elf.md.
 */

ENTRY(_start)

SECTIONS
{
    /* 128 KiB into RAM, following the ARM Linux image convention. */
    . = 0x40080000;

    .text : ALIGN(4096) {
        /* _start MUST be first: it is where QEMU jumps. KEEP() because nothing
         * *calls* _start, so dead-code elimination would otherwise delete it. */
        KEEP(*(.text.boot))
        *(.text .text.*)
    }

    .rodata : ALIGN(4096) {
        *(.rodata .rodata.*)
    }

    .data : ALIGN(4096) {
        *(.data .data.*)
    }

    /* .bss occupies zero bytes in the file, so nothing loads it and nothing
     * zeroes it. boot.s does that by hand, 8 bytes at a time, which is why both
     * bounds must be 8-byte aligned. */
    .bss : ALIGN(4096) {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(16);
        __bss_end = .;
    }

    /* The boot stack. It grows DOWN, so __stack_top is the HIGH address and
     * therefore comes *after* the reserved space. sp must be 16-byte aligned
     * (notes/stack.md).
     *
     * TODO(milestone 4): once the MMU is on, leave a page unmapped below
     * __stack_bottom so that a stack overflow faults instead of silently eating
     * our .bss. */
    . = ALIGN(16);
    __stack_bottom = .;
    . += 0x10000;              /* 64 KiB */
    __stack_top = .;

    /DISCARD/ : {
        *(.comment)
        *(.note*)
        *(.eh_frame*)         /* stack-unwinding tables; we never unwind */
    }
}

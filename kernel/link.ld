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
    /* 512 KiB into RAM. This number is NOT free to change: it must match the
     * `text_offset` field in the arm64 Image header (image_header.s), because that
     * field is what tells QEMU where to load us. 0x4000_0000 + 0x8_0000. */
    . = 0x40080000;
    __image_start = .;

    .text : ALIGN(4096) {
        /* The arm64 Image header MUST be at byte 0 of the image: the entry point is
         * the first byte, and QEMU reads the magic at a fixed offset from the start.
         * KEEP() because nothing *calls* it, so dead-code elimination would otherwise
         * delete the entire entry point. */
        KEEP(*(.text.header))
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
     * __stack_bottom so that a stack overflow FAULTS instead of silently eating our
     * .bss. This is not hypothetical: milestone 3 blew this stack with a 16 KiB array
     * in a test, wrote through .bss and .data into .text, and hung the machine while
     * printing something unrelated. There is a canary at __stack_bottom now
     * (kernel/src/stack.rs), but a guard page is the real answer. See notes/stack.md. */
    . = ALIGN(16);
    __stack_bottom = .;
    . += 0x10000;              /* 64 KiB */
    __stack_top = .;

    /* The `image_size` field of the arm64 Image header (image_header.s).
     *
     * It deliberately runs all the way to __stack_top, not just to the end of .data.
     * The flat binary itself stops after .data (since .bss occupies no file bytes),
     * but this number tells the bootloader how much MEMORY we occupy. Understate it
     * and QEMU will happily place the device tree blob on top of our .bss or our
     * stack, and we will corrupt it the instant we push a stack frame. */
    __image_end = .;
    __image_size = __image_end - __image_start;

    /DISCARD/ : {
        *(.comment)
        *(.note*)
        *(.eh_frame*)         /* stack-unwinding tables; we never unwind */
    }
}

/* cricker-os kernel image layout.
 *
 * QEMU's `virt` machine puts RAM at 0x4000_0000. Given a flat arm64 Image, QEMU follows
 * the Linux boot protocol: it loads us at RAM_base + text_offset, hands us a device tree
 * pointer in x0, and lets go. See notes/boot-protocol.md and notes/linker-scripts.md.
 *
 * Every section boundary here is exported, because milestone 4 maps each one with
 * DIFFERENT permissions and the linker is the only thing that knows where they landed.
 * They are all page-aligned for the same reason: MMU permissions are per-page, so a
 * section sharing a page with another section cannot have its own permissions.
 */

ENTRY(_start)

/* THE KERNEL IS LINKED HIGH AND LOADED LOW.
 *
 * Every symbol below is a VIRTUAL address in the top half. But `AT()` tells the loader to
 * put the bytes at the PHYSICAL address, which is where QEMU actually loads a flat Image.
 *
 * So the ELF carries both: p_vaddr = 0xffff_0000_4008_0000, p_paddr = 0x4008_0000. objcopy
 * lays the flat binary out by LMA, which is what QEMU reads.
 *
 * Which means the code in boot.s runs at a physical address while every absolute address
 * baked into it is virtual. See boot.s and notes/higher-half.md for how it survives that.
 *
 * The base is 0xffff_0000_0000_0000, and the choice is not arbitrary: it touches only bits
 * 63:48, which are NOT TRANSLATED. So a kernel virtual address and its physical address have
 * identical page-table indices, VA = PA + KERNEL_VA_BASE is exact, and the boot map can be a
 * single table serving as both the identity map and the high-half map. */
KERNEL_VA_BASE = 0xffff000000000000;

/* 512 KiB into RAM. NOT free to change: it must match `text_offset` in the arm64 Image
 * header (image_header.s), which is what tells QEMU where to load us. */
PHYS_START = 0x40080000;

SECTIONS
{
    . = KERNEL_VA_BASE + PHYS_START;
    __image_start = .;

    .text : AT(ADDR(.text) - KERNEL_VA_BASE) ALIGN(4096) {
        __text_start = .;
        /* The Image header MUST be at byte 0 of the image, and _start with it. KEEP()
         * because nothing *calls* it, so dead-code elimination would delete the entry
         * point. */
        KEEP(*(.text.header))
        KEEP(*(.text.boot))
        *(.text .text.*)
        . = ALIGN(4096);
        __text_end = .;
    }

    .rodata : AT(ADDR(.rodata) - KERNEL_VA_BASE) ALIGN(4096) {
        __rodata_start = .;
        *(.rodata .rodata.*)
        . = ALIGN(4096);
        __rodata_end = .;
    }

    .data : AT(ADDR(.data) - KERNEL_VA_BASE) ALIGN(4096) {
        __data_start = .;
        *(.data .data.*)
        . = ALIGN(4096);
        __data_end = .;
    }

    /* .bss occupies zero bytes in the file, so nothing loads it and nothing zeroes it.
     * boot.s does that by hand, 8 bytes at a time, which is why both bounds must be
     * 8-byte aligned. */
    .bss : AT(ADDR(.bss) - KERNEL_VA_BASE) ALIGN(4096) {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(4096);
        __bss_end = .;
    }

    /* THE GUARD PAGE.
     *
     * One page, deliberately LEFT UNMAPPED when we build the page tables. The stack grows
     * down into it, and the MMU faults on the very first byte written past the end.
     *
     * This is the real fix for the milestone 3 incident, where a 16 KiB array on the stack
     * walked straight through .bss, .data, and .text, and the kernel executed its own
     * overwritten code. It hung for 150 seconds with no output and two of the three
     * diagnoses along the way were wrong. See notes/stack.md.
     *
     * The canary in kernel/src/stack.rs stays, but it is a mitigation: it detects damage
     * after the fact, and only if the overflow happened to write over the canary words. A
     * guard page PREVENTS the damage, precisely, at the first byte, for free at runtime.
     * That is the difference between an afternoon and an instant, legible fault. */
    . = ALIGN(4096);
    __stack_guard = .;
    . += 4096;

    /* The boot stack. Grows DOWN, so __stack_top is the HIGH address and comes AFTER the
     * reserved space. sp must be 16-byte aligned (notes/stack.md). */
    __stack_bottom = .;
    . += 0x10000;              /* 64 KiB */
    __stack_top = .;

    /* `image_size` for the arm64 Image header.
     *
     * Runs all the way to __stack_top, not just to the end of .data. The flat binary stops
     * after .data (since .bss occupies no file bytes), but this number tells the bootloader
     * how much MEMORY we occupy. Understate it and QEMU will place the device tree on top
     * of our .bss or our stack, and we corrupt it on the first push. */
    __image_end = .;
    __image_size = __image_end - __image_start;

    /DISCARD/ : {
        *(.comment)
        *(.note*)
        *(.eh_frame*)         /* stack-unwinding tables; we never unwind */
    }
}

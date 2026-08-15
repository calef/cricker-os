/* nife kernel image layout, RISC-V (milestone 20, notes/riscv-port.md).
 *
 * THE KERNEL IS LINKED HIGH AND LOADED LOW, exactly like the aarch64 script, and for the same
 * reason: the kernel lives in the high (kernel) half of the address space, so the portable
 * "kernel = high half, user = low half" model (paging::Half, the user-VA gate) holds identically on
 * both architectures (DECISIONS §17). Every symbol below is a VIRTUAL address in the Sv39 high half;
 * AT() tells the loader to place the bytes at the PHYSICAL address, which is where OpenSBI loads us.
 *
 * QEMU's `virt` enters the payload at its PHYSICAL start (the Linux RISC-V boot protocol: hart id in
 * a0, DTB in a1), so boot.s runs at low physical addresses with every absolute symbol linked high.
 * It uses PC-relative addressing until it turns Sv39 on, then jumps to the high alias of itself. See
 * boot.s and notes/riscv-port.md for how it survives that.
 *
 * KERNEL_VA_BASE is the Sv39 high-half base: bits 63:38 all one (the sign extension of bit 38 = 1),
 * so it touches only the untranslated top bits. VA = PA | KERNEL_VA_BASE is exact and reversible,
 * and a kernel virtual address has the same page-table indices as its physical address, which is
 * what lets one boot table serve as both the identity map and the high-half map.
 */

ENTRY(_start)

KERNEL_VA_BASE = 0xffffffc000000000;

/* OpenSBI drops the S-mode payload here (its fw_payload address for the virt machine). */
PHYS_START = 0x80200000;

SECTIONS
{
    . = KERNEL_VA_BASE + PHYS_START;
    __image_start = .;

    .text : AT(ADDR(.text) - KERNEL_VA_BASE) ALIGN(4096) {
        __text_start = .;
        /* _start must be first: QEMU enters the payload at its physical base. KEEP() so
         * dead-code elimination cannot delete an entry point nothing calls. */
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

    /* .bss occupies zero bytes in the file; boot.s zeroes it by hand once running high, 8 bytes at
     * a time, so both bounds are 8-byte aligned (the 4096 alignment more than covers it). */
    .bss : AT(ADDR(.bss) - KERNEL_VA_BASE) ALIGN(4096) {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(4096);
        __bss_end = .;
    }

    /* The guard page: one page, left unmapped once the fine tables are built. Until then it is
     * reserved address space beneath the stack. Same mechanism as aarch64; see notes/stack.md. */
    . = ALIGN(4096);
    __stack_guard = .;
    . += 4096;

    /* The boot stack. Grows DOWN, so __stack_top is the HIGH address, after the reserved space.
     * sp must be 16-byte aligned (the RISC-V calling convention requires it too). */
    __stack_bottom = .;
    . += 0x10000;              /* 64 KiB */
    __stack_top = .;

    /* The per-CPU secondary stacks (milestone 90), each with an unmapped page below it, exactly as
     * the boot stack has above. The slot layout (and the core count) lives in kernel/src/smp.rs;
     * this script only anchors a page-aligned region so the holes can exist, because the stacks
     * used to be in `.bss`, which is mapped wholesale. (NOLOAD): a stack needs no file bytes, and
     * nothing zeroes this region. Same shape as link-aarch64.ld; see notes/stack-high-water.md. */
    .secondary_stacks (NOLOAD) : AT(ADDR(.secondary_stacks) - KERNEL_VA_BASE) ALIGN(4096) {
        __secondary_stacks_start = .;
        *(.secondary_stacks)
        . = ALIGN(4096);
        __secondary_stacks_end = .;
    }

    __image_end = .;
    __image_size = __image_end - __image_start;

    /DISCARD/ : {
        *(.comment)
        *(.note*)
        *(.eh_frame*)         /* stack-unwinding tables; we never unwind */
        *(.riscv.attributes)  /* ISA-attribute records; not needed in the image */
    }
}

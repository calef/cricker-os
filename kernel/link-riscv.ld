/* cricker-os kernel image layout, RISC-V (milestone 20, notes/riscv-port.md).
 *
 * QEMU's `virt` machine puts RAM at 0x8000_0000. OpenSBI (the `-bios`, running in M-mode) loads
 * itself at the base and places the S-mode payload, us, at 0x8020_0000, then jumps there with the
 * hart id in a0 and the device-tree pointer in a1. QEMU boots a riscv ELF directly (`-kernel`), so
 * there is no flat-Image / text_offset dance like aarch64 has.
 *
 * DELIBERATELY LINKED LOW (identity), unlike the aarch64 script which links high and AT()-relocates
 * the load address. The kernel runs in bare mode (satp = 0) through boot and console; virtual
 * addresses equal physical ones, so KERNEL_VA_BASE is 0 for now (see arch/riscv64/mmu.rs). The
 * high-half kernel VA arrives with the Sv39 step, which is also where the paging-format work (HAL
 * leak #2) lands. Keeping boot identity-mapped means the console milestone needs no MMU at all.
 *
 * Every section boundary is exported and page-aligned for the same reason as aarch64: MMU
 * permissions are per-page, so a section sharing a page with another cannot get its own.
 */

ENTRY(_start)

/* Bare mode: virtual == physical. The Sv39 step raises this to the Sv39 high-half base. */
KERNEL_VA_BASE = 0x0;

/* OpenSBI drops the S-mode payload here. Not free to change: it is OpenSBI's fw_payload address
 * for the virt machine. */
PHYS_START = 0x80200000;

SECTIONS
{
    . = PHYS_START;
    __image_start = .;

    .text : ALIGN(4096) {
        __text_start = .;
        /* _start must be first: OpenSBI jumps to the payload's entry (the ELF entry, _start).
         * KEEP() so dead-code elimination cannot delete an entry point nothing calls. */
        KEEP(*(.text.boot))
        *(.text .text.*)
        . = ALIGN(4096);
        __text_end = .;
    }

    .rodata : ALIGN(4096) {
        __rodata_start = .;
        *(.rodata .rodata.*)
        . = ALIGN(4096);
        __rodata_end = .;
    }

    .data : ALIGN(4096) {
        __data_start = .;
        *(.data .data.*)
        . = ALIGN(4096);
        __data_end = .;
    }

    /* .bss occupies zero bytes in the file; boot.s zeroes it by hand, 8 bytes at a time, so both
     * bounds must be 8-byte aligned (the 4096 alignment more than covers it). */
    .bss : ALIGN(4096) {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(4096);
        __bss_end = .;
    }

    /* The guard page: one page, left unmapped once the MMU is on (the Sv39 step). Until then it is
     * simply reserved address space beneath the stack. Same mechanism and same reasoning as the
     * aarch64 script; see notes/stack.md. */
    . = ALIGN(4096);
    __stack_guard = .;
    . += 4096;

    /* The boot stack. Grows DOWN, so __stack_top is the HIGH address, after the reserved space.
     * sp must be 16-byte aligned (the RISC-V calling convention requires it too). */
    __stack_bottom = .;
    . += 0x10000;              /* 64 KiB */
    __stack_top = .;

    __image_end = .;
    __image_size = __image_end - __image_start;

    /DISCARD/ : {
        *(.comment)
        *(.note*)
        *(.eh_frame*)         /* stack-unwinding tables; we never unwind */
        *(.riscv.attributes)  /* ISA-attribute records; not needed in the image */
    }
}

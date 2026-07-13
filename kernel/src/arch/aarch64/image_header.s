// The arm64 Linux Image header.
//
// This 64-byte struct is the difference between QEMU treating us as an anonymous
// blob and QEMU treating us as a kernel.
//
// Milestone 1 shipped an ELF, and QEMU took its bare-metal path: copy the segments,
// set PC, release the CPU, populate no registers. We printed x0 and got zero.
//
// A flat binary carrying THIS header is recognized as an arm64 `Image`, so QEMU
// follows the **Linux boot protocol** instead: it generates a device tree, places it
// in memory, and hands us a pointer to it in x0. Which is what we wanted all along.
//
// The Pi wants a flat image too (`kernel8.img`), so this is on the road to the port
// rather than a detour from it. See notes/portability.md and notes/boot-protocol.md.
//
// Layout, from Linux's Documentation/arch/arm64/booting.rst:
//
//   offset  size  field
//   0x00    4     code0        first instruction. The image entry point IS offset 0,
//   0x04    4     code1        so this has to branch over the rest of the header.
//   0x08    8     text_offset  where to load us, as an offset from the base of RAM
//   0x10    8     image_size   how much memory we occupy, INCLUDING .bss and stack
//   0x18    8     flags        endianness, page size, placement
//   0x20    8     res2         reserved
//   0x28    8     res3         reserved
//   0x30    8     res4         reserved
//   0x38    4     magic        0x644d5241, which is "ARM\x64" little-endian
//   0x3c    4     res5         reserved

.section ".text.header", "ax"
.global _start

_start:
    // code0. The entry point is the first byte of the image, so the very first thing
    // we execute is a jump over the 60 bytes of data that follow.
    //
    // Note what this does NOT do: touch x0. QEMU has put the device tree pointer
    // there, and it survives all the way to `_boot`, which stashes it.
    b       _boot

    .long   0                       // code1: unused

    .quad   0x80000                 // text_offset: 512 KiB into RAM.
                                    // RAM base 0x4000_0000 + 0x8_0000 = 0x4008_0000,
                                    // which is exactly where link.ld puts us. The two
                                    // numbers have to agree or nothing works.

    .quad   __image_size            // image_size: computed by link.ld as
                                    // __stack_top - __image_start, so it covers .bss
                                    // AND the boot stack. This matters: it is how the
                                    // bootloader knows not to drop the device tree on
                                    // top of our stack.

    .quad   (1 << 1)                // flags:
                                    //   bit 0    = 0  little-endian
                                    //   bits 1-2 = 1  4 KiB pages
                                    //   bit 3    = 0  load at a 2 MiB-aligned base
                                    //                 near the start of DRAM

    .quad   0                       // res2
    .quad   0                       // res3
    .quad   0                       // res4

    .long   0x644d5241              // magic. Spelled as a number rather than
                                    // .ascii "ARM\x64" because escape handling varies
                                    // between assemblers and this cannot be wrong.

    .long   0                       // res5

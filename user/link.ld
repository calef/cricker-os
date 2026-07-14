/* A user program's layout.
 *
 * The kernel is at 0xffff_..., in TTBR1. A user program is at 0x0000_..., in TTBR0, and the
 * hardware picks between them from bits 63:48 of the address. So a user program can be linked
 * at a low address with no fear of colliding with anything: the kernel is not *in* its address
 * space in any sense it can observe. See notes/higher-half.md.
 *
 * PHDRS is explicit, and that is the point. Left to itself the linker will happily put .text
 * and .rodata in one PT_LOAD, sharing a page, and our loader refuses that (Error::SegmentsOverlap).
 * More importantly, being explicit is what lets us hand the kernel three segments with three
 * different permissions, so that W^X is a property of the file and not just of our good manners.
 */

ENTRY(_start)

PHDRS
{
    text   PT_LOAD FLAGS(5);   /* R+X  */
    rodata PT_LOAD FLAGS(4);   /* R    */
    data   PT_LOAD FLAGS(6);   /* R+W  */
}

SECTIONS
{
    . = 0x400000;

    .text : ALIGN(4096) {
        *(.text._start)         /* the entry point goes first, so `entry` is legible */
        *(.text .text.*)
    } :text

    .rodata : ALIGN(4096) {
        *(.rodata .rodata.*)
    } :rodata

    .data : ALIGN(4096) {
        *(.data .data.*)
    } :data

    /* .bss is in the `data` segment but occupies NO space in the file. That is what makes
     * p_memsz > p_filesz, and the loader has to zero the difference. See crates/elf. */
    .bss : {
        *(.bss .bss.*)
        *(COMMON)
    } :data

    /DISCARD/ : { *(.comment) *(.note*) *(.eh_frame*) }
}

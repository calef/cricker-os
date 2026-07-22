// The kernel's real entry, reached by the `b _boot` in image_header.s.
//
// # We are linked HIGH but loaded LOW
//
// link.ld places the kernel at 0xffff_0000_4008_0000 (virtual) but tells the loader to put
// the bytes at 0x4008_0000 (physical). So every absolute address the compiler baked into
// this binary is a virtual address that **does not work yet**, and the code that turns the
// MMU on is inside that binary.
//
// Two facts get us out.
//
// ## 1. `adrp` is PC-relative, so it yields PHYSICAL addresses right now
//
//     adrp x0, __stack_top          // x0 = (PC & ~0xfff) + linker_offset
//
// The linker computes `linker_offset` from *virtual* addresses. But PC is currently a
// *physical* address, and VA - PA is a constant (0xffff_0000_0000_0000), so the two
// differences cancel and we get the physical address of the symbol. Free of charge.
//
// This is why nothing below uses `ldr x0, =symbol` until the MMU is on: a literal pool holds
// the absolute VA, which is exactly the thing that doesn't work yet. (Literal pools holding
// *constants* are fine; the load itself is PC-relative.)
//
// ## 2. Bits 63:48 are not translated, so ONE table serves as both maps
//
// A virtual address `PA + 0xffff_0000_0000_0000` has the same L0/L1/L2/L3 indices as `PA`
// itself, because the index only ever reads bits 47:12 (see notes/page-tables.md). So the
// identity map and the high-half map are **the same table contents**, and we simply point
// TTBR0 and TTBR1 at the same root.
//
// That is the whole trick, and it's why the boot tables below are 2 pages rather than a
// careful dance.
//
// # The sequence
//
//   1. park cores 1..n
//   2. set up a PHYSICAL stack (adrp)
//   3. zero .bss (adrp)
//   4. build a crude 1 GiB-block map: device @ 0, RAM @ 0x4000_0000
//   5. TTBR0 = TTBR1 = that map
//   6. MMU on. We are still executing at the physical address, via TTBR0's identity map.
//   7. sp = the HIGH virtual address of the stack
//   8. jump to kernel_main's HIGH virtual address
//
// After step 8, everything is virtual and Rust never has to think about this again.
//
// See notes/higher-half.md.

.section ".text.boot", "ax"
.global _boot

// The boot map is deliberately COARSE and PERMISSIVE: two 1 GiB blocks, and the RAM one is
// executable everywhere. It exists to survive the next twenty instructions, nothing more.
// mmu.rs immediately replaces it with a fine-grained map that enforces W^X and punches out
// the guard page. Linux does exactly this, for exactly this reason.
//
//   block @ 0x0000_0000, DEVICE:  AF | PXN | UXN | block
//   block @ 0x4000_0000, NORMAL:  AF | SH_inner | AttrIdx=1 | UXN | block   (PXN clear: we
//                                 must be able to execute our own .text)
.equ BOOT_DEVICE_BLOCK, 0x0060000000000401
.equ BOOT_NORMAL_BLOCK, 0x0040000040000705

// MAIR: slot 0 = Device-nGnRnE (0x00), slot 1 = Normal write-back (0xff).
.equ BOOT_MAIR,         0xff00

// TCR: T0SZ=T1SZ=16 (48-bit VAs), 4 KiB granule both halves, inner-shareable write-back
// table walks, both TTBRs enabled.
//
// **TG0 and TG1 use DIFFERENT ENCODINGS for 4 KiB**: TG0=0b00, TG1=0b10. That is not a typo
// below, it is the architecture.
.equ BOOT_TCR,          0xb5103510

_boot:
    // QEMU handed us the device tree pointer in x0, and it is a PHYSICAL address. Keep it;
    // kernel_main converts it.
    mov     x19, x0

    // Park every core but core 0 (DECISIONS.md §6).
    mrs     x0, mpidr_el1
    and     x0, x0, #0xff
    cbnz    x0, park

    // A physical stack. adrp+add yields the PA; see the header comment.
    adrp    x0, __stack_top
    add     x0, x0, :lo12:__stack_top
    mov     sp, x0

    // Zero .bss by hand. Nobody loaded it (it occupies no bytes in the file) and there is no
    // C runtime here. The boot page tables live in .bss, so this also zeroes them, which is
    // load-bearing: a page table full of whatever was in RAM is a set of pointers to nowhere,
    // followed at speed.
    adrp    x0, __bss_start
    add     x0, x0, :lo12:__bss_start
    adrp    x1, __bss_end
    add     x1, x1, :lo12:__bss_end
1:  cmp     x0, x1
    b.hs    2f
    str     xzr, [x0], #8
    b       1b
2:

    // --- build the boot page tables ---

    adrp    x0, boot_l0
    add     x0, x0, :lo12:boot_l0       // x0 = PA of the L0 table
    adrp    x1, boot_l1
    add     x1, x1, :lo12:boot_l1       // x1 = PA of the L1 table

    // L0[0] -> L1.  A table descriptor is just the address with bits[1:0] = 0b11.
    orr     x2, x1, #3
    str     x2, [x0]

    // L1[0]: 1 GiB block at 0x0000_0000, device memory. Covers the PL011 at 0x0900_0000.
    // Without this the machine goes silent the instant the MMU comes on.
    ldr     x2, =BOOT_DEVICE_BLOCK
    str     x2, [x1]

    // L1[1]: 1 GiB block at 0x4000_0000, normal memory, executable. This is where we are.
    //
    // NOTE: hardcoded for QEMU `virt`, whose RAM starts at 0x4000_0000. The Raspberry Pi
    // puts RAM at 0, and this is one of the handful of places that port will have to touch.
    ldr     x2, =BOOT_NORMAL_BLOCK
    str     x2, [x1, #8]

    // --- turn the MMU on ---

    ldr     x2, =BOOT_MAIR
    msr     mair_el1, x2

    ldr     x2, =BOOT_TCR
    mrs     x3, id_aa64mmfr0_el1
    and     x3, x3, #0xf                // PARange: how many physical address bits this CPU
    orr     x2, x2, x3, lsl #32         // actually has. Claiming more is UNPREDICTABLE.
    msr     tcr_el1, x2

    // BOTH registers, SAME table. See fact 2 in the header comment: the identity map and the
    // high-half map have identical contents, because bits 63:48 never reach an index.
    msr     ttbr0_el1, x0
    msr     ttbr1_el1, x0

    // Every write above must be visible to the page-table walker, which is a separate
    // observer, before it can possibly walk them.
    dsb     sy
    isb
    tlbi    vmalle1                     // throw away any stale translations
    dsb     ish
    isb

    // The point of no return. The instruction fetched AFTER this one goes through the MMU.
    // We survive it because TTBR0 identity-maps the page we are executing from.
    mrs     x2, sctlr_el1
    orr     x2, x2, #(1 << 0)           // M: MMU enable
    orr     x2, x2, #(1 << 2)           // C: data cache
    orr     x2, x2, #(1 << 12)          // I: instruction cache
    msr     sctlr_el1, x2
    isb

    // --- we are now running with paging on, still at the physical address ---
    //
    // From here, `ldr x, =symbol` finally means what it says: the literal pool holds the
    // virtual address, and TTBR1 maps it.

    ldr     x0, =__stack_top            // the HIGH stack
    mov     sp, x0

    mov     x0, x19                     // the device tree (still a physical address)
    ldr     x1, =kernel_main            // the HIGH entry point
    br      x1                          // and we are in the high half forever

park:
    // wfi, not wfe: QEMU idles the host thread on wfi and merely spins on wfe. A parked core
    // that burns 100% of a host CPU is not parked. See notes/qemu.md.
    wfi
    b       park

// --- secondary core entry (SMP step 2, DECISIONS.md §11) ---
//
// PSCI CPU_ON starts a secondary HERE, at this PHYSICAL address, with the MMU off, at EL1,
// exactly the way QEMU started core 0 at `_boot`. x0 holds the context word core 0 passed to
// CPU_ON: this core's HIGH-VA stack top (unusable until the MMU is on, which is fine, nothing
// below touches the stack before then).
//
// The crucial difference from `_boot`: the page tables already exist. Core 0 built `boot_l0`
// and it is still sitting in .bss, so we do NOT rebuild it. We only replay the MMU-enable
// (fact 1 in the header comment gets us the table's PA with `adrp`) and jump to the high half.
.global secondary_boot
secondary_boot:
    mov     x19, x0                     // stash the stack-top VA for after the MMU is on

    // Point at the boot tables core 0 already built. adrp yields the PA (PC-relative, MMU off).
    adrp    x0, boot_l0
    add     x0, x0, :lo12:boot_l0

    ldr     x2, =BOOT_MAIR
    msr     mair_el1, x2

    ldr     x2, =BOOT_TCR
    mrs     x3, id_aa64mmfr0_el1
    and     x3, x3, #0xf                // PARange, per-core; claiming more than we have is UB
    orr     x2, x2, x3, lsl #32
    msr     tcr_el1, x2

    // Both registers, same table, same reason as core 0 (header fact 2).
    msr     ttbr0_el1, x0
    msr     ttbr1_el1, x0

    dsb     sy
    isb
    tlbi    vmalle1
    dsb     ish
    isb

    mrs     x2, sctlr_el1
    orr     x2, x2, #(1 << 0)           // M: MMU
    orr     x2, x2, #(1 << 2)           // C: data cache
    orr     x2, x2, #(1 << 12)          // I: instruction cache
    msr     sctlr_el1, x2
    isb

    // Paging on. The high-VA stack top in x19 resolves now (TTBR1 maps the kernel image, and
    // the coarse boot map covers all of low RAM where the image lives).
    mov     sp, x19

    // This core's id is MPIDR affinity 0: QEMU `virt` numbers cores 0..N there.
    mrs     x0, mpidr_el1
    and     x0, x0, #0xff

    ldr     x1, =secondary_main         // the HIGH entry, x0 = cpu id
    br      x1

// The boot page tables. In .bss, so the zeroing loop above clears them for free.
.section ".bss", "aw", @nobits
.balign 4096
boot_l0:
    .skip 4096
boot_l1:
    .skip 4096

// The aarch64 exception vector table.
//
// VBAR_EL1 holds the base address of this table. When an exception fires, the
// hardware computes its target by adding a FIXED offset determined by two things:
// where the exception came from, and what kind it is. So the shape of this table is
// dictated by silicon, not by us.
//
//   offset  source                    kind
//   0x000   Current EL, SP_EL0        Synchronous
//   0x080                             IRQ
//   0x100                             FIQ
//   0x180                             SError
//   0x200   Current EL, SP_ELx        Synchronous   <- a kernel bug lands HERE
//   0x280                             IRQ           <- the timer, milestone 5
//   0x300                             FIQ
//   0x380                             SError
//   0x400   Lower EL, AArch64         Synchronous   <- `svc` lands HERE, milestone 7
//   0x480                             IRQ
//   0x500                             FIQ
//   0x580                             SError
//   0x600   Lower EL, AArch32         Synchronous
//   0x680                             IRQ
//   0x700                             FIQ
//   0x780                             SError
//
// Each entry is exactly 128 bytes: 32 instructions. That is enough to save state and
// branch, and not enough to do real work. The constraint is why every aarch64 kernel
// on earth looks nearly identical right here.
//
// See notes/exceptions.md.

// Save the interrupted CPU state onto the kernel stack.
//
// This layout is a CONTRACT with `struct TrapFrame` in exceptions.rs. Reorder a store
// here and the Rust side silently reads the wrong field. There is a compile-time size
// assertion over there, which catches half of the ways to get this wrong.
.macro SAVE_CONTEXT
    sub     sp,  sp,  #272

    stp     x0,  x1,  [sp, #16 * 0]
    stp     x2,  x3,  [sp, #16 * 1]
    stp     x4,  x5,  [sp, #16 * 2]
    stp     x6,  x7,  [sp, #16 * 3]
    stp     x8,  x9,  [sp, #16 * 4]
    stp     x10, x11, [sp, #16 * 5]
    stp     x12, x13, [sp, #16 * 6]
    stp     x14, x15, [sp, #16 * 7]
    stp     x16, x17, [sp, #16 * 8]
    stp     x18, x19, [sp, #16 * 9]
    stp     x20, x21, [sp, #16 * 10]
    stp     x22, x23, [sp, #16 * 11]
    stp     x24, x25, [sp, #16 * 12]
    stp     x26, x27, [sp, #16 * 13]
    stp     x28, x29, [sp, #16 * 14]

    // x1, x2 and x3 are already safely on the stack, so they are ours to scribble on.
    mrs     x1,  elr_el1            // where the interrupted code will resume
    mrs     x2,  spsr_el1           // the processor state it was in

    // SP_EL0: the USER stack pointer, and it is a physically different register.
    //
    // At EL1 we run with SPSel=1, so `sp` above means SP_EL1, the kernel stack. The
    // hardware switched to it on the way in and never touched SP_EL0, so the user's
    // stack pointer is still sitting there, intact, and nothing above saved it.
    //
    // It survives an exception on its own. It does NOT survive a context switch to
    // another user thread, which would find its own SP_EL0 already spent. So it belongs
    // in the frame, where it travels with the thread.
    //
    // (This costs nothing: it lands in the padding word the frame already had, so
    // TrapFrame is still 272 bytes. See exceptions.rs.)
    mrs     x3,  sp_el0

    stp     x30, x1,  [sp, #16 * 15]
    stp     x2,  x3,  [sp, #16 * 16]
.endm

// Put it all back, exactly as it was.
//
// Note the order: we pull ELR and SPSR out into scratch registers and write them to
// the system registers FIRST, then overwrite the scratch registers with their real
// saved values. Doing it the other way round would corrupt x1 and x2.
.macro RESTORE_CONTEXT
    ldp     x2,  x3,  [sp, #16 * 16]
    ldp     x30, x1,  [sp, #16 * 15]

    msr     spsr_el1, x2
    msr     elr_el1,  x1            // the handler may have CHANGED this. See exceptions.rs.
    msr     sp_el0,   x3            // and the user's stack pointer goes back where it lives

    ldp     x0,  x1,  [sp, #16 * 0]
    ldp     x2,  x3,  [sp, #16 * 1]
    ldp     x4,  x5,  [sp, #16 * 2]
    ldp     x6,  x7,  [sp, #16 * 3]
    ldp     x8,  x9,  [sp, #16 * 4]
    ldp     x10, x11, [sp, #16 * 5]
    ldp     x12, x13, [sp, #16 * 6]
    ldp     x14, x15, [sp, #16 * 7]
    ldp     x16, x17, [sp, #16 * 8]
    ldp     x18, x19, [sp, #16 * 9]
    ldp     x20, x21, [sp, #16 * 10]
    ldp     x22, x23, [sp, #16 * 11]
    ldp     x24, x25, [sp, #16 * 12]
    ldp     x26, x27, [sp, #16 * 13]
    ldp     x28, x29, [sp, #16 * 14]

    add     sp,  sp,  #272
.endm

// One table entry. Save everything, tell Rust which of the sixteen slots fired, and
// let Rust decide what it means.
.macro VECTOR_ENTRY index
    .balign 0x80
    SAVE_CONTEXT
    mov     x0,  sp                 // arg 0: &mut TrapFrame  (AAPCS64: first arg in x0)
    mov     x1,  #\index            // arg 1: which slot
    bl      exception_dispatch
    b       exception_restore
.endm

.section ".text.exceptions", "ax"

// The hardware requires 2048-byte alignment. 16 entries x 128 bytes = 2048.
.balign 0x800
.global exception_vectors
exception_vectors:
    VECTOR_ENTRY 0                  // Current EL, SP_EL0
    VECTOR_ENTRY 1
    VECTOR_ENTRY 2
    VECTOR_ENTRY 3

    VECTOR_ENTRY 4                  // Current EL, SP_ELx   (kernel bugs live here)
    VECTOR_ENTRY 5
    VECTOR_ENTRY 6
    VECTOR_ENTRY 7

    VECTOR_ENTRY 8                  // Lower EL, AArch64    (userspace, milestone 7)
    VECTOR_ENTRY 9
    VECTOR_ENTRY 10
    VECTOR_ENTRY 11

    VECTOR_ENTRY 12                 // Lower EL, AArch32    (we will never support this)
    VECTOR_ENTRY 13
    VECTOR_ENTRY 14
    VECTOR_ENTRY 15

// `eret` is the counterpart to the exception: it restores the processor state from
// SPSR_EL1 and jumps to ELR_EL1, in one instruction. That includes DROPPING THE
// EXCEPTION LEVEL, because SPSR_EL1 carries the level to return to.
//
// Which is the whole of milestone 7a, and it is why there is so little new assembly here.
.global exception_restore
exception_restore:
    RESTORE_CONTEXT
    eret

// ENTER USERSPACE, by returning from an exception that never happened.
//
//   x0 = a TrapFrame we FABRICATED, with SPSR = EL0t and ELR = the user's entry point.
//
// There is no "drop to EL0" instruction. There is only `eret`, which restores whatever
// SPSR_EL1 says. So we do not need a new way down: we need a fake way back.
//
// This is the second time this project has pulled the same trick. `Thread::spawn` fakes a
// `switch_to` frame so that the `ret` which RESUMES a thread also STARTS one
// (notes/threads.md). Here we fake a TrapFrame so that the `eret` which RETURNS to
// interrupted code also ENTERS userspace. Both times, the "start" path turned out to be the
// "resume" path with a forged frame, and no new code at all.
//
// After the eret, SP_EL1 = x0 + 272: exactly where the next SAVE_CONTEXT will build its
// frame when the user traps back in. The symmetry is not a coincidence, it is the contract.
.global enter_userspace
enter_userspace:
    mov     sp,  x0
    b       exception_restore

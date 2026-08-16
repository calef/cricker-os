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
    // MASK INTERRUPTS FIRST, and this is not belt-and-braces: it closes a real race that
    // cost a rare, spectacular failure (milestone 22 phase B.2 found it).
    //
    // SPSR_EL1 and ELR_EL1 are the eret's *only* record of where to go and at what level,
    // and they are single copies the hardware overwrites on every exception. Between the
    // `msr spsr_el1` below and the `eret` at the end there is a window: an interrupt taken
    // in it saves its own EL1 state into those two registers, the nested handler's own
    // RESTORE_CONTEXT puts the EL1 state back (SPSR = EL1h), and our `eret` then returns to
    // the user's entry point AT EL1. The symptom is an instruction abort at the user's code
    // address taken "from the same EL", which reads as impossible until you find this.
    //
    // On a normal trap return the window is already closed, because taking the exception set
    // PSTATE.DAIF. The exposed path is `enter_userspace`, which branches in here from
    // ordinary kernel code with interrupts ENABLED, so every first entry to a process had a
    // two-instruction window. Rare per entry, and the suite entered enough processes to hit
    // it about one run in four.
    //
    // Masking costs nothing at the far end: the `eret` restores DAIF from SPSR, so the state
    // we return to is unchanged. It is in the macro rather than in `enter_userspace` so that
    // any future path reaching here with interrupts on is covered by construction.
    msr     daifset, #0xf

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

// RUN THE HANDLER ON THIS CORE'S INTERRUPT STACK (milestone 124).
//
//   x0 = &mut TrapFrame      x1 = vector index      x2 = the stack to run on, or 0 to stay
//
// The frame is already built, on whatever stack the trap interrupted, and it has to be: a preempted
// thread's frame must still be there when that thread runs again, and a per-core stack cannot
// promise that. What moves is everything ABOVE the frame, which is the part that made a preemption
// cost the interrupted thread ~2.3 KiB at its deepest instant. See kernel/src/interrupt_stack.rs.
//
// Rust decides whether to switch (`interrupt_stack::top_for_trap`) and hands the answer down in x2,
// so the policy is readable in Rust and only the stack pointer move is assembly.
//
// x19 holds the interrupted `sp` across the call, because a callee-saved register cannot be
// clobbered by the handler and needs no slot on either stack. Its own save goes on the interrupted
// stack, which costs that stack 32 bytes: the price of the switch, against the kilobytes it moves.
//
// `exception_body` returns a bool in w0 saying whether the caller owes a deferred `schedule()`, and
// nothing here touches x0.
.global dispatch_on_interrupt_stack
dispatch_on_interrupt_stack:
    stp     x29, x30, [sp, #-32]!
    str     x19, [sp, #16]
    mov     x29, sp

    cbz     x2,  1f                 // 0: stay on this stack (from EL0, pre-init, or nesting)
    mov     x19, sp
    mov     sp,  x2
    bl      exception_body
    mov     sp,  x19                // back to the interrupted stack BEFORE anything can switch away
    b       2f
1:  bl      exception_body

2:  ldr     x19, [sp, #16]
    ldp     x29, x30, [sp], #32
    ret

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

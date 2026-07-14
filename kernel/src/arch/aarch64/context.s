// The context switch. Fifteen instructions, and one of them is doing something strange.
//
// # Why this is so much smaller than the trap frame
//
// `vectors.s` saves **all 31** general registers, because an exception can land between any
// two instructions and the interrupted code has no idea it happened.
//
// This is different. `switch_to` is an ordinary **function call**, and the aarch64 calling
// convention (AAPCS64) already says the caller must assume `x0`–`x18` are destroyed by any
// call it makes. So we do not have to save them: the compiler already spilled anything it
// cared about.
//
// What we must preserve is exactly what AAPCS64 promises a callee will preserve:
//
//   x19–x28   callee-saved general registers
//   x29       frame pointer
//   x30       link register — **where to return to**
//   sp        the stack pointer
//
// Twelve registers and a stack pointer, instead of thirty-three. That is not an optimization
// we invented; it falls out of the calling convention, and it is why real kernels have both a
// trap frame *and* a much cheaper voluntary switch.
//
// (No floating-point registers, because the kernel is built `softfloat`. See notes/aarch64.md
// — that decision, made at milestone 1 for a completely different reason, means `d8`–`d15`
// simply do not appear here.)
//
// # The strange instruction is the last one
//
//     ret
//
// It returns into `x30`. And by that point, `x30` has been loaded from **the other thread's
// stack**. So this `ret` does not go back to the caller. **It resumes a different thread, at
// the point where that thread last called `switch_to`, possibly a long time ago.**
//
// That is the whole trick. A context switch is a function call that returns somewhere else.
//
// See notes/threads.md.

.section ".text", "ax"

// switch_to(prev_context: *mut *mut Context, next_context: *mut Context)
//
//   x0 = where to STORE our stack pointer (i.e. `&mut prev.context`)
//   x1 = the stack pointer to RESTORE     (i.e. `next.context`)
//
// AAPCS64 puts the first two arguments in x0 and x1. See notes/registers.md.
.global switch_to
switch_to:
    // Push the callee-saved registers onto OUR stack. 12 registers, 96 bytes, which is a
    // multiple of 16 so `sp` stays aligned (notes/stack.md).
    sub     sp,  sp,  #96
    stp     x19, x20, [sp, #0]
    stp     x21, x22, [sp, #16]
    stp     x23, x24, [sp, #32]
    stp     x25, x26, [sp, #48]
    stp     x27, x28, [sp, #64]
    stp     x29, x30, [sp, #80]

    // Remember where we put them. THIS is the entire saved state of a thread: a single
    // stack pointer. Everything else is on the stack it points at.
    mov     x2,  sp
    str     x2,  [x0]

    // And now we are running on somebody else's stack.
    mov     sp,  x1

    // Pop THEIR callee-saved registers. These were pushed by their call to switch_to, whenever
    // that was.
    ldp     x19, x20, [sp, #0]
    ldp     x21, x22, [sp, #16]
    ldp     x23, x24, [sp, #32]
    ldp     x25, x26, [sp, #48]
    ldp     x27, x28, [sp, #64]
    ldp     x29, x30, [sp, #80]
    add     sp,  sp,  #96

    // x30 now holds the OTHER thread's return address. This does not go back to our caller.
    ret

// Where a brand-new thread begins.
//
// A new thread has never called `switch_to`, so it has no saved registers to restore. We
// **fake** them: `Context::new` writes a frame onto the fresh stack with `x30` pointing here,
// so the `ret` above lands on this instruction the first time the thread is scheduled.
//
// The thread's closure comes in `x19` — a callee-saved register, chosen precisely because
// `switch_to` restores it for us on the way in.
.global thread_trampoline
thread_trampoline:
    // ENABLE INTERRUPTS.
    //
    // Easy to miss, and fatal if you do. We usually arrive here from inside the timer IRQ
    // handler, which the hardware entered with IRQs masked. Every *other* thread gets its
    // interrupt state back from `eret` restoring SPSR_EL1 — but a brand-new thread has no
    // SPSR to restore, because it was never interrupted. It has never run at all.
    //
    // Without this, the first thread we spawn runs with interrupts masked forever: it can
    // never be preempted, and if it loops, the machine is gone. Which would be a cooperative
    // scheduler with extra steps, and an ironic way to lose this particular argument.
    msr     daifclr, #2

    mov     x0,  x19            // the boxed closure
    bl      thread_entry        // extern "C" fn(*mut ()) -> !  — never returns

    // thread_entry is `-> !`. If we somehow get here, stop rather than run off into whatever
    // happens to be next in memory.
1:  wfi
    b       1b

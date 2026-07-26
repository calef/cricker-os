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
// The thread's closure lives on its own stack, just above the faked frame; its address comes
// in `x19` and the (monomorphized) function that knows how to call it comes in `x20` — both
// callee-saved registers, chosen precisely because `switch_to` restores them on the way in.
// Two registers because the closure's concrete type was erased: the address alone says where,
// the caller says how. See Thread::spawn (milestone 14 phase B.3).
.global thread_trampoline
thread_trampoline:
    // We arrive here with IRQs masked (from the `schedule()` that switched to us, or the timer
    // IRQ it ran inside). A brand-new thread has no SPSR to restore its interrupt state from, so
    // it must unmask by hand — but **`thread_entry` does that, AFTER `finish_switch`**, not here.
    //
    // Enabling IRQs *here*, before `finish_switch`, was a real, intermittent hang. `finish_switch`
    // reaps the predecessor and completes any wake it deferred, reading this core's `switched_from`.
    // A timer IRQ landing between an early unmask and `finish_switch` would run `schedule()`, which
    // overwrites `switched_from` with *us*, and the predecessor is stranded: its `on_cpu` never
    // clears, so every future wake for it is deferred forever. `finish_switch` must run masked.
    mov     x0,  x19            // the closure, on this thread's own stack
    mov     x1,  x20            // extern "C" fn(*mut ()): the closure's monomorphized caller
    bl      thread_entry        // extern "C" fn(*mut (), fn(*mut ())) -> !  — never returns

    // thread_entry is `-> !`. If we somehow get here, stop rather than run off into whatever
    // happens to be next in memory.
1:  wfi
    b       1b

// Where a brand-new USER thread begins (milestone 19c.3): the EL0 mirror of thread_trampoline.
//
// A thread retyped and started via the TCB object surface begins life at EL0, not in a kernel
// closure. `Thread::arm_for_start` faked a switch frame whose x30 points here, with x19 = the
// EL0 entry address and x20 = the user stack pointer, both restored by `switch_to` on the way
// in. We enable interrupts (a brand-new thread has no SPSR to restore, exactly as above) and
// tail-call the Rust half, which reaps our predecessor and drops to EL0.
.global user_entry_trampoline
user_entry_trampoline:
    // No early unmask here either (see thread_trampoline for the hang it caused). `finish_switch`
    // runs masked inside `user_thread_entry`; the `eret` that drops us to EL0 restores an SPSR
    // with IRQs enabled, so the EL0 thread is preemptible from its first instruction.
    mov     x0,  x19            // the EL0 entry address
    mov     x1,  x20            // the user stack pointer
    mov     x2,  x21            // the child's initial x0 (19d)
    mov     x3,  x22            // ...x1 (19e)
    mov     x4,  x23            // ...x2
    bl      user_thread_entry   // extern "C" fn(u64, u64, u64, u64, u64) -> !  — never returns
1:  wfi
    b       1b

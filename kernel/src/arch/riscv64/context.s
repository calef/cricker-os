# The context switch and the two first-run trampolines, RISC-V. The asm half of context.rs.
#
# This mirrors arch/aarch64/context.s exactly in structure; only the register set differs. A
# voluntary switch is an ordinary function call, so the RISC-V calling convention already lets the
# caller assume the temporaries (t0-t6, a0-a7) are clobbered. We save only what the convention
# says a callee preserves: ra (the return address, i.e. WHERE TO RETURN) and s0-s11. sp is the
# saved state itself, so it is not stored in the frame; it is what `switch_to` swaps.
#
# The trick is identical to aarch64: `ret` returns into `ra`, and by that point `ra` was loaded
# from the OTHER thread's stack, so the return lands in a different thread. See notes/threads.md.
#
# UNPROVEN until RISC-V boots (the console step and beyond exercise it). It assembles and links now,
# which is all the compile milestone claims.

.section ".text", "ax"

# switch_to(prev_context: *mut *mut Context, next_context: *mut Context)
#   a0 = where to STORE our stack pointer (&mut prev.context)
#   a1 = the stack pointer to RESTORE     (next.context)
# The RISC-V calling convention puts the first two arguments in a0 and a1.
.global switch_to
switch_to:
    # Push the callee-saved registers. 112 bytes: 13 registers plus 8 bytes of padding, a multiple
    # of 16 so sp stays aligned. The layout matches `struct Context` field for field.
    addi sp, sp, -112
    sd   ra,   0(sp)
    sd   s0,   8(sp)
    sd   s1,  16(sp)
    sd   s2,  24(sp)
    sd   s3,  32(sp)
    sd   s4,  40(sp)
    sd   s5,  48(sp)
    sd   s6,  56(sp)
    sd   s7,  64(sp)
    sd   s8,  72(sp)
    sd   s9,  80(sp)
    sd   s10, 88(sp)
    sd   s11, 96(sp)
    # offset 104 is the padding field; nothing is written there.

    # Remember where we put them: THIS is the entire saved state of a thread, a single sp.
    sd   sp, 0(a0)

    # And now we run on somebody else's stack.
    mv   sp, a1

    # Pop THEIR callee-saved registers, pushed by their own call to switch_to, whenever that was
    # (or faked by Context::for_*_thread for a thread that has never run).
    ld   ra,   0(sp)
    ld   s0,   8(sp)
    ld   s1,  16(sp)
    ld   s2,  24(sp)
    ld   s3,  32(sp)
    ld   s4,  40(sp)
    ld   s5,  48(sp)
    ld   s6,  56(sp)
    ld   s7,  64(sp)
    ld   s8,  72(sp)
    ld   s9,  80(sp)
    ld   s10, 88(sp)
    ld   s11, 96(sp)
    addi sp, sp, 112

    # ra now holds the OTHER thread's return address. This does not go back to our caller.
    ret

# Where a brand-new KERNEL thread begins. Context::for_kernel_thread faked a frame whose ra points
# here, with s0 = the closure pointer and s1 = the monomorphized caller, both restored by switch_to
# on the way in. We hand them to the portable `thread_entry`, which never returns.
.global thread_trampoline
thread_trampoline:
    # We arrive with interrupts masked (as on aarch64); the portable `thread_entry` unmasks after
    # `finish_switch`, never here. See arch/aarch64/context.s for the hang an early unmask caused.
    mv   a0, s0            # the closure, on this thread's own stack
    mv   a1, s1            # extern "C" fn(*mut ()): the closure's monomorphized caller
    call thread_entry      # extern "C" fn(*mut (), fn(*mut ())) -> !  -- never returns
1:  wfi
    j    1b

# Where a brand-new USER thread begins: the U-mode mirror of thread_trampoline.
# Context::for_user_thread faked a frame whose ra points here, with s0 = the U-mode entry, s1 = the
# user sp, and s2..s4 = the child's initial a0..a2. The portable `user_thread_entry` reaps our
# predecessor and drops to U-mode.
.global user_entry_trampoline
user_entry_trampoline:
    mv   a0, s0            # the U-mode entry address
    mv   a1, s1            # the user stack pointer
    mv   a2, s2            # the child's initial a0
    mv   a3, s3            # ...a1
    mv   a4, s4            # ...a2
    call user_thread_entry # extern "C" fn(u64, u64, u64, u64, u64) -> !  -- never returns
1:  wfi
    j    1b

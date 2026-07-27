# The S-mode trap vector: save the trap frame, call the Rust dispatcher, restore, sret.
#
# This is the RISC-V analog of aarch64's vectors.s SAVE_CONTEXT / exception_dispatch /
# RESTORE_CONTEXT. RISC-V has a single trap entry (stvec), not a 16-slot table; the cause is in
# scause and the dispatcher fans out on it.
#
# First cut: traps taken in S-mode, on the current kernel stack. There is no sscratch stack switch
# yet, because that is only needed for traps taken from U-mode (a user thread's kernel stack), which
# arrive with the user path. So this proves the mechanism (stvec, save, dispatch, restore, sret) and
# serves kernel-side traps (a breakpoint self-test, and faults); the U-mode entry extends it.
#
# The frame layout is `struct TrapFrame` in exceptions.rs: x[0..32] then sepc, scause, stval,
# sstatus. 36 u64 = 288 bytes. x[0] (the hardwired zero) and x[2] (sp) are handled specially.

.section ".text", "ax"
.balign 4                       # stvec direct mode needs the vector 4-byte aligned (low 2 bits = 0)
.global trap_entry
trap_entry:
    addi    sp, sp, -288        # make room for the frame on the current (kernel) stack

    # General registers. x0 is always zero; x2 (sp) is saved specially below.
    sd      x1,  1*8(sp)
    sd      x3,  3*8(sp)
    sd      x4,  4*8(sp)
    sd      x5,  5*8(sp)
    sd      x6,  6*8(sp)
    sd      x7,  7*8(sp)
    sd      x8,  8*8(sp)
    sd      x9,  9*8(sp)
    sd      x10, 10*8(sp)
    sd      x11, 11*8(sp)
    sd      x12, 12*8(sp)
    sd      x13, 13*8(sp)
    sd      x14, 14*8(sp)
    sd      x15, 15*8(sp)
    sd      x16, 16*8(sp)
    sd      x17, 17*8(sp)
    sd      x18, 18*8(sp)
    sd      x19, 19*8(sp)
    sd      x20, 20*8(sp)
    sd      x21, 21*8(sp)
    sd      x22, 22*8(sp)
    sd      x23, 23*8(sp)
    sd      x24, 24*8(sp)
    sd      x25, 25*8(sp)
    sd      x26, 26*8(sp)
    sd      x27, 27*8(sp)
    sd      x28, 28*8(sp)
    sd      x29, 29*8(sp)
    sd      x30, 30*8(sp)
    sd      x31, 31*8(sp)
    sd      zero, 0*8(sp)       # x[0] = 0, for a complete frame

    # The interrupted sp (x2) is where we were before pushing the frame.
    addi    t0, sp, 288
    sd      t0, 2*8(sp)

    # The trap CSRs.
    csrr    t0, sepc
    sd      t0, 32*8(sp)
    csrr    t0, scause
    sd      t0, 33*8(sp)
    csrr    t0, stval
    sd      t0, 34*8(sp)
    csrr    t0, sstatus
    sd      t0, 35*8(sp)

    # riscv_trap_dispatch(frame: &mut TrapFrame). The frame is the current sp.
    mv      a0, sp
    call    riscv_trap_dispatch

    # Restore the CSRs the dispatcher may have changed (sepc to step past a syscall/breakpoint,
    # sstatus for the return privilege/interrupt state).
    ld      t0, 32*8(sp)
    csrw    sepc, t0
    ld      t0, 35*8(sp)
    csrw    sstatus, t0

    # Restore the general registers (x0 stays zero; x2/sp restored last, off the still-live frame sp).
    ld      x1,  1*8(sp)
    ld      x3,  3*8(sp)
    ld      x4,  4*8(sp)
    ld      x5,  5*8(sp)
    ld      x6,  6*8(sp)
    ld      x7,  7*8(sp)
    ld      x8,  8*8(sp)
    ld      x9,  9*8(sp)
    ld      x10, 10*8(sp)
    ld      x11, 11*8(sp)
    ld      x12, 12*8(sp)
    ld      x13, 13*8(sp)
    ld      x14, 14*8(sp)
    ld      x15, 15*8(sp)
    ld      x16, 16*8(sp)
    ld      x17, 17*8(sp)
    ld      x18, 18*8(sp)
    ld      x19, 19*8(sp)
    ld      x20, 20*8(sp)
    ld      x21, 21*8(sp)
    ld      x22, 22*8(sp)
    ld      x23, 23*8(sp)
    ld      x24, 24*8(sp)
    ld      x25, 25*8(sp)
    ld      x26, 26*8(sp)
    ld      x27, 27*8(sp)
    ld      x28, 28*8(sp)
    ld      x29, 29*8(sp)
    ld      x30, 30*8(sp)
    ld      x31, 31*8(sp)
    ld      x2,  2*8(sp)        # restore the interrupted sp (discards the frame)

    sret

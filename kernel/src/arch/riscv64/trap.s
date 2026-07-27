# The S-mode trap vector and the U-mode return path.
#
# The RISC-V analog of aarch64's vectors.s. RISC-V has one trap entry (stvec), and unlike aarch64 it
# does NOT switch stacks automatically: a trap from U-mode arrives still on the user stack. So we use
# the sscratch CSR as a per-hart trap-stack pointer, the standard RISC-V dance:
#
#   sscratch = the current thread's kernel-stack top  while running in U-mode
#   sscratch = 0                                       while running in S-mode (the kernel)
#
# On a trap we swap sp and sscratch. If the swapped-in value is nonzero we came from U-mode and now
# hold the kernel stack; if it is zero we came from S-mode and swap back to the stack we were on.
# Either way we then build a TrapFrame, dispatch, and return through the shared `trap_return`, which
# restores sscratch to the kernel-stack top when it returns to U-mode.
#
# The frame layout is `struct TrapFrame`: x[0..32] then sepc, scause, stval, sstatus (288 bytes).

.section ".text", "ax"
.balign 4                       # stvec direct mode needs the vector 4-byte aligned
.global trap_entry
trap_entry:
    csrrw   sp, sscratch, sp    # swap: sp <-> sscratch
    bnez    sp, 1f              # nonzero => came from U-mode; sp is now the kernel stack
    csrrw   sp, sscratch, sp    # came from S-mode (sscratch was 0); swap back to our own sp
1:
    # sp = the kernel stack to build the frame on. For a U-mode trap, sscratch now holds the user sp;
    # for an S-mode trap, sscratch is 0.
    addi    sp, sp, -288

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
    sd      zero, 0*8(sp)

    # The interrupted sp (x[2]). From U-mode it is the user sp, now sitting in sscratch. From S-mode
    # sscratch is 0 and the interrupted sp is just above our frame (sp + 288).
    csrr    t0, sscratch
    bnez    t0, 2f
    addi    t0, sp, 288         # S-mode: the sp we were on before pushing the frame
2:  sd      t0, 2*8(sp)
    csrw    sscratch, zero      # we are in S-mode now; a nested trap uses this same kernel stack

    csrr    t0, sepc
    sd      t0, 32*8(sp)
    csrr    t0, scause
    sd      t0, 33*8(sp)
    csrr    t0, stval
    sd      t0, 34*8(sp)
    csrr    t0, sstatus
    sd      t0, 35*8(sp)

    # Restore the kernel's tp (per-CPU pointer). tp is a general register on RISC-V, so a trap from
    # U-mode arrives with the user's tp (0). The user's value is already saved in the frame above;
    # reload the kernel's from KERNEL_TP so the handler's cpu::current() is valid. Harmless from
    # S-mode (same value). Done here, after the frame save, so t0 is free to clobber.
    la      t0, KERNEL_TP
    ld      tp, 0(t0)

    mv      a0, sp
    call    riscv_trap_dispatch
    # fall through to trap_return

# Restore a TrapFrame at sp and return from the trap. Shared by the trap path and by the first entry
# to U-mode (enter_user). If the frame returns to U-mode (sstatus.SPP == 0), arm sscratch with the
# kernel-stack top so the next U-mode trap lands on the kernel stack.
trap_return:
    ld      t0, 35*8(sp)        # sstatus
    andi    t1, t0, 0x100       # SPP (bit 8): 1 = return to S-mode, 0 = return to U-mode
    bnez    t1, 3f
    addi    t0, sp, 288         # returning to U-mode: sscratch = this thread's kernel-stack top
    csrw    sscratch, t0
3:
    ld      t0, 32*8(sp)
    csrw    sepc, t0
    ld      t0, 35*8(sp)
    csrw    sstatus, t0

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
    ld      x2,  2*8(sp)        # the interrupted sp (user sp for a U-mode return)

    sret

# The first entry to U-mode: load `frame` (a0) as the trap frame and return into it. The frame was
# built by TrapFrame::for_user_entry with sstatus.SPP = 0 (U-mode) and SPIE set, sepc = the entry,
# x[2] = the user sp, a0..a2 = the child's arguments. Reached only through `enter_user` in
# exceptions.rs, which is #[inline(always)] so the frame (sitting on this same kernel stack) is not
# clobbered by a call-frame push before the `mv sp, a0`.
.global user_return
user_return:                    # a0 = *mut TrapFrame
    mv      sp, a0
    j       trap_return

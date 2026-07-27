# The S-mode trap vector and the U-mode return path.
#
# The RISC-V analog of aarch64's vectors.s. RISC-V has one trap entry (stvec) and does NOT switch
# stacks automatically: a trap from U-mode arrives still on the user stack, with the user's `tp`.
#
# So `sscratch` holds a pointer to this hart's per-hart trap stash (`struct TrapStash` in mod.rs), in
# BOTH U- and S-mode. The stash carries what the entry needs to recover before it has a stack or a
# valid `tp`:
#
#   0(stash)  kernel_sp  the current thread's kernel-stack top (where a U-mode trap lands)
#   8(stash)  percpu     this hart's PerCpu pointer (the kernel `tp`)
#   16(stash) scratch0   a scratch word
#   24(stash) scratch1   a scratch word
#
# Unlike the old single-hart design (which kept the kernel `tp` in one global and the kernel-stack top
# directly in `sscratch`), the stash is per-hart and reached through the per-hart `sscratch`, so every
# hart recovers ITS OWN `tp` and stack. That is what makes the trap path SMP-correct. Whether a trap
# came from U- or S-mode is read from `sstatus.SPP`, not from `sscratch` (which is never 0 now).
#
# The frame layout is `struct TrapFrame`: x[0..32] then sepc, scause, stval, sstatus (288 bytes).

.section ".text", "ax"
.balign 4                       # stvec direct mode needs the vector 4-byte aligned
.global trap_entry
trap_entry:
    # sscratch = &TrapStash for this hart. Swap it into t0, parking the interrupted t0 in sscratch.
    csrrw   t0, sscratch, t0    # t0 = &stash ; sscratch = interrupted t0
    sd      t1, 16(t0)          # scratch0 = interrupted t1 (frees t1)
    sd      sp, 24(t0)          # scratch1 = interrupted sp (frees sp)

    # Which stack to build the frame on? SPP (sstatus bit 8): 1 = from S-mode (stay on the sp we were
    # on), 0 = from U-mode (switch to this thread's kernel-stack top).
    csrr    t1, sstatus
    andi    t1, t1, 0x100
    bnez    t1, 1f
    ld      sp, 0(t0)           # U-mode: land on kernel_sp
    j       2f
1:  ld      sp, 24(t0)          # S-mode: the interrupted sp (saved in scratch1)
2:  addi    sp, sp, -288

    # Save the general registers. Registers we have not clobbered (x1, x3, x4, x7..x31) are saved
    # straight from their live values; the clobbered ones (x2=sp, x5=t0, x6=t1) come from the stash
    # or sscratch, where the entry parked them.
    sd      x1,  1*8(sp)
    sd      x3,  3*8(sp)
    sd      x4,  4*8(sp)        # interrupted tp; the kernel tp is reloaded below
    csrr    t1, sscratch        # interrupted t0 (parked in sscratch by the first swap)
    sd      t1,  5*8(sp)
    ld      t1, 16(t0)          # interrupted t1 (scratch0)
    sd      t1,  6*8(sp)
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
    ld      t1, 24(t0)          # interrupted sp (scratch1)
    sd      t1,  2*8(sp)

    csrr    t1, sepc
    sd      t1, 32*8(sp)
    csrr    t1, scause
    sd      t1, 33*8(sp)
    csrr    t1, stval
    sd      t1, 34*8(sp)
    csrr    t1, sstatus
    sd      t1, 35*8(sp)

    # Recover the kernel per-CPU pointer (`tp`) for THIS hart from the stash. A trap from U-mode
    # arrived with the user's tp; from S-mode it is already correct, and this reloads the same value.
    ld      tp, 8(t0)           # percpu

    # Restore sscratch to &stash (t0) for the next trap; it currently holds the interrupted t0, which
    # is already saved in the frame.
    csrw    sscratch, t0

    mv      a0, sp
    call    riscv_trap_dispatch
    # fall through to trap_return

# Restore a TrapFrame at sp and return from the trap. Shared by the trap path and by the first entry
# to U-mode (enter_user). On a return to U-mode (sstatus.SPP == 0), record this thread's kernel-stack
# top in the hart's stash, so the next U-mode trap lands there.
trap_return:
    ld      t0, 35*8(sp)        # sstatus
    andi    t1, t0, 0x100       # SPP (bit 8): 1 = return to S-mode, 0 = return to U-mode
    bnez    t1, 3f
    csrr    t0, sscratch        # &stash for this hart
    addi    t1, sp, 288         # this thread's kernel-stack top
    sd      t1, 0(t0)           # stash.kernel_sp
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

# 3. Hand out physical memory, and detect a smashed stack

**Status: BUILT.**

Backfilled 2026-08-03 from history (milestone 76). Built in `ba47a27` (2026-07-13), written up in
`097ef52` ("including the two hours it cost"), with the initrd reservation and the bitmap's
placement proven in `bad2e28`.

The kernel reads its memory map from the device tree instead of a constant, and hands out 4 KiB
frames. The commit records the two decisions that still describe the allocator: the bitmap is
carved by hand out of the memory it is about to manage ("the allocator's first act is to allocate
itself"), and everything starts marked USED, because a bitmap that defaulted to free would hand
out the MMIO hole and give you the UART's control registers as scratch space.

The milestone also blew the kernel stack, took an embarrassingly long time to notice, and left
two permanent marks: the stack canary (checked after every test, in the panic handler, and in the
fault handler) and notes/stack.md. The canary is recorded in the commit as a mitigation rather
than a fix; the real fix, a guard page below the stack, waited for milestone 4's MMU.

One known hole was recorded rather than papered over: the allocator's lock was a formality on one
core with no interrupts, and the commit demanded a written-down locking discipline before
milestone 5. That arrived as `6974f6c` (IrqSafeMutex, handlers do not allocate) and was later
enforced in `e85802d` rather than merely written down.

# Milestones

Each rung is independently demoable. The dividing line between "a Rust program that
boots" and "an operating system" is milestone 7.

| #  | Milestone                                      | What it teaches                          |    |
|----|------------------------------------------------|------------------------------------------|----|
| 1  | Boot to Rust on QEMU `virt`, print to UART      | Freestanding binaries, linker scripts    | ✅ |
| 2  | Exception vectors, handlers, fault reports      | ARM privilege model, exception dispatch  | ✅ |
| 3  | Physical frame allocator from the memory map    | Where RAM actually comes from            | ✅ |
| 4  | MMU on: page tables, address spaces, kernel heap| Virtual memory, `alloc` in `no_std`      | ✅ |
| 5  | GIC + timer interrupts                          | The preemption source                    | ✅ |
| 6  | Kernel threads, context switch, scheduler       | Stacks, register files, run queues       | ✅ |
| 7  | **EL0, address spaces, CSpaces, ELF loader, IPC** | **The actual OS boundary.** Decided in §10  | ✅ |
| 8  | **The console driver LEAVES the kernel**        | The microkernel thesis, executable        | ✅ |
| 9  | virtio-blk in userspace + a filesystem server   | Userspace drivers, MMIO caps, IRQ-as-message, DMA | ✅ |
| 10 | A process server, and a shell that spawns binaries | Proof the whole stack works            | ✅ |
| 11 | Untyped memory: a process allocates, the kernel does not | §10's deferred axis, to the extent §10 intended. | ✅ |

Milestone 8 is the one that proves §10 was real. When it lands, **the kernel no longer knows
what a UART is.** If we cannot take the console out, we did not build a microkernel; we built a
monolithic kernel with an unusual syscall table.

Milestone 11 is complete *to its intent*, not to seL4's. The kernel still allocates its own
page tables, TCBs, and endpoints from the heap; §10 chose that deliberately (Zircon's model).
What 11 demonstrates is the half that was the point: a userspace process spends pages out of an
`Untyped` capability and **the kernel's free-frame count does not move**, so a process cannot
force the kernel to allocate, and kernel-memory exhaustion stops being an attack class. Taking
the allocators out of the kernel entirely stays additive and unbuilt.

## Beyond the plan (post-v1)

The eleven milestones are the plan. Work since, in git order: a security audit
(notes/security.md); per-process spawn quotas (notes/quotas.md); kernel-mediated DMA
confinement, since QEMU `virt` has no IOMMU (notes/dma.md); capability delegation between
processes via `SEND_CAP`/`RECV_CAP` (notes/delegation.md); frame capabilities, shared memory a
process owns and delegates (notes/frames.md); SMP (§11); Call/Reply IPC, a one-shot reply capability
(§12, milestone 12); and capability revocation with safe untyped reclamation, scoped to frames (§13,
milestone 13).

**The road past v1** is sketched in [design/roadmap/](roadmap/README.md): proposed milestones
12-17 and the two decisions they force. Milestone 12 (Call/Reply IPC) is §11's sibling in getting its own
decision entry before code, and the first of them built; the rest stay proposals until started.

Deliberately out of scope for v1: a writable filesystem, networking, a GUI, dynamic linking.
Each multiplies debugging difficulty and none teaches something the first ten don't already set
up. SMP and real hardware, listed here originally, are now on the table.

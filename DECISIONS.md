# cricker-os: Architecture Decisions

Decisions made 2026-07-12, before any code was written. Each entry records what we
chose, what we rejected, and why. Revisit these deliberately, not accidentally.

## 1. Target architecture: aarch64

Chosen over x86_64 and RISC-V.

x86_64 has the deepest pool of tutorials, but a large fraction of what it teaches is
Intel history (real mode, the A20 line, segmentation ghosts, PIC-vs-APIC) rather than
operating system concepts. RISC-V is the cleanest architecture to learn on, but it is
the *hardest* of the three to actually get onto silicon: peripheral documentation for
the JH7110-class SoCs is thin.

aarch64 gives a clean exception model (EL0/EL1/EL2/EL3), a sane MMU, an excellent
bare-metal community, and real hardware at the end of the road (Raspberry Pi). The dev
machine is also ARM, so kernel assembly and host assembly are the same instruction set.

## 2. Primary target: QEMU `virt`, Raspberry Pi as a later port

The QEMU `virt` machine has a PL011 UART, a GIC interrupt controller, and virtio
devices, all well-specified. Boots in a second, debuggable with GDB, scriptable in
tests.

The Raspberry Pi port is a deliberate later milestone, not an afterthought. It is the
moment the hardware abstraction layer gets tested for real, and it will reveal exactly
which assumptions were secretly QEMU-shaped.

## 3. Use the crate ecosystem

`aarch64-cpu` for system-register access, `tock-registers` for typed MMIO. Not
hand-rolled `asm!` and raw volatile pointer writes.

Time goes to kernel concepts (memory, scheduling, syscalls, filesystems), not to
debugging typos in ARM system-register encodings that a crate would have caught at
compile time.

## 4. Kernel shape: monolithic, deferred, with two cheap rules

We are NOT speculatively trait-ifying every subsystem to "keep the microkernel door
open." That builds the wrong abstraction before the requirements are known, and taxes
every file for a door we may never walk through.

Instead, two rules that cost almost nothing and preserve the real option:

1. **A driver never reaches into a kernel global.** It gets what it needs passed in.
2. **The syscall surface stays narrow and explicit.** It is a boundary, not a habit.

## 5. Execution model: preemptive threads with real stacks

Rejected: async/await cooperative multitasking (where the Philipp Oppermann blog series
ends).

The reason is a hard ceiling, not a matter of taste. A userspace process is an arbitrary
ELF binary. It has its own stack, it never yields, and it will loop forever because we
will write a bug. Under cooperative scheduling, one bad user program hangs the machine
permanently. Real user mode *requires* per-thread stacks, a context switch that saves and
restores the register file, and timer-driven preemption. Async doesn't defer that work,
it forecloses it.

Async can come back later, in userspace, on top of real threads, exactly the way a real
OS lets a program run Tokio. Nothing is given up.

## 6. SMP: single-core, refactor when it hurts

Boot CPU 0 only. Globals and a big lock are fine for now.

We explicitly considered shaping per-CPU data structures up front as cheap insurance,
and declined. Feeling the pain that created per-CPU structures is itself a legitimate
way to learn why they exist. Cost: a scheduler rewrite later. Accepted knowingly.

## 7. Testing: QEMU harness + host-testable crates, from commit one

A custom test harness boots the kernel in QEMU, runs tests, and exits with a status code
`cargo test` understands. Separately, pure logic (allocator algorithms, page-table math,
scheduling policy, filesystem parsing) lives in crates that compile for the *host*, so
most tests run in milliseconds with no emulator.

Front-loads about a day. Prevents a year of debugging by `println!`.

## 8. Process model / syscall ABI: DEFERRED to a hard decision point

Unix-like (fds, fork/exec) versus capability-based (seL4/Fuchsia-shaped) is genuinely
undecided, on purpose. Milestones 1-6 do not touch the syscall boundary, and every
kernel builds them roughly the same way, so the deferral is free until it isn't.

**Milestone 7 (user mode) is a hard decision point.** When we get there we stop, look at
what we've built, and choose deliberately. This deferral is a plan, not a drift. If we
find ourselves hacking in a syscall without having had that conversation, the plan has
failed.

---

## Open design ideas

Not decisions yet. Proposals with real open questions, parked deliberately.

- [Microarchitecture-variant binaries](design/fat-binaries.md) — our targets straddle the
  ARMv8.0 / ARMv8.2 line (no LSE atomics on Cortex-A72, LSE on everything newer), and with
  no libc we can't lean on LLVM's `outline-atomics` to paper over it. Milestone 6 forces
  the kernel-atomics question; milestone 7 is where a fat userspace format would be
  decided. Feature detection via the `ID_AA64ISAR*_EL1` registers is worth building at
  milestone 2 regardless.

---

## Milestones

Each rung is independently demoable. The dividing line between "a Rust program that
boots" and "an operating system" is milestone 7.

| #  | Milestone                                      | What it teaches                          |    |
|----|------------------------------------------------|------------------------------------------|----|
| 1  | Boot to Rust on QEMU `virt`, print to UART      | Freestanding binaries, linker scripts    | ✅ |
| 2  | Exception vectors, handlers, fault reports      | ARM privilege model, exception dispatch  | ✅ |
| 3  | Physical frame allocator from the memory map    | Where RAM actually comes from            |    |
| 4  | MMU on: page tables, address spaces, kernel heap| Virtual memory, `alloc` in `no_std`      |    |
| 5  | GIC + timer interrupts                          | The preemption source                    |    |
| 6  | Kernel threads, context switch, scheduler       | Stacks, register files, run queues       |    |
| 7  | **User mode (EL0), syscalls, ELF loader**       | **The actual OS boundary. Decision point.** |    |
| 8  | virtio-blk driver + read-only filesystem        | Drivers, DMA, block I/O                  |    |
| 9  | Processes: spawn, exit, wait                    | Process lifecycle                        |    |
| 10 | A userspace shell that runs other binaries      | Proof the whole stack works              |    |

Deliberately out of scope for v1: SMP, a writable filesystem, networking, a GUI,
dynamic linking, real hardware. Each multiplies debugging difficulty and none teaches
something the first ten don't already set up.

## Reading

- **xv6 book** (MIT, ~100pp) for how a real Unix-shaped kernel is structured
- `rust-raspberrypi-OS-tutorials` for the aarch64-specific mechanics
- OSDev wiki as a reference, not a tutorial
- *Operating Systems: Three Easy Pieces* for the theory

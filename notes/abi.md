# The native ABI

*(Milestone 19e, "Decision 2". The contract a cricker-os program runs against: how it starts, how
it makes syscalls, and how it meets its capabilities. `crates/abi` is the machine-readable half;
this note is the prose half. See DECISIONS.md §10 for why the model is capability-based, and §14 for
why "native ABI" and not Linux-compat.)*

The big fork was already settled at milestone 7 (§10): the process model is capability-based, not
Unix. So this is not a decision about `fork` versus capabilities. It is the smaller, still-open
question the 19f split forced into the open: now that we deliver and run distinct programs, what is
the *contract* between a program and the system? Three parts: the syscall convention, the object
surface reached through it, and how a program meets its world at startup.

The decision here is to **write down and commit the convention we already run**, rather than build a
self-describing environment (a BootInfo page). Hardcoded, out-of-band agreement on the initial
capability layout between a parent and the children it builds is the normal microkernel pattern
(seL4 hands a BootInfo only to its *root* task; every other task gets caps placed by its parent per
a private layout). Our init is that parent. A BootInfo mechanism earns its keep when a loader must
start programs whose layout it cannot know in advance, which is milestone 23 (live component
replacement, competing vendors), not now. See "What is deliberately deferred".

## 1. The syscall convention

One instruction, `svc #0`. The kernel reads the registers, does the work, and returns.

| register | on entry | on return |
|---|---|---|
| `x8` | the syscall number | unchanged |
| `x0` | first argument | the `i64` result |
| `x1`–`x4` | further arguments | (see the specific syscall) |

Four syscall numbers, and that is the whole width of the trap:

| `x8` | name | meaning |
|---|---|---|
| 0 | `SYS_EXIT` | terminate this thread; the kernel reaps it and frees its address space. Never returns. |
| 1 | `SYS_YIELD` | give up the CPU voluntarily. |
| 2 | `SYS_INVOKE` | invoke a capability. **This one carries the entire capability world** (see §2). |
| 3 | `SYS_CAP_DELETE` | drop the capability in a cspace slot, so the slot can be reused. |

That narrowness is deliberate (DECISIONS rule 3: the syscall surface stays a boundary, not a habit).
Everything a program can do to another object goes through the single `SYS_INVOKE` door; adding a
capability *type* or a *method* does not widen the trap, it adds a row to a table the kernel already
dispatches. `crates/user_rt` is the userspace side of this: `invoke`, and `send`/`recv`/`exit` built
on it.

## 2. The object surface, reached through `SYS_INVOKE`

`invoke(cap, method, a0, a1, a2)`: `cap` names a capability in the calling thread's cspace (a small
integer, like a file descriptor), `method` selects an operation on the object that capability points
at, and `a0..a2` are the operation's arguments. The kernel checks that the slot holds a capability,
that its *rights* permit the method, and that the object's type understands it. The method numbers
live per object type in `crates/abi`:

- **Endpoint** (`endpoint::`): `SEND`, `RECV`, `CALL`, and the capability-passing pair `SEND_CAP` /
  `RECV_CAP`. The synchronous-IPC primitive the whole system talks over. `WRITE` rights permit
  `SEND`; `READ` rights permit `RECV`; `GRANT` permits passing a capability along.
- **Reply** (`reply::REPLY`): the one-shot return leg of a `CALL`.
- **Untyped / objects** (`objtype::`): `RETYPE` an untyped region into an `ENDPOINT`, `ASPACE`, or
  `TCB`. This is how a process builds new kernel objects out of a raw memory budget it holds.
- **TCB** (`tcb::`): `CONFIGURE` (entry, stack, address space), `CAP_INSERT` (place a capability into
  the child's cspace), `START` (see §3).
- **Aspace** (`aspace::MAP_INTO`, with modes `MAP_RO` / `MAP_RW` / `MAP_CODE`): map a frame into an
  address space at a chosen virtual address with chosen permissions.
- **Irq** (`irq::WAIT` / `ACK`): block until an interrupt the capability names fires, then
  re-enable it. This is how a userspace driver owns its device's interrupt.
- **Rights** (`rights::READ` / `WRITE` / `GRANT`): the authority a capability carries, checked on
  every invoke. A capability can be delegated with *narrowed* rights but never widened.

A program never sees a raw pointer to any of these. It sees a slot number, and the kernel is the
only thing that can turn that number into the object. That is the §10 thesis in one sentence.

## 3. The entry contract

A program is an ordinary aarch64 **ELF**, linked in the low half (TTBR0, at `0x40_0000`; see
notes/linker-scripts.md). The loader lays out its segments, gives it a stack, populates its cspace
(§4), and enters it at the ELF's `e_entry` with three register arguments:

```
_start(x0, x1, x2) -> !
```

`START` (the TCB method) is what hands those three words to the new EL0 thread; the kernel routes
them through `Thread::start_args` into `x0`/`x1`/`x2` at first entry (milestone 19e widened `START`
from one argument to three; see notes/tcb.md). Their meaning is **the program's to define**, with one
reserved case:

- For most programs, `x0`/`x1`/`x2` are plain arguments. A worker takes its input `n` in `x1`. A
  standalone binary that needs no argument ignores all three.
- **init** is the exception the loader knows about: the kernel starts init with the initrd length in
  `x1`, because init must find the archive it loads everything else from (notes/init-and-loading.md).
- Historically `x0` was a *role selector* for the one multi-tool `hello` binary. After the 19f split
  every program is its own binary, so `x0` is a free argument again, not a dispatch key.

A program never returns from `_start`. It runs until it calls `SYS_EXIT` (a worker, when its job is
done) or loops forever serving requests (a driver). Returning would fall off the end of the world;
there is no runtime to catch it.

There is no libc, no `argv`/`envp` array, no dynamic loader, no `main` wrapper. `_start` *is* the
program. What a C runtime would do before `main` (zero `.bss`, set up the stack) is either done by
the loader (the stack) or unnecessary (a freshly mapped frame is already zero; `.bss` is a fresh
frame).

## 4. How a program meets its capabilities

Before `START`, the program's loader (init, or the kernel's own service wiring) has placed the
capabilities the program needs into low cspace slots, and mapped any shared pages it needs at agreed
virtual addresses. The program hardcodes which slot holds what and which VA is which. That agreement
is the contract, and it is **per program**, published in that program's own source:

- the **worker** is granted one endpoint at slot 0 (its result channel).
- the **console** server gets its request endpoint at slot 0, its reply endpoint at slot 1, the
  shared text page read-only at `0x60_0000`, and the UART device frame.
- the **input** driver gets the line endpoint at slot 0 and its RX interrupt capability at slot 1.
- the **shell** holds five endpoints (slots 0–4) and two shared pages.

This is out-of-band agreement, not discovery: the program does not ask "what am I holding," it knows
by the contract it was built to. That is the same shape seL4 uses for every task below the root, and
it is honest to call it a convention rather than dress it up as an API. The convention *is* the ABI;
writing it down (here, and in each program's header comment) is what milestone 19e commits.

## What is deliberately deferred

- **A BootInfo / self-describing environment.** A structured block the loader hands the program that
  lists its initial capabilities, their rights, and its arguments, so a program can *discover* its
  world instead of assuming a layout. This is what a generic loader needs when it starts programs it
  did not build and whose layout it cannot know. We do not have that situation yet (init builds every
  program and knows every layout), so a BootInfo would be a mechanism without a requirement. It lands
  when milestone 23 (live component replacement) creates the requirement.
- **A POSIX shim.** §10 records why this is *additive* and can come later without a rewrite: `open`
  / `read` / `write` over capability handles, the way Fuchsia's `fdio` does. Not needed to run a
  native workload, which is the whole point of doing the native ABI first.

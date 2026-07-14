# The console driver leaves the kernel

Milestone 8. The one that decides whether DECISIONS §10 was a real architecture or a syscall
table with an unusual shape.

## What actually moved, stated precisely so we don't lie to ourselves

**The console that user programs use is now a userspace process.** A program that wants to print
holds a `WRITE` capability on the console server's endpoint and sends to it. The server, running
at **EL0**, owns a mapping of the PL011's registers and does the `while TXFF { } ; DR = byte`
that used to live in `kernel/src/drivers/pl011.rs`. That loop is the same; only its exception
level changed.

What **stays** in the kernel is a *debug* UART: `println!`, for boot messages, panics, and the
test harness. This is not a cheat and it is not a failure of the thesis. **seL4 does exactly
this** — a debug `putchar`, compiled out of release builds, entirely separate from the console
anyone actually uses. A kernel cannot IPC to a userspace console *while it is panicking*, so it
must be able to put a byte on a wire by itself. The honest claim is therefore narrow and true:

> **There is no code path a user program can take that reaches kernel UART code.**

`Object::Console` is gone from the syscall dispatch. `console::write_bytes` is gone. The kernel
no longer reads a user's bytes and puts them on the wire. That is the part that left.

## The kernel is no longer on the data path, and a bug went with it

Here is the deep change, and it is worth dwelling on.

In milestone 7d, printing was `write(console_cap, ptr, len)`: the user handed the kernel a
**pointer**, and the kernel read the user's memory and wrote it to the UART. That is why 7d needed
`user_can_read` (the `AT S1E0R` trick): a user could pass `0xffff_0000_...`, the kernel's own
memory, and a careless kernel would print it *on the user's behalf, using its own authority*. The
confused deputy.

Milestone 8 **dissolves that bug** rather than defending against it. The data path is now:

```
  client's address space          server's address space
  ----------------------          ----------------------
     shared page  (RW) ───── same physical frame ───── shared page  (RO)
          ▲                                                  │
          │ write the bytes                                  │ read the bytes
          │                                                  ▼
        client ───────── SEND(len) over endpoint ────────► server ──► UART
                         (the KERNEL sees only a number)
```

The bytes never enter the kernel. The client writes them into a page it *shares* with the server;
only the **length** crosses the endpoint, in a register. The kernel copies nothing, validates no
pointer, and **cannot be a confused deputy because it is not a deputy** — it does no I/O for
anyone. The thing that could be confused no longer exists.

This is DECISIONS §10's rule, executed exactly: **IPC carries control, shared memory carries
data.** Put the bytes in the message and you copy twice and you are Mach; put a shared frame under
them and the data moves once, by the client's own store and the server's own load.

(`mmu::user_can_read` stays in the tree, marked `allow(dead_code)`. Its caller left with the
console, but it is the tool the *next* syscall that takes a user pointer — a filesystem server's
`read` — will need, and it is not speculative: the technique is load-bearing and documented.)

## One binary, several roles

The console server and its client are the **same ELF**. There is one file in the initrd, and the
kernel tells the copies apart by the value it puts in `x0` at `_start`, the way a real kernel
hands a new process its `argv`:

- `SELF_CHECK` (0): inspects its own image and yields. Needs no capabilities. This is the program
  the milestone-7 tests spawn bare.
- `CONSOLE_SERVER` (1): the driver loop. Owns the UART and the shared page (read-only).
- `PRINTING` (2): self-checks, then prints through the server.

`x0` is a channel that needs no capability, which is right: "which of you are you" is not
authority over anything.

## The mechanism that lets a driver hold hardware

`AddressSpace::map_physical(va, phys, flags)` maps an **existing** physical page into a user
address space, without recording it as owned (so it is not freed when the process dies, because
the process does not own it — it is shared, or it is a device). Two uses, both new:

- The **UART's registers** at `phys 0x0900_0000`, into the server, with `Flags::user_device()`:
  device-typed (no caching, no reordering of register writes), EL0 read/write, never executable.
  This one mapping is what a userspace driver *is*.
- The **shared buffer**, into both client and server, one `user_data()` (RW) and one
  `user_rodata()` (RO).

`Flags::user_device()` is the new page-permission, and it is the smallest possible statement of
"a driver, at EL0." Everything else was already there.

## Two mappings of one UART

The kernel maps the PL011 in its direct map (device, EL1) for `println!`; the server maps the
same registers in its own address space (device, EL0). Two mappings of one physical device. In
QEMU that is fine, and since the kernel now prints only at boot and on a panic — never during a
user program's run — they do not interleave in practice. On real hardware you would give the
server exclusive ownership (a device capability the kernel checks before mapping); we do not model
that enforcement yet, and the note here is where that gap is recorded.

## What it prints

The lines between the kernel's own bookends are produced entirely by a program at EL0, driving
hardware the kernel handed it:

```
  the console driver now runs at EL0. what follows is printed by it:

      hello from EL0, printed by a driver that also runs at EL0.
      the kernel never saw these bytes.

  ...and control is back in the kernel, which never saw those bytes.
```

## What a driver bug costs now

Everything §10 promised. The console server is a process. If its driver loop dereferences a bad
pointer, **it faults alone** (`user_fault` → `sched::exit`, milestone 7a), the kernel prints the
death on its debug UART, and the machine keeps running. A driver bug is a crashed process, not a
dead kernel. We could restart it. That is the entire reason the microkernel argument exists, and
it is now a property of this system rather than a claim about other ones.

## What is still scaffolding

- The shared buffer is wired up **at spawn** by the kernel, not delegated at runtime. Handing
  memory over as a first-class `Frame` capability (so a process can share a page it already holds,
  with rights that narrow) is the natural next object type. Today the sharing is static.
- There is **one** client and **one** server. A real console multiplexes many clients; that is a
  server-side concern (a queue, a lock) and does not change the kernel.
- The kernel still allocates the shared frame and the server's stack from its own heap. That is
  §10's deferred third axis (untyped memory, milestone 11), where the kernel stops allocating at
  all.

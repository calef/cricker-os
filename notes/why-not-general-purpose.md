# Why cricker-os isn't suited to general-purpose applications

Two answers, layered. The first is about intent, the second about mechanics, and the third
is the nuance that keeps both honest.

## It was never trying to be

The stated goal (top of [CLAUDE.md](../CLAUDE.md), and [DECISIONS.md](../DECISIONS.md) §10)
is understanding how operating systems work, not running applications. *Velocity is not the
goal. Understanding is.*

So "not suited to general-purpose use" is not a shortfall against the aim. It is a
consequence of choices made to maximize learning, several of which trade *away*
general-purpose readiness on purpose:

- Capability-based, with **no `open(path)` and no ambient authority**, which makes porting
  existing software deliberately hard (§10: we are not building the back door).
- **No xv6 to copy from**, so every design is derived rather than transcribed. A feature if
  the goal is understanding, a cost if the goal is shipping.
- Drivers pushed out to **userspace for isolation**, not throughput (§10). More crossings,
  chosen for what it teaches and proves.

## What an application would actually hit

A "general-purpose application" (a browser, a database, `curl`, anything people wrote) needs
a world to run in. cricker-os does not provide it, roughly in order of severity:

| Gap | What breaks |
|---|---|
| **No POSIX, no libc, no `std` target** | The big one. Real programs are written against an API (POSIX, Win32) and linked with a libc. Nothing targets our tiny capability syscall surface. You cannot drop in existing software; every program is hand-written against our ABI, which is why the only programs are the handful in `user/` that we wrote. |
| **No writable filesystem** | `crickerfs` is read-only, one block, built at compile time by `xtask`. Apps read config and write output and keep state; there is nowhere to do any of that at runtime. |
| **No networking** | No TCP/IP, no sockets. A huge fraction of general-purpose software is a network client or server. |
| **No display, GUI, or input beyond a serial console** | No framebuffer, windowing, keyboard, or mouse. The only I/O to a human is a UART. |
| **No dynamic linking** | Static only; every program is a standalone ELF baked into the initrd. No shared libraries, no `dlopen`, no loading code from a filesystem you populate. |
| **Tiny, fixed platform** | QEMU `virt` (and HVF), 128 MiB, virtio-blk as the only storage, no swap, no demand paging, and single-core until the §11 SMP work lands. |

The kernel genuinely **can** run an arbitrary ELF it never compiled: that was milestone 7's
whole point, the "run code it did not compile" thesis ([userspace.md](userspace.md)). What
it cannot do is run *useful* software, because there is no ecosystem to build that software
against and no subsystems for it to touch once running.

## The nuance: the model is not the barrier

A capability-based microkernel is exactly what **Fuchsia / Zircon** ships as a
general-purpose OS on real devices. cricker-os is not unsuited because capabilities cannot
do general-purpose work. It is unsuited because it is a deliberately small **teaching subset**
of that model.

What is missing is the userspace built *on top* of the kernel:

- a **POSIX personality** (Fuchsia's `fdio`; §10 notes this is additive, `open`/`read`/`write`
  over capability handles),
- a real **VFS and writable filesystem**,
- a **network stack**,
- a **display server**.

Each is a milestone someone could build, and the kernel was shaped ([DECISIONS.md](../DECISIONS.md)
§4 rules) to keep them **additive** rather than blocked. This is the same asymmetry argument
that decided §5 and §10: capabilities → a Unix-shaped API is additive; the reverse is a
rewrite.

## So what it *is* suited for

An early, honest, thoroughly-understood floor. It is not the wrong *kind* of thing to grow
into a general-purpose OS; it is missing the large subsystems and the compatibility ecosystem
that turn a kernel into a platform. Every one of those is out of scope on purpose, because
building them teaches less per unit of debugging pain than the foundation did, and because
the point was to understand the machine, not to ship a product.

---

*Add to this file if the scope question comes up again, or when a subsystem that would move
the needle (a writable FS, a POSIX shim, a net stack) actually gets built.*

# Semihosting

**A syscall interface where the operating system on the other side is QEMU.**

## Half-hosted

That's what "semi" means. The code is running bare metal with no OS beneath it, but it is
*half* hosted: it can reach out and use a real machine's facilities.

The idea comes from ARM's debug tooling. Picture developing on a bare board over a JTAG
cable. The board may have no filesystem, no stdout, maybe not even a working UART yet. But
the workstation it's plugged into has all of that. Semihosting lets code on the board call
`printf` and have the text appear on your PC, or open a file that lives on your PC's disk.

## The mechanism

A magic trap instruction. On aarch64: `hlt #0xF000`.

Nothing is special about that instruction or that immediate. It is simply a number everyone
agreed to watch for.

1. The CPU executes `hlt #0xF000` and traps.
2. If a semihosting host is attached (a debugger, or QEMU with `-semihosting`), it
   **intercepts the trap** before the guest ever sees it.
3. The host reads `x0` for the operation number and `x1` for a pointer to the arguments.
4. It performs the operation **on the host machine**, writes a result back into `x0`, and
   resumes the guest at the next instruction.

The operations are a small fixed ABI: `SYS_OPEN` (0x01), `SYS_WRITEC` (0x03), `SYS_WRITE`
(0x05), `SYS_READ`, `SYS_EXIT` (0x18), `SYS_CLOCK`, `SYS_TIME`, and a handful more.

## The punchline

A trap instruction. An operation number in a register. Arguments pointed to by another
register. A result returned in a register.

**That is a syscall.** Semihosting is a syscall ABI, and the kernel answering it is QEMU. We
are the userspace program.

Which makes it a **preview of milestone 7, running in reverse.** At milestone 7 we build the
*other* side of exactly this shape: a user program at EL0 executes `svc`, traps into our
kernel at EL1, we read an operation number out of a register, do the work, put a result back,
and return. Same architecture. We are currently on the calling end of a mechanism we are
about to go implement.

## What we use it for: exactly one call

See `kernel/src/arch/aarch64/semihosting.rs`.

```rust
let block = [ADP_STOPPED_APPLICATION_EXIT, code as u64];
asm!("hlt #0xf000", in("x0") SYS_EXIT, in("x1") block.as_ptr());
```

`x0` says "terminate." `x1` points at a two-word block holding a reason code and an exit
status. QEMU sees it and exits its own process with that status.

**That is how `cargo test` learns whether the tests passed.** Cargo runs QEMU as a subprocess
and reads its exit code. Zero is a pass, nonzero is a failure. Our test runner exits 0 after
all tests pass; our panic handler exits 1.

Why not print "PASS" to the UART and grep for it? Because cargo doesn't grep, it reads an
exit code. Building on the standard tooling beats a hack that parses text.

## The hang watchdog, and what it cannot tell you

A lost IPC wakeup would leave a test blocked forever, so the whole run hangs silently and
`script/test` never returns. `kernel/src/testing.rs` guards against that: the timer IRQ watches a
heartbeat, and if it does not advance for ~60 s it dumps the thread table and exits with a failure
status (the same semihosting exit, just with a nonzero code). So a real hang fails loudly instead of
hanging the CI.

**The limitation, learned the hard way (milestone 32).** The heartbeat is bumped **once per test, at
the test's start** (in `Testable::run`), not while a test runs. So "no progress for 60 s" cannot
distinguish a genuine deadlock from a test that is simply *slower than 60 s*. Both look identical:
the heartbeat stops advancing because no *new* test started. The milestone 32 FS-server test tripped
this as a false deadlock: it was not stuck, it was starved (leaked spinning driver threads crammed on
core 0 slowed the RedoxFS mount past 60 s), and raising the limit made it pass, which is exactly what
a deadlock can never do. The dump reinforced the wrong read, because a starved thread and a
deadlocked one both sit `Blocked`/`Ready` with nothing obviously moving.

**A harness improvement candidate, not yet built:** a per-test *progress* heartbeat (the test, or the
IPC/scheduler, bumps a counter as work happens) would tell "slow" from "stuck." The enriched
`sched::dump_threads` (each thread's EL0 PC and the per-endpoint sender/receiver/pending counts,
added while chasing that false deadlock) is the other half: two dumps a few seconds apart show
whether the pipeline's threads are changing state (starved but progressing) or frozen (a real
deadlock). Until the heartbeat is per-progress, read a watchdog trip as "stuck **or** slow" and
confirm which with a raised limit before assuming a lost wakeup.

## Why we *don't* use it for console output

Semihosting can print characters (`SYS_WRITEC`). We deliberately don't:

**It's slow.** Every character is a trap, a switch into the host, a host-side write, and a
resume back into the guest. A UART write is one store to one address. Orders of magnitude
apart.

**It only works when a host is attached.** On a real Raspberry Pi with no debugger plugged
in, semihosting does nothing. Our [UART](uart.md) works everywhere, on real silicon, forever.

**"A host is attached" is narrower than "we are under QEMU."** QEMU 11.0.2 does not intercept
semihosting on the **HVF** path: with `-accel hvf` the `hlt #0xf000` is not swallowed but raised
into the guest as a real synchronous exception, `EC 0x00` (Unknown reason,
`ESR_EL1 = 0x02000000`), with the operation number still sitting in `x0`. Our exception handler
correctly panics on an exception it does not recognise, and under `cfg(test)` the panic handler
calls `exit`, which takes the same trap again: an unbounded loop, on every core. This is measured,
not inferred (milestone 81), and it is why the HVF test leg reads the transcript for its verdict
rather than the process's exit status. See [the HVF leg](hvf-leg.md).

So the split is principled: **the UART does everything a real machine can do, and semihosting
does the one thing a real machine fundamentally cannot** (terminate the emulator with a
status code).

You would never ship semihosting enabled in production either. It is a hole straight into the
host machine, which is exactly why QEMU makes you pass an explicit `-semihosting` flag.

## An honest hole in our own code

If no semihosting host is attached, `hlt #0xF000` raises a real exception. **We have not set
up `VBAR_EL1` yet**, so the CPU would jump to whatever address the exception vector base
happens to hold, and we would die silently. The `halt()` fallback at the end of `exit()` would
not actually be reached.

It doesn't matter today, because we always run under QEMU with `-semihosting`. But it is a
real hole, and **milestone 2 (exception vectors) is what closes it.** Once `VBAR_EL1` points
at a handler, that trap becomes something we can see and report instead of a silent death.

---

*Add to this file as new semihosting operations come up.*

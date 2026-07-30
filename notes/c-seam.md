# Running a foreign language: the C seam

Milestone 36, DECISIONS §31. Built 2026-07-29, both ISAs, in QEMU.

The question that started it: **can a userspace service be written in another language, the way a
monolith would have put a C FAT32 driver in the kernel?** The answer is yes, and this milestone is the
smallest thing that proves it. A memory-unsafe C component, compiled by bare-metal clang, running
confined, faulting on a deliberate out-of-bounds write, and restarted by its supervisor.

The component is throwaway on purpose. What is being de-risked is the **seam**: the toolchain, the
linkage shape, the libc, and the confinement proof. That has to happen before a real foreign component
depends on it (libghostty-vt, the display ladder's later rung), because if the seam has a problem, you
want to find it with 150 lines of disposable C rather than half way into a port.

## Why memory-unsafe C is the best demonstration, not the worst

It looks backwards at first. The project's thesis (DECISIONS §14) is a verified-Rust capability
microkernel, and here we are linking C.

But read the thesis again: *a verified core that confines unverified workloads.* The workload was
always going to be unverified. C is simply the most unverified workload available: no bounds checks, no
borrow checker, nothing at all between a bad index and a store to whatever address it computed. **The
more unverified the component, the more the confinement has to prove.** So this is the sharpest
available test of the claim rather than a dilution of it.

And the contrast with a monolith stops being rhetorical and becomes arithmetic:

| | in-kernel C (a monolith) | confined C (here) |
|---|---|---|
| what a bad index reaches | any physical memory, any device register | one page it was granted |
| who notices | nobody, until something else breaks | the kernel, at the instruction |
| what recovers | reboot | its supervisor, in userspace |
| what is trusted | the C | the page tables |

The peer project Atom keeps FAT32, AHCI, and xHCI in the kernel today, which is what the left column
looks like in practice.

The other half of the argument is that **isolation here does not know what a language is.** Page
tables, unforgeable capabilities, the DMA validator, and the IOMMU are all language-agnostic
mechanisms. None of them has an opinion about which compiler produced the instructions they are
confining. That is exactly why a language boundary is cheap here and would be expensive in a system
whose safety came from the language.

## The shape: a Rust shell that holds everything

```text
   kernel  <--- svc / ecall --->  cshim (Rust)  <--- C ABI: (u8*, usize) --->  cseam.c
                                  ^^^^^^^^^^^^
                                  every capability and every syscall stops here
```

`user/src/cshim.rs` is the whole design, and what it does *not* do is the point:

- **The C makes no syscalls.** Not because we asked nicely, but because a syscall needs a capability
  slot number and the C has never seen one. There is no `svc`, no `ecall`, and no inline asm anywhere
  in `cseam.c`, and there could not usefully be: an `svc` with a made-up slot number gets
  `NoSuchSlot`.
- **The C holds no capabilities.** It is handed a pointer and a length.
- So the foreign component **cannot widen the kernel's syscall surface** (DECISIONS §4 rule 3). A
  vendor who wanted one more syscall to make their component work would have to change the Rust shell,
  in this repository, in review.

That makes the confinement claim precisely as narrow as it should be: **the C can corrupt memory
inside a grant the shell already had, and nothing else.**

It is also the same sans-IO shape RedoxFS's `Disk` trait already uses (DECISIONS §27): a component
with the logic and none of the I/O, wired up from outside. Here the boundary is a language rather than
a trait, and the shape did not have to change.

### What crosses, and what cannot

Crossing:

```c
uint32_t cseam_transform(unsigned char *grant, size_t len);   /* honest work */
void     cseam_overrun  (unsigned char *grant, size_t len);   /* the bug, one byte past */
void     cseam_wild     (unsigned char *grant, size_t len);   /* the bug, a page past */
```

Scalars and buffers. Not crossing, and each of these is a seam decision this spike did not have to
make: structs by value, callbacks from C into Rust, ownership transfer, an error type, varargs, C++
(name mangling, exceptions, static initializers), bitfields, enum widths.

The layout of the shared page is agreed by a **comment in both languages** (`user/c/cseam.c`'s
`CSEAM_*` defines and `user/src/cseam.rs`'s constants) rather than by generated bindings. For one page
of bytes that is the right trade; for a real API it would not be, and the honest reason to say so here
is that "we will generate bindings when there is an API worth generating" is a plan and "we forgot"
is not.

## The libc question, answered by tier

The roadmap's milestone-36 block records three tiers of C dependency (design/roadmap.md, "The line
this does not cross"):

1. **Freestanding.** No libc at all, fixed buffers, no allocation. libghostty-vt and littlefs are
   here. Easy.
2. **A handful of symbols.** Shim what the component actually references. **This is what this spike
   proves.**
3. **Full POSIX.** `open`, `fork`, `socket`, threads. Needs a real libc port, which is DECISIONS §15's
   "later, if ever" road and the one Redox took with relibc. **Not walked here, and saying so is what
   keeps this from becoming that project.**

### What the object demands (five), and what the linker demands (two)

Discovered by letting the build fail and reading the error, which is the only honest way to learn what
a foreign object file needs. `llvm-nm --undefined-only` on the compiled object:

```
$ llvm-nm --undefined-only cseam.o          # identical for aarch64 and riscv64
                 U free
                 U malloc
                 U memcpy
                 U memset
                 U strlen
```

Five, and *only* five. Worth stating explicitly what is absent, because each would have been a
finding: no compiler-rt helper (`__muldi3` and friends), no `memmove` conjured out of a struct copy,
no `__stack_chk_fail`. Checked at `-O0`, `-Os`, `-O2`, `-O3`, with and without `-fno-builtin`, and
even with `-fstack-protector-strong` (this component has no stack arrays, so no canary was inserted;
`build.rs` passes `-fno-stack-protector` anyway, so that stays a decision rather than luck).

Then delete all five shims and let the linker speak:

```
rust-lld: error: undefined symbol: malloc
rust-lld: error: undefined symbol: free
```

**Two.** `memcpy`, `memset`, and `strlen` are already there, weakly, from Rust's own
`compiler_builtins` for the bare-metal targets, which is what lets a `no_std` Rust binary link at all.
The C component's needs and the Rust runtime's needs overlap almost exactly. That is a reusable
finding: tier two is smaller than it looks, because the freestanding Rust runtime has already paid for
most of it.

### The trap: a Rust `memcpy` shim calls itself

Do not define the three symbols the runtime already owns. Here is why, and it cost a debugging session.

The obvious Rust implementation of `memcpy` is `core::ptr::copy_nonoverlapping`. **That lowers to a
call to `memcpy`.** So the shim calls itself, forever.

The symptom does not look like recursion. It looks like this:

```
  user thread ... killed: Data abort from a lower EL
    pc 0x4003a8   far 0x4fcff0   user sp 0x4fcff0   esr 0x92000047
```

A store fault at exactly `sp`, sixteen bytes below the bottom of the stack, in
`core::ptr::const_ptr::is_aligned_to` (a debug build spends a frame per `core::ptr` helper, so the
backtrace is all allocator plumbing). That reads unmistakably like "the stack is too small". It is
not: quadrupling the stack from 16 KiB to 64 KiB reproduced it exactly, sixteen bytes below the new
bottom, which is the tell. `compiler_builtins` avoids the trap by compiling with `#[no_builtins]`; a
program crate cannot.

The fix is also the smaller answer: shim `malloc` and `free`, and let the runtime keep the rest.

### Where `malloc` comes from

Milestone 27's untyped-backed `GlobalAlloc` (`user_rt::heap::UntypedHeap`, DECISIONS §22), wired to the
untyped region **the instance was built in**. Three consequences, all of them the point:

- The C heap is the process's own memory budget. There is no ambient allocator to leak into.
- A C leak exhausts that instance, visibly, and reaches no other process's memory.
- The single `Untyped::DESTROY` that reaps the corpse reclaims the heap along with everything else, so
  a restart loop is not a leak.

One real cost of the C ABI shows up here and cannot be shimmed away: **`free(p)` carries no size**,
while `GlobalAlloc::dealloc` requires the original `Layout`. So `malloc` stores the size in a 16-byte
header in front of the pointer it returns (16 because that is also the alignment `malloc` must
guarantee for any C type, so the payload stays aligned for free). Every C-to-Rust allocator bridge pays
this; it is worth knowing it is a property of `free`'s signature rather than a shortcut.

## The toolchain: bare-metal clang, one compiler for two ISAs

`user/build.rs` compiles `user/c/cseam.c` and hands the object to the linker for the `cshim` binary
only (`cargo::rustc-link-arg-bin=cshim=...`). No archive and no `ar`: one translation unit, one object,
straight onto the linker's command line. Every other program in the `user` package links exactly as
before, which keeps the foreign component from becoming everyone's problem.

**How clang is found, and why the check is a capability check.** The same discipline `xtask`'s
`llvm_tool` uses for `llvm-objcopy`: resolve the tool from a known list rather than hoping it is on
`PATH` under the right name, and fail loudly with what to install.

1. `$CRICKER_CC`, the escape hatch.
2. `/opt/homebrew/opt/llvm/bin/clang`, then `/usr/local/opt/llvm/bin/clang`. Homebrew's llvm keg is
   deliberately not linked onto `PATH` (it would shadow Apple's clang), so it is named directly.
3. `clang` on `PATH`. Debian and Ubuntu build every LLVM backend into their packages, so this is the
   usual CI answer.

Each candidate must have **both** the AArch64 and RISC-V backends, checked with `clang -print-targets`:

```
$ /usr/bin/clang -print-targets | grep -c riscv        # Apple clang, Xcode CLT
0
$ /opt/homebrew/opt/llvm/bin/clang -print-targets | grep -c riscv
4
```

**Apple's clang is rejected on purpose**, and it is worth being clear that this is not an oversight:
it compiles the aarch64 side perfectly well. Requiring both backends from *one* compiler even when
only one ISA is being built is DECISIONS §19 (architectural parity is a gate) applied to the
toolchain. A machine where the two architectures are compiled by two different clangs is a machine
where "it works on aarch64" has stopped predicting anything about riscv64, and the failure would
surface halfway through a build instead of at the front door.

Cost to a fresh clone: one dependency. `script/bootstrap` installs it (`brew install llvm` on macOS,
`apt-get install clang` on Debian), and from this milestone on `cargo build -p user` needs a
cross-capable clang. Without one, `user/build.rs` panics with installation instructions rather than
leaving the link to fail with a bare "undefined symbol: cseam_transform".

### The flags, and the one that is load-bearing

```
aarch64:  --target=aarch64-unknown-none-elf  -mgeneral-regs-only
riscv64:  --target=riscv64-unknown-none-elf  -march=rv64imac -mabi=lp64
both:     -ffreestanding -fno-pic -fno-stack-protector -Os -std=c11 -Wall -Wextra -Werror
```

- **`-mgeneral-regs-only` is not a size optimization, it is a correctness requirement.** Without it,
  clang happily vectorizes the component's byte loops: 53 vector-register operands appear in the
  object. Three independent reasons that breaks here, and any one is enough. The Rust target is
  `aarch64-unknown-none-softfloat`, which uses no FP/SIMD registers in its ABI. The kernel never
  touches `CPACR_EL1`, so FP/SIMD traps at EL0. And the context switch saves no FP state, so even if
  it did not trap, the registers would be corrupted across a preemption. Whichever of those bit first,
  the bug would be a confined component that fails for a reason having nothing to do with its logic.
- **`-mabi=lp64`, not `lp64d`,** because the Rust target is `riscv64imac` (no F or D extension). An ABI
  mismatch here would either be refused by lld or, worse, linked with arguments passed in registers the
  other side never reads.
- **`-fno-pic`** to match the bare targets' `static` relocation model.
- **`-ffreestanding`** so no hosted libc is implied. clang still supplies its own `stddef.h` and
  `stdint.h` from its resource directory, which is all the component includes.

## The confinement test, which is the milestone

`kernel/src/user::c_seam_tests`, both ISAs. Three processes are involved:

```text
  cwarden  budget + initrd + report + the C component's supervision endpoint
    |        holds GRANT, WITNESS_RO, and WITNESS_FAR mapped read/write in ITS OWN space
    |
    +-- cshim (instance N)   report endpoint, its own region's untyped, GRANT rw, WITNESS_RO ro
          |
          +-- cseam.c        a pointer and a length
```

`cwarden` is builder, supervisor, and checker at once. The checker role has to be a separate address
space from the C component, because a checker *inside* the faulting address space could only report
what that address space could see, which is exactly the thing under suspicion.

### The layout, and why there are two witnesses

```text
  cshim (the C component's process)        cwarden (the checker)
  ----------------------------------       ----------------------------------------
  0x0040_0000  text / rodata / data        0x0040_0000  text / rodata / data
  0x0050_0000  stack                       0x0050_0000  stack
  0x4000_0000  heap (malloc/free)          0x2000_0000  the initrd, read-only
  0x5000_0000  GRANT       1 page, RW <--> 0x5000_0000  the same frame, RW
  0x5000_1000  WITNESS_RO  1 page, RO <--> 0x5000_1000  the same frame, RW
  0x5000_2000  NOTHING (unmapped)          0x5000_2000  a DIFFERENT frame, RW
```

Two witnesses because there are two different claims, and neither implies the other:

- **`WITNESS_RO` is the same physical frame**, mapped read-only into the component and read/write into
  the warden. `cseam_overrun` writes `grant[len]`, which is this page's first byte. When it comes back
  unchanged, that is not "the store landed somewhere else": the page was right there, in the offender's
  own page tables, one byte past a pointer it legitimately held, and **the store did not happen.** This
  is the stronger of the two.
- **`WITNESS_FAR` is a different frame at the same virtual address.** `cseam_wild` writes
  `grant[len + 4096]`, an address the component has no mapping for at all and the warden does. When the
  warden's page is unchanged, that is the statement **a virtual address means nothing outside the
  address space that owns it**, which is the MMU claim itself, made concrete rather than assumed.

Both patterns are position-derived (`i*31+7` and `i*17+3`) and checked byte by byte, which is milestone
29's framebuffer discipline: a partial overwrite is detected, and a `memset` of any single value could
not pass.

### The five assertions, and the control that makes them mean anything

Every one is made from outside the faulting address space, after the component is dead.

1. **It faults**, rather than silently corrupting and continuing. The death message exists at all,
   carries `EVENT_FAULT`, and carries a non-zero kernel-stamped tid (DECISIONS §26.5: the kernel is the
   only sender on that endpoint, so the tid needs no badge).
2. **The fault is the bug we planted.** The address the kernel reports equals the address the C code
   computed. Without this the witness checks would be vacuous, because a crash on the way to the bug
   looks identical from the outside.
3. **`WITNESS_RO` is intact**, every byte.
4. **`WITNESS_FAR` is intact**, every byte.
5. **The restart works, and works means computes.** Three instances run in sequence: attempt 0
   overruns, attempt 1 goes wild, attempt 2 runs `cseam_transform` (uppercase the input, FNV-1a it,
   write both into the grant) and exits cleanly. The warden reads the output out of the shared grant
   and checks it against an **independent Rust implementation of the same definition**, so a restart
   that revives a process which merely reports for duty fails. The clean exit arrives as `EVENT_EXIT`
   and is not restarted, which is the other half of §26.3's "both events flow".

**The control.** Each misbehaving C function stores *inside* its grant first (`grant[0] = 0xC0`), and
that store must be visible in the warden's view. Without it, every witness assertion could be satisfied
by a process whose stores never worked at all, which would prove nothing. This is the single most
important line in the test.

The verdict is asserted for **equality** with the expected bitmap, not for containing the interesting
bits: a missing bit is what broken confinement looks like, and a superset would mean the checker started
answering a question nobody asked.

### What the machine actually printed

aarch64:

```
  user thread ... killed: Data abort from a lower EL
    pc 0x402178   far 0x50001000   esr 0x9200004f     <- overrun: permission fault on WITNESS_RO
  user thread ... killed: Data abort from a lower EL
    pc 0x402190   far 0x50002000   esr 0x92000047     <- wild:    translation fault, nothing mapped
```

riscv64:

```
  user thread ... killed: scause 0xf (code 15)
    pc 0x401786   stval 0x50001000                    <- overrun
  user thread ... killed: scause 0xf (code 15)
    pc 0x40179a   stval 0x50002000                    <- wild
```

**A cross-ISA difference, found here and worth recording.** aarch64's `ESR_EL1` distinguishes the two
bugs in its fault status code (`0x...4f` permission, `0x...47` translation); RISC-V's `scause` reports
both as `0xf`, Store/AMO page fault, with no permission-versus-translation distinction at all. Both
deliver the exact faulting byte address, which is what the test asserts on, so the difference costs
nothing today. It would matter to a **userspace pager**, which needs to tell "this page is absent, fetch
it" from "this page is protected, that is an error"; on RISC-V that requires reading the page tables
rather than the cause register. That is on the SUSPEND tracker's plate, not this milestone's.

## What the supervisor had to hold, and what it would rather have held

This is the part that feeds an open design fork, so it is written down precisely.

`cwarden` restarts its child, and to restart it must first **reap** it: the corpse is dead-until-reaped
(§26.4), and its region stays pinned until somebody says otherwise. Reaping is `Untyped::DESTROY`,
which needs `WRITE` on the region. **`WRITE` on a region is also exactly what builds a process from
it** (`RETYPE`, `RETYPE_OBJ`, `SPLIT`). There is no narrower right.

- **What it had to hold:** a full-rights untyped budget, for its whole life. From that it can split
  regions, make frames, make address spaces, make threads, make endpoints, and destroy regions. It
  needs the last one. Everything else came attached.
- **What it would have preferred:** a **reap-only right over one region it did not create**. That is
  enough to collect a corpse and return its pages, and not enough to build anything.
- **The alternative that exists today**, and why this milestone did not use it: milestone 22 phase
  B.2's proxy. Its supervisor (`subsup`) holds no memory at all and asks a construction sub-server
  (`spawner`) to reap on its behalf, so policy and authority sit either side of an IPC boundary. That
  is the right answer for a system's init, where the whole point is that init can no longer build. It
  is the wrong answer *here*, because it moves the requirement behind an IPC hop and the requirement is
  the interesting part. A spike should make its requirements visible.

Nothing was invented to work around this: no new capability, no new method, no new syscall. The
concrete requirement, for whoever decides the fork, is one sentence: **a supervisor needs exactly
`DESTROY` on one region it did not create.** Whether that is a rights bit split out of `WRITE` or a
distinct `Untyped::REAP` changes the rights model and the syscall surface, which makes it a decision
rather than an implementation detail. DECISIONS §26's phase-B block already records it as one.

## Honest caveats: what a spike does not prove

The whole value of doing this cheaply is that it fails early. The corollary is that it does not prove
much about the expensive thing.

- **A throwaway component is not a vendor component.** Untested here: a real build system (this is one
  `clang -c`, not autotools, CMake, or `build.zig`), multiple translation units and their link order,
  headers we do not control, and API churn across upstream versions. Milestone 29's libghostty-vt is a
  **tier-one** (freestanding) component by design, which is the cheapest possible next step up from
  here, and that sequencing is deliberate rather than lucky.
- **The libc list is this component's list.** A component wanting `errno`, `assert`, `stdio`, locales,
  `setjmp`, floating point, or thread-local storage asks a different question, and some of those
  (`stdio`, TLS) are tier-three questions wearing a tier-two coat. The rule from DECISIONS §31 stands:
  if a symbol cannot be answered without POSIX semantics, that is a finding, and the answer is to
  choose a component that does not need it.
- **The C ABI surface is one function shape.** See "what crosses" above.
- **Nothing here is verified.** DECISIONS §18's proof toolchain does not reach C and never will. The C
  is confined, not correct. That is the whole point, and it is also the limit of the claim: this
  milestone says nothing about the component's behaviour, only about its blast radius.
- **The grant is one page and the workload is trivial.** No claim is made about the cost of the seam at
  volume: no benchmark, no copy-avoidance story, no measurement of what a C component costs versus a
  Rust one. A real component gets that treatment when there is a real component.

## See also

- [Rust `std` on the native ABI](std.md) — where the three libc tiers are recorded, and the heap this
  `malloc` is built on.
- [Supervision](supervision.md) — the fault endpoint this leans on, and the five-word message.
- [Trusted init](trusted-init.md) — milestone 22 phase B.2's proxy shape, the alternative to the
  authority this warden holds directly.
- [The native ABI](abi.md) — the capability contract the shell speaks and the C cannot.
- [The framebuffer contract](framebuffer-contract.md) — the two-witness discipline this test borrows.

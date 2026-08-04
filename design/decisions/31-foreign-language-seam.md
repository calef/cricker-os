# 31. The foreign-language seam: C holds no capabilities and makes no syscalls (milestone 36)

**Status: DECIDED.**

**Built 2026-07-29**, both ISAs, in QEMU. *(Section number claimed while two other lanes were open;
if one of them also took 30, this is the entry to renumber. It depends on §4 rule 3, §15, §16, §22,
and §26, and nothing depends on it yet.)*

A memory-unsafe C component, compiled by bare-metal clang, confined by the kernel like any other
process, faulting on a deliberate out-of-bounds write, and restarted by its supervisor. The component
itself (`user/c/c_seam.c`, 150 lines) is throwaway on purpose: **what this milestone de-risks is the
seam**, before milestone 29's libghostty-vt rung and milestone 23's vendor-component claim owe
anything to another project's toolchain and API churn. Concept note: notes/c-seam.md.

**Why C is the right thing to run, not a dilution of the thesis.** §14 promises a verified core that
confines unverified workloads. C is the most unverified workload available, so it is the strongest
available test rather than a compromise, and the contrast with a monolith is concrete: in-kernel C
means one bad index is a kernel compromise (the peer project Atom keeps FAT32, AHCI, and xHCI in the
kernel today); confined C means one bad index scribbles its own grant and gets restarted. Isolation
here is enforced by mechanisms that do not know what a language is: page tables, unforgeable
capabilities, the DMA validator, the IOMMU.

## The seam's rules, which are the decision

1. **The C makes no syscalls and holds no capabilities.** A Rust `user_rt` shell
   (`user/src/c_shim.rs`) holds every capability and performs every IPC; the C is called over the C
   ABI and gets a pointer and a length. This is not a request the C is trusted to honour, it is a
   property of what it can name: a syscall needs a capability slot, and the C never sees one. So a
   foreign component **cannot widen the kernel's syscall surface** (§4 rule 3), and the confinement
   claim is exactly as narrow as it should be: the C can corrupt memory inside a grant the shell
   already had, and nothing else.
2. **What crosses is scalars and buffers only.** `(u8*, usize) -> u32`. No structs, no callbacks into
   Rust, no ownership transfer, no error type. The layout of the shared page is agreed by a comment in
   both languages rather than generated bindings, which is the right trade for one page and would not
   be for a real API. Same sans-IO shape RedoxFS's `Disk` trait already uses (§27), across a language
   boundary instead of a trait boundary.
3. **The libc is two symbols, `malloc` and `free`.** Tier two of the roadmap's three tiers
   (freestanding / a handful of symbols / full POSIX; design/roadmap/36-foreign-component.md). The C object references five (`malloc`, `free`,
   `memcpy`, `memset`, `strlen`, identical on both ISAs at every optimization level, with no
   compiler-rt helper and no `__stack_chk_fail`), and the linker demands only two, because
   `compiler_builtins` already supplies the other three weakly for the bare targets. **Tier three is
   not walked**: a component needing `open`, `fork`, `socket`, or threads needs a real libc port, which
   is §15's "later, if ever" road, and saying so is what keeps this from becoming that project.
4. **`malloc` comes from the process's own untyped budget** (§22 / milestone 27's `UntypedHeap`), wired
   to the very region the instance was built in. So the C heap is the process's own memory, a C leak
   exhausts that instance and nothing else, and the single `Untyped::DESTROY` that reaps the corpse
   reclaims the heap with it. `free` carries no size while `GlobalAlloc::dealloc` needs a `Layout`, so
   the shim stores a 16-byte header; that is a real and unavoidable cost of the C ABI, not a shortcut.
5. **Bare-metal clang, one compiler for both ISAs, resolved rather than assumed.** `user/build.rs`
   looks for a clang whose `-print-targets` lists **both** aarch64 and riscv64 (`$CRICKER_CC`, then
   Homebrew's llvm keg, then `clang` on `PATH`) and fails with installation instructions otherwise, the
   same discipline `xtask`'s `llvm_tool` uses for `llvm-objcopy`. Requiring both backends from one
   compiler even when building one ISA is §19 applied to the toolchain: a machine where the two
   architectures are compiled by two different clangs is a machine where "works on aarch64" stops
   predicting anything about riscv64. Apple's clang is therefore **rejected on purpose** (no RISC-V
   backend), and `script/bootstrap` grew `brew install llvm` / `apt-get install clang`.

## What the confinement test proves, and how each claim is proven rather than assumed

`kernel/src/user::c_seam_tests`, both ISAs. `c_confiner` builds the shim, supervises it, and holds
the witness pages; every assertion is made from **outside** the faulting address space after the
component is dead, because a checker inside it could only report what that address space could see.

- **It faults.** The death message exists at all, with `EVENT_FAULT` and a non-zero kernel-stamped tid.
- **The fault is the planted bug.** The kernel's reported fault address equals the address the C code
  computed. Without this the witness checks would be vacuous: a crash on the way to the bug would look
  identical.
- **Nothing outside the grant changed, proven twice because there are two different claims.**
  `WITNESS_RO` is the **same physical frame** mapped read-only into the component and read/write
  into the confiner, so an unchanged page is not "the store landed elsewhere"; the page was
  reachable and the store did not happen. `WITNESS_FAR` is a **different frame at the same virtual
  address**, which is the statement that a virtual address means nothing outside the address space
  that owns it. Both patterns are position-derived and checked byte by byte (milestone 29's
  two-witness discipline).
- **The restart works.** Three instances run in sequence: two crash, the third computes a checksum
  and a transform in C, writes them into the shared grant, and exits cleanly. The confiner checks
  that output against an independent Rust implementation of the same definition, so a restart
  producing a process that merely reports for duty fails. The clean exit arrives as `EVENT_EXIT` and
  is **not** restarted, which is the other half of §26.3.
- **The control that makes the rest mean anything.** Each misbehaving C function stores *inside* its
  grant first, and that store must be visible. A process whose stores never worked would satisfy every
  witness check while proving nothing.

## What authority the supervisor had to hold, and what it would have preferred

The open fork this feeds. `c_confiner` is builder, supervisor, and checker in one process, and the
reason is authority, not convenience: **reaping a corpse needs `WRITE` on the region it lives in,
which is the same right that builds one.** So a supervisor that restarts its child holds
construction authority, or proxies the reap through something that does.

- **What it had to hold:** a full-rights untyped budget, for its whole life. From that it can
  `SPLIT` a region, `RETYPE` frames and kernel objects, build any address space and any thread, and
  `DESTROY` regions. The reap needs the last of those; everything else came attached.
- **What it would have preferred:** a **reap-only right** over the instance region and nothing more.
  That is enough to collect a corpse and return its pages, and it is not enough to build a process.
- **The alternative that exists today and why this milestone did not use it.** Milestone 22 phase
  B.2's proxy: a supervisor holding no memory that asks a construction sub-server to reap
  (`sub_server_supervisor` -> `spawner`). That is the right answer for a system's init, where the point is that init
  can no longer build. It is the wrong answer here, because it moves the requirement behind an IPC hop
  and the requirement is the interesting part. **The concrete requirement, for whoever decides the
  fork: a supervisor needs exactly `DESTROY` on one region it did not create.** Neither a rights bit
  on `WRITE` nor an `Untyped::REAP` method was invented here; that is a rights-model and
  syscall-surface decision, and §26's phase-B block already records it as one.

## The honest caveats, including what a spike does not prove

- **A throwaway component does not prove a vendor component.** What is untested: a real build system
  (this is one `clang -c` invocation, not autotools, CMake, or `build.zig`), multiple translation units
  and their link order, headers we do not control, a component that wants `errno`, `assert`, `stdio`,
  locales, `setjmp`, floating point, or thread-local storage, and API churn across upstream versions.
  Milestone 29's libghostty-vt is a tier-one (freestanding) component by design, which is the cheapest
  possible next step up from here, and that sequencing is deliberate.
- **The C ABI's surface is one function shape.** No struct passed by value, no varargs, no callback
  from C into Rust, no C++ (name mangling, exceptions, static initializers, `operator new`), and no
  bitfield or enum-width question. Each of those is a real seam decision this spike did not have to
  make.
- **Nothing here is verified.** §18's proof toolchain does not reach C and never will; the C is
  confined, not correct. That is the whole point, and it is also the limit of the claim.
- **`-mgeneral-regs-only` is load-bearing on aarch64, and the reason is worth keeping.** Without it
  clang vectorizes the component's byte loops into NEON (53 vector-register operands in the object).
  The Rust target is `-softfloat`, the kernel never enables FP/SIMD for EL0, and the context switch
  saves no FP state, so vector registers in a confined component would be a trap or a corruption
  depending on which of those two bit first.
- **A cross-ISA difference in fault reporting, found here.** aarch64's `ESR_EL1` distinguishes the two
  bugs (`0x9200004f` permission, `0x92000047` translation); RISC-V's `scause` reports both as `0xf`,
  Store/AMO page fault, with no permission-versus-translation distinction. Both deliver the exact
  byte address, which is what the test asserts on, so the difference costs nothing today. It would
  matter to a userspace pager, which is on the SUSPEND tracker.
- **A trap worth one line, because the next person will hit it.** The obvious Rust `memcpy` shim is
  `core::ptr::copy_nonoverlapping`, which *lowers to a call to `memcpy`*: the shim calls itself. The
  symptom is a store fault exactly at `sp` at whatever stack depth the process was given, which reads
  like a stack-size problem and is not one. `compiler_builtins` avoids it with `#[no_builtins]`; a
  program crate cannot, so the right answer is to not define the three symbols the runtime already
  owns.

**Cost to a fresh clone:** one dependency. `script/bootstrap` installs a cross-capable clang, and from
this milestone on `cargo build -p user` needs one; without it `user/build.rs` fails with what to
install rather than with an undefined symbol. The roadmap already accepted that cost for Zig at 29, so
paying it here, where the component is disposable, is the point of doing the seam first.

# LLVM

The machine that turns our Rust into aarch64. We have been using it all day.

## The three-phase idea

With M languages and N target architectures, the naive approach needs **M × N** compilers.
That doesn't scale, and historically it didn't.

```
frontend    →    LLVM IR    →    optimizer    →    backend
(per language)  (universal)   (target-agnostic)  (per architecture)
```

Put a well-specified **intermediate representation** in the middle and you need **M + N**
pieces. Write a frontend and you inherit every architecture LLVM supports. Write a backend
and you inherit every language.

(The idea isn't original to LLVM. GCC has the same shape internally. LLVM's innovation was
making it a **reusable library**, which is the whole story. See below.)

Clang, **Rust**, Swift, Zig, and Julia are all frontends that meet in the same IR.

## rustc is a frontend

The part worth internalizing. `rustc` parses Rust, type-checks it, borrow-checks it,
monomorphizes generics, then **emits LLVM IR and hands off**. LLVM optimizes and generates
the actual ARM instructions.

**Nobody on the Rust team wrote an aarch64 code generator.** That is why
`aarch64-unknown-none-softfloat` works at all: Rust inherited a battle-tested ARM backend,
the same one Clang uses, for free.

## Our actual pipeline

Everything below the second arrow is LLVM.

```
Rust source
   │  rustc: parse, type-check, borrow-check, monomorphize
   ▼
MIR                      (Rust's own IR. The borrow checker lives here.)
   │  rustc: lower
   ▼
LLVM IR                  ← the universal interchange format
   │  LLVM: optimize (target-independent)
   ▼
LLVM IR, better
   │  LLVM aarch64 backend: instruction selection, register allocation, scheduling
   ▼
aarch64 machine code (.o)
   │  rust-lld  +  our link.ld
   ▼
kernel.elf
   │  llvm-objcopy
   ▼
kernel.img
```

The borrow checker is Rust's. Essentially everything after it is LLVM. When `cargo build`
feels slow, most of that time is LLVM.

## Seeing it

```bash
cargo rustc -p kernel --target aarch64-unknown-none-softfloat -- --emit=llvm-ir
```

Our `ec_name()` from `exceptions.rs` comes out as:

```llvm
define internal fastcc { ptr, i64 } @..._6kernel4arch7aarch6410exceptions7ec_name(i64 %class) {
start:
  switch i64 %class, label %bb17 [
    i64 21, label %bb13      ; 0x15 = SVC
    i64 37, label %bb7       ; 0x25 = data abort, same EL
    i64 38, label %bb6       ; 0x26 = SP alignment fault
    ...
```

A Rust `match` became an LLVM `switch`. And at the top of the file:

```llvm
target triple = "aarch64-unknown-none"
```

rustc told LLVM what machine to aim at, and LLVM did the aiming.

**Compare a function's IR against `cargo xtask objdump`.** Watching the same function at both
levels is the fastest way to build intuition about what the compiler is actually doing for
you.

## Five places LLVM has already shaped this project

**`rust-lld` linked the kernel.** LLVM's linker. It is why we produced an aarch64 ELF on a Mac
without installing a cross-binutils toolchain. It is cross-target by design; GNU `ld` is not.

**`llvm-objcopy` makes the flat image.** We resolve it out of the rustup sysroot in
`scripts/qemu-runner.sh`. See [boot-protocol.md](boot-protocol.md).

**LLVM's integrated assembler parses our `.s` files.** `global_asm!` hands the text to LLVM,
not to GNU `as`. That is exactly why `image_header.s` writes `.long 0x644d5241` instead of
`.ascii "ARM\x64"`: escape handling differs between the two assemblers, and the entire boot
process depended on those four bytes.

**The `softfloat` in the target triple is an instruction to LLVM**: do not emit FP/SIMD
instructions. Enforced in the backend. See [aarch64.md](aarch64.md).

**[design/fat-binaries.md](../design/fat-binaries.md) is really about controlling LLVM's
codegen.** `-C target-feature=+lse` tells LLVM whether to emit a single `CAS` or an
`LDXR`/`STXR` retry loop. `outline-atomics` is an LLVM feature. The open question in that doc
is fundamentally "how do we ship more than one of LLVM's answers in one binary."

## Why it won

Chris Lattner started it as a grad project at UIUC around 2000. Apple hired him and built
Clang on it, wanting off GCC.

GCC has the same three-phase structure internally. The difference: GCC was **deliberately
architected to be hard to reuse as a library**, because Stallman was concerned about
proprietary frontends and backends bolting onto it. LLVM is a permissively-licensed set of
libraries you can link into anything.

That single decision is why LLVM ate the world. Once the compiler is a library, you get IDE
autocomplete (clangd), sanitizers (ASan, TSan), JITs, static analyzers, and a linker, all on
one shared infrastructure.

The name "Low Level Virtual Machine" was officially retired years ago. It isn't one and never
really was.

---

*Add to this file as new toolchain details come up.*

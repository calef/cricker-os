# Rust `std` on the native ABI

*(Milestone 27. The first wall an application hits on cricker-os was "no std": you could write a
`no_std` binary against `crates/user_rt`, and nothing else. This milestone makes ordinary Rust,
`Vec` and `String` and `println!` and `Instant`, compile and run on the capability ABI. See
DECISIONS.md §22 for the decision and why; notes/abi.md for the ABI it binds to.)*

The shape is **Hermit's, not Redox's**. Hermit implements std's platform layer directly on a
non-POSIX unikernel ABI; Redox writes a POSIX C library (relibc) first and puts std on top of that.
We took the native road: there is no errno, no fd table, no `open`, no `fork` under our `sys`
backend, because the OS does not have them and std does not actually need them to run a workload
that stays off files and sockets. That is the whole point of having done the native ABI first
(DECISIONS §14, §15): std widens "runs real workloads" from hand-built `no_std` binaries to most of
crates.io, without smuggling in the POSIX assumptions the ABI deliberately excludes.

## What a std program is given

A std program is an ordinary cricker-os ELF (notes/abi.md §3): entered at `_start`, linked at
`0x40_0000`, cspace populated by its parent. std's runtime contract needs two things, and the ABI's
out-of-band convention (notes/abi.md §4) grants them at fixed slots:

- **slot 0: an untyped budget.** The global allocator draws heap pages from it lazily via
  `untyped::MAP`, one page per invoke, at `0x4000_0000`. This is the same untyped-backed heap the
  `allocdemo` workload proved (`crates/uheap` algorithm, host-tested), restated inside std because
  std cannot depend on an out-of-tree crate.
- **slot 1: an endpoint with WRITE.** `stdout` and `stderr` SEND here, 16 bytes per message (w0 =
  byte count, w1|w2 = the bytes, little-endian). std's own `LineWriter` batches user writes; the
  receiver reassembles.

A program that never allocates or prints never touches the slots it does not use.

## The PAL surface, and what each piece binds to

The backend lives in `patches/std-cricker/overlay/std/src/sys/` and is materialized into a patched
std by `cargo xtask std-src`. Each file binds one std concept to the ABI:

| std concept | cricker binding |
|---|---|
| `GlobalAlloc` | `untyped::MAP` from slot 0, grow-on-demand (`sys/alloc/cricker`) |
| `stdout` / `stderr` | `endpoint::SEND` on slot 1 (`sys/stdio/cricker.rs`) |
| `Instant`, `SystemTime` | the virtual counter, `CNTVCT_EL0` / `rdtime` (`sys/time/cricker.rs`) |
| `panic!` | print, then `brk`/`ebreak`: a fault the kernel attributes. No unwinding. |
| `thread::spawn` | `Unsupported` in phase one; `sleep`/`yield` are real |
| `fs`, `net` | `Unsupported`, honestly, until capability-granted servers exist |
| `HashMap` seed | splitmix64 from the counter (`sys/random/cricker.rs`), **not** cryptographic |
| `std::env::consts::OS` | `"cricker"` (patched into `env_consts.rs`) |

The syscall glue (`sys/pal/cricker/rt.rs`) is a deliberate twin of `crates/user_rt`: the same
`svc`/`ecall` wrappers, restated because std cannot depend on the crate. The ABI **constants** are
not restated: `abi.rs` is generated verbatim from `crates/abi` by `std-src`, so the numbers cannot
drift. Likewise `uheap.rs` is generated verbatim from `crates/uheap`, so the host-tested heap
algorithm is the only heap algorithm.

## The toolchain: build-std against a patched rust-src

There is no crate to adopt; the deliverable IS the PAL, plus the machinery to build it. Rust's
`-Zbuild-std` compiles std from source, and it finds that source in the sysroot of the rustc it
invokes. So a **patched std means a toolchain whose sysroot is patched**. `cargo xtask std-src`
builds one:

1. **Hardlink-clone the real nightly** (`cp -al` of `bin` and `lib`). Blocks are shared, so the
   clone costs almost no disk. rustc resolves *this* directory as its sysroot (it derives the
   sysroot from the location of `librustc_driver`, which the clone puts inside the farm; a symlink
   farm does not work, because the symlink resolves back to the real toolchain, which was the first
   thing tried and measured).
2. **Replace the `src` subtree with a real copy** (independent inodes), so patching it never
   touches the shared rustup toolchain.
3. **Patch that copy**: drop in the overlay PAL files, generate `abi.rs`/`uheap.rs`, and insert a
   `target_os = "cricker"` arm into std's `cfg_select!` dispatchers (pal, alloc, stdio, random,
   thread, time, io/error, thread_local storage and guard) plus `env_consts` and the
   `restricted_std` chain in std's `build.rs`.
4. **Link it** as the `cricker-dev` toolchain (`rustup toolchain link`).

`cargo xtask user-std` then builds the `hellostd` demo for both custom targets against it. The build
sets `RUSTUP_TOOLCHAIN=cricker-dev` explicitly rather than `+cricker-dev`, because the cargo proxy
that launched xtask already exports `RUSTUP_TOOLCHAIN=nightly`, which would override a `+` selector
and silently build std from the *unpatched* sysroot.

`std-src` is idempotent: a stamp of all inputs (the toolchain version, the ABI/heap crates, the
target specs, every overlay file, and a patch-logic version) guards the rebuild, so a warm farm and
its build-std cache survive across runs and only a PAL change forces std to recompile.

### The target specs

`targets/{aarch64,riscv64}-unknown-cricker.json`, built with `-Zbuild-std` and `-Zjson-target-spec`.
The load-bearing fields:

- `"os": "cricker"` selects our `sys` backend through every dispatcher.
- `"panic-strategy": "abort"` means unwinding machinery is never even linked; `panic!` prints and
  faults.
- `"singlethread": true` turns off `target_has_threads`, so std uses its `no_threads` sync
  primitives and single-`static` TLS. This is honest for phase one (one thread of execution per
  process, `thread::spawn` is `Unsupported`); it flips off when real threads arrive.
- softfloat (aarch64 `-neon`, riscv `lp64`) matches EL0/U-mode with no FP save area, the same
  choice the `no_std` `user` crate makes.

The build also passes `-Zbuild-std-features=compiler-builtins-mem` to supply `memcpy`/`memset` for
the bare target.

## Honest caveats (what is Unsupported, and why)

- **`thread::spawn` returns `Unsupported`.** The kernel has everything it needs (retype a TCB,
  configure it, start it); what does not exist yet is the std-side plumbing that makes the result
  safe: a TLS story, park/unpark on a kernel primitive, join. Phase one ships without it rather than
  shipping it wrong. The sync primitives are std's single-threaded `no_threads` implementations, and
  the allocator's spinlock is uncontended today but stays correct under future preemption.
- **`fs` and `net` return `Unsupported`.** No file capability points anywhere until milestone 32's
  FS server; no socket until milestone 30's network stack. Both back std's `unsupported` paths, and
  the demo checks that they refuse with `ErrorKind::Unsupported` rather than pretend.
- **`SystemTime` is monotonic-since-boot, not wall-clock.** No RTC, no NTP, so "system time" honestly
  measures "since this machine came up". Differencing two `SystemTime`s gives a correct duration;
  reading a calendar date gives 1970 plus uptime, which is the truth available.
- **`std::random` is not cryptographic.** splitmix64 seeded from the virtual counter: fine for
  `HashMap`'s seeds and `sort_unstable`'s pivots, predictable to anyone who can guess boot-relative
  time. Never for keys or tokens. A real entropy story (a virtio-rng service) would replace the file.
- **stdout and stderr share one endpoint**, so they interleave by 16-byte chunk. One endpoint is what
  the contract grants today; milestone 28's terminal contract owns fixing it.
- **The `std-src` patches are string-anchored to the pinned nightly's std internals.** A rustc bump
  that reshapes a `cfg_select!` dispatcher fails loudly in `std_patch_dispatch` ("anchor not found"),
  which is the intended tripwire: re-point the anchor, do not paper over it. `rust-toolchain.toml`
  pins the channel; the coupling is the price of build-std against a std we do not fork.

## The proof

`user-std/src/main.rs` is an ordinary Rust program, no `no_std`, no attributes, no `unsafe`. It
exercises `Vec` (10,000-element collect against the untyped heap), `String`, `HashMap` (the random
seed), `Instant` (asserted monotonic and advancing), and the honesty of `fs`/`net`. Its stdout is a
fixed, deterministic transcript. The kernel test `std_tests::a_whole_std_program_runs_on_the_native_abi`
(`kernel/src/user.rs`) spawns it with the two grants, reassembles the byte stream off the endpoint,
and compares it byte for byte, on **both** ISAs out of each arch's own initrd (the parity gate,
DECISIONS §19). `cargo xtask test` builds the demo for both targets first, so both initrds carry it.

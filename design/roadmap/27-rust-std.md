# 27. Rust `std` on the native ABI

**Status: BUILT.**

**In brief.** A custom target whose `std` builds: `Vec`, `String`, `println!`, `Instant`, allocation from the process's own untyped, stdio over the console endpoint, `fs`/`net` honestly `Unsupported` until capability-granted servers back them

**Why it matters.** **widens "runs real workloads" by orders of magnitude**: the pool of programs that build for cricker-os becomes "most Rust code that doesn't touch fs/net", and milestone 23's components become writable by people who are not kernel people. Grows toward general purpose (notes/why-not-general-purpose.md) without smuggling POSIX: the `sys` layer maps to capabilities directly, no fork, no open-by-path

**Built 2026-07-28, both ISAs green; phase two complete 2026-07-29.** std's platform layer runs
directly on the capability ABI (Hermit's shape); a real std program (`Vec`, `String`, `HashMap`,
`println!`, `Instant`) is spawned and checked byte for byte on aarch64 and riscv64. Phase two bound
**`std::net`** to net_stack's socket contract and **`std::fs`** to the §27 FS service, so the same binary
now has three behaviours chosen by its grants alone: a filesystem if it holds a directory capability,
a network if it holds a `Stack` endpoint, and honest `Unsupported` for whichever it was not given.
`std::fs`'s interesting half is what a path *means* with no global namespace: "under the directory I
hold", so an absolute path or a `..` is refused as un-nameable rather than served. `thread::spawn`
remains `Unsupported`, as do the operations no contract verb backs (creating or truncating a file,
directory iteration, permissions, symlinks). See notes/std.md and DECISIONS §22.

**Deliverable.** A custom rustc target (`aarch64-unknown-cricker` / `riscv64-unknown-cricker`,
`-Zbuild-std` against a target spec first, a real target later if ever warranted) whose `std`
compiles and links against the capability ABI (notes/abi.md). Concretely: implement std's
Platform Abstraction Layer (PAL, `library/std/src/sys/pal/*`), a **native** cricker-os backend
over what a process already has, not a libc shim under the Unix one. Allocation draws from the process's own untyped
(the `user_rt` heap growing into a real `GlobalAlloc`); `stdout`/`stderr` SEND to the console
endpoint by slot convention; `Instant`/`SystemTime` read the virtual counter; `panic!` aborts (a
fault the kernel reports) before unwinding is ever attempted; `thread::spawn` retypes a TCB, or
returns `Unsupported` in phase one; `fs` and `net` return `Unsupported`, honestly, until
capability-granted servers exist to back them.

**Why.** The first wall an application hits on cricker-os is "no std" (the note
why-not-general-purpose.md names it), and milestone 23's vendor-component ambition needs
components writable by people who are not kernel people. `std` on the native ABI widens "runs
real workloads" from hand-built `no_std` binaries to most of crates.io that stays off fs and
net, without smuggling the POSIX assumptions the ABI deliberately excludes: no fork, no
open-by-path, no ambient anything. Paths, when they come, name capabilities.

**Prior art and reuse.** Hermit is the closest shape (std's pal implemented directly over a
non-POSIX unikernel ABI) and the model to follow; Fuchsia did the same at scale. Redox took the
other road, std via relibc (a POSIX shim first), which is exactly the "later, if ever"
DECISIONS §15 already prices at nothing. Code to use: rustc's own `build-std` machinery and
target-spec JSON; there is no crate to adopt, because the deliverable IS the pal. Mistake to
avoid: an errno-shaped `sys` layer that makes `std` work by pretending the OS is Unix.

**Sequencing.** After 19 (the ABI, done) and object revocation (done); independent of 16 and 22;
feeds 23 directly. **Effort: unpriced** (it depends on another project's toolchain and API, which
the history here cannot bound). Off the thesis path, like 20 was: a reach the demonstrator earns.

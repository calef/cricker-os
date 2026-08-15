# 36. A foreign-language component, seam first (spike; feeds 29 and 23)

**Status: BUILT.**

**In brief.** Prove the FFI seam end to end with a *minimal* C component before committing to a large one: bare-metal clang for both bare targets in the build, a Rust `user_rt` shell that holds every capability and does every syscall while the C code gets plain buffers over the C ABI (so the §4 surface does not widen), and only the handful of libc symbols the component actually needs, with `malloc` on milestone 27's untyped-backed `GlobalAlloc`. The deliverable that matters is one test: a deliberate out-of-bounds write in the C code faults the process, touches nothing outside its grant, and its supervisor restarts it. **Built, DECISIONS §31, both ISAs**: clang capability-checked for both backends from one compiler (Apple's is rejected: no RISC-V), `c_shim` holds every capability so the C holds none, the libc turned out to be **two** symbols not five (`compiler_builtins` already supplies the rest), and two witnesses prove the confinement (a read-only page that is the *same physical frame*, and a different frame at the same virtual address). notes/c-seam.md

**Why it matters.** **the thesis in one assertion.** Memory-unsafe foreign code is not a dilution of "a verified core that confines unverified workloads", it is the strongest available demonstration of it: the more unverified the component, the more the confinement has to prove. It also de-risks 29's libghostty-vt rung and 23's vendor-component claim *before* we owe anything to another project's toolchain or API churn

**DONE 2026-07-29**, both ISAs, in QEMU. DECISIONS §31; concept note notes/c-seam.md.

All four deliverables landed as specified, and the two that produced findings are worth reading before
the next foreign component:

1. **Toolchain.** `user/build.rs` compiles `user/c/c_seam.c` with a clang resolved from a candidate
   list and *capability-checked* (`-print-targets` must list both aarch64 and riscv64), object
   handed to the linker for the `c_shim` binary only. One compiler for both ISAs is §19 applied to
   the toolchain, which means **Apple's clang is rejected on purpose** (no RISC-V backend) even
   though it would compile the aarch64 half. `script/bootstrap` grew `brew install llvm` / `apt-get
   install clang`, and the CI clippy job grew the same, since it clippies `user`.
2. **Linkage.** `c_shim` (Rust) holds every capability and makes every syscall; the C gets `(u8*,
   usize)` and returns a scalar. The syscall surface did not change, and could not have: the C
   cannot name a capability slot.
3. **libc.** The object demands five symbols; the linker demands **two** (`malloc`, `free`), because
   `compiler_builtins` already supplies `memcpy`/`memset`/`strlen` weakly for bare targets. **Do not
   shim the other three:** the obvious Rust `memcpy` is `copy_nonoverlapping`, which lowers to a call to
   `memcpy`, so it calls itself, and the symptom is a store fault at `sp` that reads like a stack-size
   problem at any stack size. `malloc` is milestone 27's untyped heap on the instance's own region, so
   one `DESTROY` reclaims it.
4. **The test.** `c_seam_tests`, both ISAs: two out-of-bounds writes (one byte past into a read-only
   page that is the *same physical frame* the confiner holds read/write; one page past into an
   address the component has no mapping for and the confiner does), both fault at exactly the
   address the C computed, both leave a position-derived witness pattern intact byte for byte, and
   the third instance does real C work whose output is checked against an independent Rust
   computation. The control that makes it mean anything: each bug stores *inside* its grant first,
   and that store must be visible.

**The fork this fed, stated concretely.** The confiner is builder, supervisor, and checker in one
process, because reaping needs `WRITE` on the region and `WRITE` is also what builds one. **A
supervisor needs exactly `DESTROY` on one region it did not create**, and nothing narrower exists.
Milestone 22 phase B.2's IPC proxy is the workaround that exists today; this spike deliberately did
not use it, so the requirement is visible in one program instead of hidden behind a hop.

**What it does not prove**, recorded so 29 and 23 do not inherit false confidence: one `clang -c` is
not a build system, one translation unit is not a link order, this component's five symbols are not
another's, and confined is not correct. Sequencing holds: libghostty-vt is tier one (freestanding),
which is the cheapest step up from here.

**Added 2026-07-29, from calef's question: can we run user services in other languages, like a C
FAT32 that a monolith would have put in the kernel?** The answer is yes, and the roadmap already
commits to one (libghostty-vt, Zig, at 29). This item exists so the *seam* is proven by something
tiny before a large foreign component depends on it.

**Why the language does not matter to the confinement.** Isolation here is enforced by mechanisms
that are entirely language-agnostic: MMU page tables (proved), unforgeable capabilities (proved),
the DMA validator (proved, milestone 35), and the IOMMU. A C component in a confined process can
corrupt its own address space and reach nothing else, and when it dies §26's fault endpoint tells
its supervisor, which restarts it (the tree milestone 22 phase B built). That inverts the usual
worry: memory-unsafe C is not a problem for the thesis, it is the best demonstration of it. The
contrast with a monolith is the whole argument, and it is concrete rather than rhetorical: in-kernel
C means one bug is a kernel compromise (the peer project Atom keeps FAT32, AHCI, and xHCI in the
kernel today); confined C means one bug scribbles its own grant and gets restarted.

**Deliverable, deliberately small.**

1. **Toolchain in the build.** Bare-metal clang cross-compiling for both targets, driven from the
   build the way the rest of userspace is; `script/setup` grows a dependency. The roadmap already
   accepts this cost for Zig at 29, so pay it once, here, where the component is throwaway.
2. **The linkage shape, which must not widen the syscall surface.** A Rust `user_rt` outer shell
   holds every capability and performs every IPC; the C logic is linked in and called over the C ABI
   with plain buffers and makes **zero syscalls**. This is the same sans-IO shape RedoxFS's `Disk`
   trait already uses, just across a language boundary instead of a trait boundary.
3. **The libc question, answered by tier.** Shim only the symbols the component actually needs
   (`memcpy`, `memset`, `strlen`, `malloc`/`free`), with `malloc` backed by milestone 27's
   untyped-backed `GlobalAlloc` (`crates/user_heap` plus `user_rt::heap`).
4. **The test that is the point.** A deliberate out-of-bounds write in the C code must fault the
   process, leave everything outside its grant untouched, and be restarted by its supervisor.

**The line this does not cross.** C dependencies come in three tiers: *freestanding* (no libc,
fixed buffers, no alloc: libghostty-vt, littlefs) is easy; *a handful of symbols* is tractable and
is what this spike proves; *full POSIX* (`open`, `fork`, `socket`, threads) needs a real libc port,
which is the relibc road DECISIONS §15 prices at "later, if ever" and Redox took. Tiers one and two
only. A component wanting the third is a different and much larger project, and saying so here is
what keeps this from becoming one.

**Candidates, and the honest ranking.** Bring in a foreign language only where the foreign
implementation genuinely beats the Rust option. **libghostty-vt** is the roadmap's pick and clears
that bar (a mature VT engine with scrollback and reflow; `vte` is a parser only). **HarfBuzz** if
`rustybuzz` proves insufficient for 33's text shaping. **SQLite** is the canonical "C you cannot
beat" but is tier three. **doomgeneric** has real demonstrator value (memory-unsafe C game,
capability-confined, on a verified core) and Atom already vendored it, so we would be following
rather than leading. **FAT32, the question that prompted this, is a weak first candidate**: RedoxFS
already provides a better filesystem, `no_std` Rust FAT crates exist so the FFI cost buys nothing,
and its real value is host interoperability (write an SD card on a Mac, read it on the milestone-16a
board), which is a 16a story to do in Rust when first silicon makes it concrete.

**Sequencing.** After 29's rung one, so the framebuffer seam exists as a real consumer to point the
component at, and before committing to libghostty-vt. **Effort: 1 lane** (measured). The whole value is that it is
cheap and it fails early: if the toolchain, the shim, or the confinement story has a problem, we
find it with a throwaway component rather than half way into a port.

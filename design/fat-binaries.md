# Design proposal: microarchitecture-variant binaries

**Status:** open idea. Not decided. Revisit at milestone 6 (SMP) and milestone 7 (ELF
loader).

**Owner:** Chris

---

## The general argument (why this is interesting at all)

Fat binaries are usually discussed as an x86-vs-ARM thing, and in that framing they look
like a vendor-platform hack: Apple needs them because it drags a captive ecosystem across
an ISA transition every decade, and nobody else does.

That framing misses the stronger case: **microarchitecture variants within a single ISA.**

Almost every x86 binary in the world is compiled for a ~2003 baseline. Your CPU has AVX2
and maybe AVX-512 and the binary can't use any of it, because whoever compiled it didn't
know what machine you'd have. Same story on ARM with LSE, SVE, dot product, FP16.

Performance-critical software works around this by **shipping several copies of its hot
functions and choosing one at runtime.** glibc does it. OpenSSL does it. Every BLAS does
it. GCC and Clang have a whole feature for it (`target_clones`, ifunc resolvers).

That is a fat binary. It's just implemented ad hoc, by hand, per-library, thousands of
times over, with no shared tooling and no format support.

And the "just use a package manager" answer does not work here. Red Hat baselined RHEL 9
at `x86-64-v2` and there has been a running argument for years about moving to `v3`,
because the moment you do you drop still-working hardware. No distro is going to ship four
variants of every package in the archive. **This is a real, current, unsolved problem, and
it has nothing to do with consumer platforms.**

### The honest counterargument

**Apple retreated.** The App Store now does "app thinning": it strips the unused
architecture slices out of a universal binary *before download*. The poster child for fat
binaries added a distribution layer that picks the right slice, because carrying every
slice to every device was wasteful.

That is precisely what the Linux community said when it rejected
[FatELF](https://icculus.org/fatelf/) in 2009.

**So: fat binaries are the right answer exactly when you cannot interpose an intelligent
distribution layer.** When you can, the distribution layer wins, because it's lazy (you
transfer only the slice you need). Docker manifest lists are the most successful
implementation of this idea ever shipped, and they live at the registry, not in the file.

---

## Why it's live for cricker-os specifically

**Our two targets straddle the line.**

| Target | Core | ARM version | LSE atomics? |
|---|---|---|---|
| QEMU `-cpu cortex-a72` (current) | Cortex-A72 | **ARMv8.0-A** | **No** |
| Raspberry Pi 4 | Cortex-A72 | ARMv8.0-A | No |
| Raspberry Pi 5 | Cortex-A76 | ARMv8.2-A | Yes |
| Graviton / Neoverse N1 | N1 | ARMv8.2-A | Yes |
| Apple M-series | — | ARMv8.4+ | Yes |

**LSE** (Large System Extensions) adds single-instruction atomics: `CAS`, `LDADD`, `SWP`.
Without it, an atomic is a load-exclusive / store-exclusive retry loop (`LDXR` / `STXR`),
which is fine on one core and degrades badly under contention on many.

**Our spinlocks are the code that cares.** The moment we go SMP (see
[DECISIONS.md](../DECISIONS.md) §6) we hit a real performance cliff between our own two
deployment targets.

### And we can't use the normal escape hatch

Userspace already solves LSE dispatch. LLVM has **`outline-atomics`**: it emits a runtime
check against a global (`__aarch64_have_lse_atomics`) that **libc initializes at startup**,
and picks the right instruction sequence.

**We have no libc.** In a `no_std` kernel there is nothing to initialize that global. So we
either:

1. Compile the whole kernel for ARMv8.0 and eat slow atomics forever, or
2. Build the dispatch mechanism ourselves.

This is not a hypothetical we'd implement for elegance. **Milestone 6 forces the decision.**
The only real question is whether we solve it narrowly (one runtime check for LSE) or
generally (a feature-detection layer plus a format that carries variants).

---

## Design sketch

### 1. Feature detection (prerequisite, useful on its own)

aarch64 exposes CPU capabilities in **ID registers**, readable at EL1:

| Register | Tells us |
|---|---|
| `ID_AA64ISAR0_EL1` | LSE atomics, CRC32, AES, SHA, dot product |
| `ID_AA64ISAR1_EL1` | pointer auth, LRCPC, FCMA, JSCVT |
| `ID_AA64PFR0_EL1` | FP, AdvSIMD, SVE, which ELs exist |

The kernel reads these at boot and builds a feature vector.

**Note the asymmetry that makes this the kernel's job:** EL0 cannot freely read these
registers; access traps to EL1. So the kernel *must* mediate feature discovery for
userspace. Linux does this by stuffing a bitmask into the aux vector (`AT_HWCAP`) at exec
time, and every userspace dispatcher reads it from there.

**We get to design that interface.** `AT_HWCAP` is a flat bitmask that ran out of bits and
grew `AT_HWCAP2`. We can do better.

*This piece is worth building early regardless.* Reading `ID_AA64ISAR0_EL1` at boot and
printing the feature set is a good milestone-2 exercise and it's the prerequisite for
everything below.

### 2. Where does selection happen?

Three options, meaningfully different:

| Approach | Granularity | Cost | Precedent |
|---|---|---|---|
| **Load-time slice selection** (fat binary) | whole binary | none at runtime; binary is N× bigger | Mach-O / `lipo`, FatELF |
| **Load-time symbol resolution** (ifunc) | per function | one indirect call | glibc, ELF `STT_GNU_IFUNC` |
| **Runtime branch** (outline-atomics) | per call site | a predictable branch | LLVM `outline-atomics` |

For the **kernel itself**, the fat-binary approach is awkward (we are the thing doing the
loading; who selects our slice?). Realistically the kernel wants option 3, or link-time
specialization with a separate kernel image per feature level.

For **userspace binaries the kernel loads**, option 1 is genuinely attractive and is the
thing worth designing, because we control the loader and we control the format.

### 3. Container format

Follow Mach-O's actual design rather than FatELF's: **the fat part is a wrapper**, not a
change to ELF. A fat header lists N complete ELF files, each tagged with a required feature
vector, plus offsets.

Advantages: our ELF loader stays a plain ELF loader (good for milestone 7 — build the
boring thing first), and the fat handling is a thin layer in front of it that picks a slice
and hands one ordinary ELF downstream. Also means non-fat ELFs keep working with zero
special-casing.

```
+---------------------------+
| fat magic                 |
| n_slices                  |
+---------------------------+
| slice[0]: features, off, len  |  --> a complete, ordinary ELF
| slice[1]: features, off, len  |  --> a complete, ordinary ELF
+---------------------------+
| ...ELF #0 bytes...        |
| ...ELF #1 bytes...        |
+---------------------------+
```

Selection rule: pick the slice with the **most** required features that are all satisfied
by the CPU's feature vector. Ties broken by declaration order. If no slice matches, fail
the exec with a clear error rather than picking a baseline and crashing later.

---

## Open questions

1. **Is whole-binary granularity actually useful?** Carrying two full copies of a program
   to speed up three hot loops is a bad trade. ifunc-style per-symbol selection is strictly
   more precise. Does load-time slice selection earn its size cost, or is it just the
   *easiest* thing to implement?
2. **What's the feature vector's shape?** A flat bitmask (`AT_HWCAP`) ran out of bits and
   needed a sequel. Do we do a versioned level (`armv8.0`, `armv8.2`, like `x86-64-v3`), a
   set of named features, or both? Levels are simpler and match how compilers actually
   target things; features are more precise.
3. **Who builds the fat binaries?** This needs an `xtask` command (our `lipo`). What does
   the build/CI story look like when every userspace program has N slices?
4. **Does this compose with dynamic linking?** We have no dynamic linker and no plans for
   one, which honestly makes this *easier* than it is on Linux. Worth keeping it that way
   as long as possible.
5. **The kernel's own atomics.** Separate problem from userspace fat binaries, and it lands
   first (milestone 6). Do we solve it with a runtime branch, or by shipping separate kernel
   images per feature level? Probably a runtime branch to start.

---

## Recommendation on timing

**Don't build the fat loader yet.** Build the boring, plain ELF loader at milestone 7
first. You need a working loader before you have anything to make fat, and the plain path
is where the actual learning about program loading lives.

**Do build feature detection early.** Reading the `ID_AA64ISAR*_EL1` registers at boot,
building a feature vector, and printing it costs an afternoon at milestone 2, is a genuinely
good exercise in aarch64 system registers, and is the prerequisite for every option above.

**Milestone 6 forces the kernel-atomics question.** Answer that one narrowly when it lands
(a runtime branch), and let it inform whether the general mechanism is worth it.

Then decide, deliberately, at milestone 7, alongside the process-model decision point
([DECISIONS.md](../DECISIONS.md) §8).

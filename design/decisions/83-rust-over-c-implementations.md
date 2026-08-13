# 83. When the same thing exists in C and in Rust, take the Rust one

**Status: DECIDED.** Chris, 2026-08-13: *"we want to consider an approach of preferring rust
replacements of C libraries rather than relying on them. We are rust first in part because rust
solves for so many potential security vulnerabilities."* A corollary of §82, and an axis §46 does not
cover.

## The axis §46 left out

§46 decides **write versus take**, and its amendment decides **vendor versus depend**. Neither
decides which implementation to take when the same capability exists in both languages, which is the
question every compressor, every archive format and every database poses.

**The tree has already been answering it, unanimously and in writing nowhere.** §46 names crypto as
the case where you take rather than write, and what was actually taken is RustCrypto: `digest`,
`hmac`, `md-5` and `md4` in `crates/ntlm`, all pure Rust. Nobody reached for a C library, and nobody
recorded why not. That is the exact condition §46 itself was created to fix, in its own opening
words: *"The practice was already unanimous and written down nowhere, which is the state that
produces an inconsistent decision the first time someone does not share the instinct."*

## The rule

**When a dependency is available as a maintained Rust implementation and as a C one, take the Rust
implementation.** The C option needs a reason, recorded where the dependency is taken.

## Why, and it is sharpest exactly where this came up

Decompressors are close to the worst case for C. They parse attacker-controlled input with complex
state machines and hand-written buffer arithmetic, and the CVE history of zlib, libbzip2, liblzma and
the various zip implementations is dominated by heap overflows and out-of-bounds access. That is the
class Rust removes by construction rather than by care, and it is the class that matters most for a
component whose entire job is to consume hostile bytes.

**It is also cheaper here, which is measured rather than assumed.** Milestone 64 sorted the crates
that failed to build, and class C is "a C library or C sources": `zip` via `zstd-sys`, `ring` via C
and assembly, `diesel` failing at link on `-lsqlite3`. The note observes that class C is *"the only
class where 'make it build' and 'make it work' are the same task."* Preferring Rust does not solve
that class, it avoids it.

**And it is the corollary of §82.** You do not replace a C and ambient-authority ecosystem by
depending on it. A system whose thesis is that the old ecosystem should be rewritten cannot reach for
that ecosystem by default and remain coherent.

## Four qualifications, without which this breaks on first contact

**§46's exposure test survives as the tiebreaker.** Rule 4 prefers depending when correctness is won
by *exposure* rather than by reading a specification, and that is still true. Maturity varies sharply
across candidates: `miniz_oxide` is mature and already in milestone 64's built-with-no-change set,
while `ruzstd`, `lzma-rs` and `sevenz-rust` are younger and differ in whether they encode or only
decode. **Prefer means prefer.** An immature Rust implementation of a hostile-input parser is not
automatically safer than a battle-tested C one, and the assessment belongs next to the dependency
rather than in this section.

**Rust does not replace confinement, and confinement does not replace Rust.** §31's seam bounds what
a compromised component can reach; memory safety prevents the compromise. A pure-Rust decompressor
with a logic bug still emits wrong bytes, and wrong bytes out of a decompressor are the input to
everything downstream. Nobody may cite this section to argue the C seam is redundant.

**Performance is a cost to measure, not to wave away.** Hand-tuned C codecs, `zstd` above all, can
beat their Rust counterparts by margins that matter, and milestone 55's backup target is a workload
where compression throughput *is* the product. This tree measures rather than argues, so a Rust
implementation chosen under this rule and found materially slower is a finding to record, not an
embarrassment to hide.

**It is not absolute, and SQLite is the proof.** Milestone 66 names SQLite as a C library requiring
the §31 seam, and no drop-in pure-Rust SQLite exists. Where there is no credible Rust option, the C
seam is the answer and this section does not apply. That is a reason the seam stays a first-class
part of the system rather than a transitional hack.

## What it changes

Class C shrinks to the cases with no Rust option. The compressors Chris asked about (`gzip`,
`bzip2`, `xz`, `zstd`, `zip`, `7z`) stop being a C-seam problem and become ordinary dependency
choices, most of which milestone 64 already measured as building. What remains genuinely C-only is
essentially SQLite, plus the open question of whether `ring` should be replaced by RustCrypto rather
than ported.

## BUGS

- **"Maintained" and "credible" are not defined here**, and they are doing real work in the rule. The
  assessment is per dependency and this section deliberately does not attempt a threshold, which
  means two people can reach different answers about the same crate.
- **Nothing enforces it.** §81's foreclosure check is a lint; this is prose, which is rung three of
  CLAUDE.md's ladder. A `-sys` crate can enter the tree without anything asking whether a Rust
  implementation existed.
- **It says nothing about vendored C already present.** RedoxFS is vendored under §34 and is not
  affected; whether a future Rust filesystem should replace it is a separate decision nobody has
  raised.

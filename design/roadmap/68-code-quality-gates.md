# 68. Code-quality gates: one lint policy, and the lints that lost

**Status: BUILT.** The lint policy landed 2026-08-02. The doc-example half **closed 2026-08-17**;
the `missing_docs` half's open policy question is **decided 2026-08-22 (DECISIONS §107): opt-out,
workspace-wide.** The worklist itself (401 items across 32 crates on 2026-08-17, closed to 235 across
7 crates by a 2026-08-22 follow-up lane) is not required to reach zero for BUILT; it is an ordinary
ratchet now, the same shape §38 already uses for dead code, tracked in notes/doc-coverage.md.

`missing_docs = "warn"` is in `[workspace.lints.rust]`; every crate with `[lints]
workspace = true` (all 57) is checked by default, and the 7 crates with a real remaining worklist
(`isa`, `smb_proto`, `mdns_proto`, `pci`, `gpt`, `grant_plan`, `compositor`) carry an explicit
`#![allow(missing_docs)]` citing §107 and their own count. A crate added tomorrow is gated from the
day it is written, closing the gap this block's BUGS section used to name.

What is now checked rather than counted, which is the change that matters most here: **the examples
are enforced by `cargo test --doc`** and the item docs by `script/lint`'s `-D warnings` in the crates
that opted in. Both remaining halves used to be numbers in this file, the shape §76 warns about, and
both numbers had moved by the time anyone acted on them.

## What landed

The tree had no `rustfmt.toml`, so import order was whatever each author typed, and lint selection
lived in 19 of 39 crates repeating one `[lints.rust]` table while the other 20 said nothing. Both are
now single decisions: `group_imports`/`imports_granularity` in `rustfmt.toml`, and
`[workspace.lints]` with a one-line opt-in per member.

Adopted: `cast_ptr_alignment`, `ptr_as_ptr`, `semicolon_if_nothing_returned`, `manual_let_else`,
`doc_markdown`. 1,221 warnings went to zero. Three new non-clippy gates joined `script/lint`:
**dependency direction** (nothing under `crates/` may depend on a binary, which would take it out of
the host tests and Kani while still building), **unused dependencies** (§46 with a gate), and
**spelling** over the prose.

## The part worth carrying off: three lints were removed on the evidence

Each was enabled, measured against the real tree, and dropped, with the number recorded next to it
in `Cargo.toml` and `rustfmt.toml` rather than silently omitted.

- **`cast_possible_truncation`**: 199 of 497 hits are `u64`/`i64` to `usize`, warned about for
  32-bit-pointer targets. §19 names aarch64, riscv64 and x86_64, all 64-bit. Over half its output is
  about a platform that does not exist here, and clippy cannot be told otherwise.
- **`items_after_statements`**: all 43 hits are a `const` sitting beside its use, under the comment
  that explains it. Obeying it separates every one from its explanation.
- **`format_code_in_doc_comments`** (rustfmt): destroyed an authored alignment column inside
  `crates/gpt`'s module example, and emitted trailing whitespace into a doc comment.

`doc_markdown` is the same story with the opposite ending: 416 hits, about half wanting backticks
around `RedoxFS`, `PCIe` and `OpenSBI`, which are proper nouns that would then render as code a
reader could type. `clippy.toml`'s `doc-valid-idents` takes those; the other half were real.

The general rule, and the reason this milestone is worth a roadmap entry at all: **a lint that is
right in general can be wrong for a tree, and the way to find out is to run it and read the hits.**
Reasoning about a lint's description predicts none of these.

## The doc-example half: closed, and the counts had moved

**Every crate under `crates/` now has a worked example**, and the workspace runs 116 doctests
where this block recorded 23, 109 of which `script/test`'s selection actually runs. The detail, the per-crate treatment, and the honest gaps are in
notes/doc-coverage.md; three things belong here because they are about the block rather than about the
work.

**The count was 31 when someone finally measured it, not 28**, and it had moved in both directions,
which is the interesting part. Three of the four crates this block named as the hard ones left
(`dtb`, `nifefs`, `gpt`) got their examples in the fortnight after it was written, and `isa`,
`manual`, `swish` and `slots` gained theirs too. Meanwhile five crates arrived carrying none: `ntlm`
and `system_initializer`, then `nvme`, `mdns_proto` and `smb_proto`. **A count of what is missing is a
moving target in a tree that adds a crate every few days**, and writing it into a block converts it
into a claim that rots. The gate is `cargo test --doc`, and what it measures cannot go stale.

**Five crates' doctests are run by nothing in CI**, which is the one gap the pass could not close.
`user_rt`, `swap_proto`, `virtio`, `supervision_proto` and `system_initializer` take unconditional
`user_rt` dependencies, so the host test selection excludes them. Splitting each crate's pure half
from its syscall half is what would fix it, and that is a lane of its own.

**An example that runs was preferred everywhere it was possible**, because a fenced block that never
compiles is a comment. `elf` forges a writable-and-executable segment and watches it be refused;
`paging` builds real aarch64 page tables on the host and shows that break-before-make is *forced*;
`smb_proto` performs a whole SMB2 mount against the fixture share; `ntp_proto` shows an off-path spoof
failing the origin check before any of the packet is believed. Three crates got `no_run` with the
reason in the prose, because a `svc` on a host is a fault and a function returning `!` has nothing to
assert.

## The `missing_docs` half: a ratchet, a worklist, and a decided policy

**401 undocumented public items across 32 of the 55 crates** (2026-08-17). Adopting the lint
tree-wide is a commitment to write those 401 first, so it was not adopted tree-wide at first. What was
adopted then is `#![warn(missing_docs)]` in **the 23 crates that were already clean**, which under
`script/lint`'s `-D warnings` means they cannot regress.

**Two measurement traps caught the first lane and are worth the paragraph**, because both make a
number look authoritative while being wrong.

`rustdoc --show-coverage` and `missing_docs` **do not measure the same thing**, and this block used
the first to justify deferring the second. Six crates report 100% documented and still have hits, in
one case 41 of them: `--show-coverage` does not count struct fields, type aliases or `macro_rules!`,
and the lint does. So the coverage range quoted above was never evidence about `missing_docs` at all.

And **cargo replays cached diagnostics**, so a per-package loop attributes other crates' warnings to
the selected crate. That produced a first answer of 647 items across 38 crates, which is wrong and
looked entirely plausible. Measure it as one workspace-wide invocation into a clean target directory.

**A follow-up lane burned the worklist down on 2026-08-22**, re-measuring the trap-avoiding way before
and after every batch: the honest count had drifted to 404 across 31 crates in the five days since,
and the lane closed 169 of those, crate by crate, writing real doc comments (not filler) for every
item and turning on `#![warn(missing_docs)]` on each crate it brought to zero. **235 items remain,
across 7 crates**: `isa` (54), `smb_proto` (52), `mdns_proto` (41), `pci` (24), `gpt` (23),
`grant_plan` (22), `compositor` (19). 50 of the 57 crates under `crates/` now carry the opt-in, up
from 23. See notes/doc-coverage.md for the full crate list and the per-crate worklist table.

**Decided 2026-08-22 (DECISIONS §107): `[workspace.lints.rust]`, opt-out.** Higher on CLAUDE.md's
ladder, since the default becomes on and the opt-out list is a worklist that can only shrink, and it
is the only version that covers a crate created tomorrow. It does invert a rule §61 itself states
("adding a lint to this table is a decision to fix every existing violation first"); §107 records
this as a deliberate, considered exception rather than a quiet reversal, made because the worklist a
switch needed `#[allow]` for had shrunk from 32 crates to 7 by the time the question was answered.

## What closing the unsafe half taught

All 205 blocks are commented and `undocumented_unsafe_blocks` is in `[workspace.lints]`, so the
convention is now enforced rather than followed. The useful finding is what the sites turned out to
be, because it is not what the raw count suggested.

**Three quarters of them were genuinely uniform**, and the uniformity was a fact about the system
rather than an excuse:

- **58 panic-handler traps**, byte-identical `asm!("brk #0", options(nostack, nomem))` or its
  `ebreak` twin, in EL0 programs.
- **73 `invoke` syscalls.** `user_rt::invoke` is the only unsafe function in the EL0 runtime, and its
  contract is that there is no caller obligation: *"the kernel validates the capability and the
  method before acting; the caller is trusting the kernel, not the other way around."* An
  `unsafe { invoke(..) }` is unsafe because it is inline asm, not because a bad slot could break
  anything. **That is the capability model showing through the type system**, and it is why one
  sentence is honest at all 73 sites.

**The remaining quarter was the real work**, and each site's comment says something a reader could
not have guessed: intrusive-queue link ownership (including a drop-order fact, that a test's nodes
are declared before the queue so they outlive it); allocator alignment invariants; virtio ring
aliasing, where the read side is the driver's memory and the write side is a kernel-private shadow;
`env::set_var` in `xtask`, unsafe since edition 2024, sound only because the one thread it ever
spawns copies pipe bytes and never reads the environment.

**The test that decides whether a batch may share a comment** is whether the sentence is checkable
at each site. For a trap in a panic handler it is, because it is literally the same site 58 times.
For a test module's pointers it is not, which is what the reverted generic pass got wrong.

One regression is worth recording because nothing else would have caught it: adding an
`#[allow(clippy::cast_ptr_alignment)]` above an `unsafe` block silently **separated an existing
`SAFETY:` comment from its block**, and clippy then reported the block as undocumented. An attribute
between a comment and the item it describes breaks the association. The fix is ordering: attribute
first, then the comment, then the block.

# 68. Code-quality gates: one lint policy, and the lints that lost

**Status: PARTIAL.** Started and largely landed 2026-08-02, from an audit of what the tree checked
and what it did not. Two halves are deliberately unfinished and scoped below rather than rushed.

**Gate: NONE.** Both remaining halves are plain work with counts attached: 28 host crates with no
doc example at all, and item coverage running from 36.4% to 100% before `missing_docs` is
adoptable. The block asks for them in one pass.

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

## What is NOT done, with counts

Both remaining halves are real engineering, not mechanical, and a first attempt at automating one of
them was reverted for producing exactly the wrong artefact.

- **Doc examples.** 5 doctests in the whole host workspace became 23, and nine crates went from
  0.0% example coverage to somewhere between 2.4% and 25%. That is a real start and explicitly not
  the FreeBSD standard CLAUDE.md sets: **28 host crates still have no example at all.** The crates
  done first were the ones where an example carries an argument rather than a signature (`capability`
  showing that intersection is the only transfer operation, `measured_boot` showing that an
  unmeasured name fails CLOSED, `regions` showing the two refusals that are not about the budget).
  The ones left are mostly parsers that need a real fixture to demonstrate (`elf`, `dtb`,
  `nifefs`, `gpt`), which is more work per example, not less valuable.
- **`missing_docs`** is still not adoptable, and the number says why: item coverage runs from
  **36.4%** (`socket_proto`) to 100%, with `pci` at 48.9% and `intrusive` at 50%. Adding it to
  `[workspace.lints]` is a commitment to write several hundred item docs first, which is §61's rule
  and not a formality.
- **Doc examples: 5 doctests in the entire host workspace**, and `rustdoc --show-coverage` reports
  0.0% examples on every crate sampled (`ipc` 94.4% of items documented, 0.0% examples; `capability`
  67.6%/0.0%). CLAUDE.md sets the FreeBSD standard explicitly ("a page without a worked example has
  not finished explaining itself"), so this is a stated commitment the tree does not meet. A doctest
  is documentation and a test at once, and `cargo test` already runs them, so the harness needs no
  work; only the examples are missing.

`missing_docs` belongs with the second of those (item coverage is 67–94% and no crate warns on it),
and is best done in the same pass as the examples rather than separately.

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

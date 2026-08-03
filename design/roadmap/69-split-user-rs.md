# 69. Split `kernel/src/user.rs` by service

**Status: BUILT (2026-08-02), both ISAs.** Raised 2026-08-02, from a question about whether
thousand-line files are an antipattern in Rust. The general answer is no, and this file is the
exception that proves why.

All 46 top-level modules moved to `kernel/src/user/<name>.rs`; `user.rs` went from **15,499 lines to
1,993**, and the largest file left in the tree from that split is `user/tests.rs` at 2,306. Nothing
gained visibility: not one `pub`, `pub(crate)` or `use` was added or widened, which the section below
predicted and which was then checked mechanically rather than by eye. Re-inlining every new file back
into `user.rs` and running `rustfmt` over the result reproduces the pre-split file **byte for byte**,
so the only content change in the whole milestone is `rustfmt` reflowing about 90 lines that gained
four columns of width when they lost a level of indentation.

The declaration each module left behind keeps its own doc comment and its own `#[cfg]`/`#[cfg_attr]`
attributes, so `user.rs` reads as an annotated index of the services and `rustdoc` is unchanged.

## Why file length is usually the wrong metric here

In Java the one-public-class-per-file rule is compiler-enforced; in Rails, autoloading maps paths to
constants. Both make size a proxy for "too many responsibilities". Rust has neither: a file **is** a
module, the module is the privacy boundary, and the standard library ships multi-thousand-line files
that are one coherent thing. This tree also comments far more heavily than production code by policy
and keeps unit tests in-file, which inflates counts for good reasons. `crates/glob` is 1,173 lines of
which **54% are tests**; `crates/calendar` is 1,578 at 43%. Shrinking either would make it worse.

## What makes `user.rs` different

It is **15,499 lines holding 46 top-level modules**: roughly a dozen `pub mod *_service` blocks
(`console`, `virtio`, `fs`, `display`, `compositor`, `keyboard`, `clock`, `entropy`, `credential`,
`ntp`, `untyped`, `pipeline`) and roughly 34 `#[cfg(test)]` modules, interleaved. `fs_service` alone
is 1,217 lines and the top-level `tests` module is 2,320.

The test that matters is not the line count but this: **to change the NTP service you open the file
where the compositor lives.** That is ten responsibilities in one place, and no amount of Rust
module semantics makes it one.

## Why this split is unusually cheap

The standard argument against splitting a Rust file is that it forces you to widen visibility: items
private to the module become `pub(crate)` merely to be reachable, and a long file is traded for a
leakier API.

**That argument does not apply, because the boundaries already exist as `mod` blocks.** A child
module can see its ancestors' private items whether it is written inline or in its own file, so

```rust
pub mod fs_service;          // was: pub mod fs_service { ... }
```

is semantically identical to what is there now: no visibility change, no API change, and `use
super::*` inside keeps working. This is a file move, not a refactor, which is what separates it from
the speculative restructuring CLAUDE.md warns against.

## The one real cost, and the scheduling consequence

`user.rs` is the kernel's service-wiring file and nearly every milestone touches it, so the diff
conflicts with anything in flight. **Do it while the tree is quiet**, in one pass, and do not
interleave it with feature work. Splitting it across two lanes would be worse than not splitting it.

Suggested shape: `kernel/src/user/` with one file per service and each service's tests beside it,
leaving `user.rs` as the wiring that names them.

# Where an unsafe obligation is written, and where it is only implied

Milestone 82. The tree enforces two lints over `unsafe`, and they are meant to compose:

- `clippy::undocumented_unsafe_blocks` (milestone 68) fires on an `unsafe {}` block with no
  `// SAFETY:` comment above it.
- `unsafe_op_in_unsafe_fn` fires on an unsafe operation inside an `unsafe fn` that is not wrapped in
  an explicit `unsafe {}` block.

Neither is interesting alone. An `unsafe fn` body is one implicit unsafe block, so a function with
three unsafe operations carries three separate invariants under a single signature, and the clippy
lint sees none of them because there is nothing for it to fire on. The second lint removes the
implicitness; the first then charges each resulting block for its comment. What you get is the
property this kernel wants: **every unsafe operation sits next to the written invariant that makes
it sound**, whether or not the enclosing function is unsafe.

Both are in `[workspace.lints]` in the root `Cargo.toml`, which is where lint policy lives and where
the reasoning for each is recorded.

## The survey, and the thing it found instead

The milestone was raised expecting a burn-down: 33 `unsafe fn`s, some number of bare operations
inside them, fix each with an honest SAFETY comment, then turn the lint on.

**The count of violations was zero, before anything was changed.** Measured by adding the lint and
running `cargo check` over each of the thirteen configurations `script/lint` builds (the host pass,
the three side workspaces, the bare-metal pass, and each of the four kernel boot-mode features on
both ISAs), with every `.rs` file touched first so nothing was served from cache. Plus a fourteenth
that `script/lint` does not build: `-p user -p user_rt` for riscv64. The gate compiles those two
packages for aarch64 only, which is worth knowing on its own.

The reason is the edition. Every one of the 49 packages we own is edition 2024, and
`unsafe_op_in_unsafe_fn` is **warn-by-default in that edition**, as part of
`rust_2024_compatibility`. `script/lint` runs `-D warnings`. So the rule has been a hard gate here
since the edition bump, enforced by nothing anybody wrote down.

That is easy to check rather than take on faith. Delete one `unsafe {}` wrapper inside an `unsafe
fn`, with the workspace lint line removed, and rustc says:

```
warning[E0133]: dereference of raw pointer is unsafe and requires unsafe block
   --> crates/intrusive/src/lib.rs:116:9
note: an unsafe function restricts its caller, but its body is safe by default
    = note: `#[warn(unsafe_op_in_unsafe_fn)]` (part of `#[warn(rust_2024_compatibility)]`) on by default
```

The line landed anyway, for two reasons that survive the redundancy. A reader of the lint policy can
see the rule, which was milestone 68's entire argument for putting policy in one place. And a
package at an older edition cannot escape it; the tree already contains one, `vendor/redoxfs` at
edition 2021, and any external crate pulled into the workspace arrives at whatever edition its
author picked.

## The shape of the 33

All 33 `unsafe fn`s are in `kernel/` and `crates/`. **`user/src/` has none**, which corrects the
milestone spec's "across `kernel/`, `crates/`, and `user/`".

Twenty-two have at least one explicit `unsafe {}` in the body. Every one of those blocks has a
SAFETY comment, and clippy reproves it on each run, with the single exception of `ipc`'s `seed`,
which is `#[cfg(kani)]` and therefore never compiled by the gate (see BUGS below). The other
**eleven have no unsafe block at all**, and since the lint is clean, that means their bodies
contain **no unsafe operation**:

| Site | Why it is `unsafe fn` anyway |
|---|---|
| `crates/clock_proto/src/lib.rs:178` `Clock::new` | takes a VA the caller promises is a mapped clock page |
| `crates/paging/src/lib.rs:323` `assume_no_stale_entry` | the name is the contract: the caller asserts a TLB fact |
| `crates/paging/src/lib.rs:410` `Mapper::new` | the caller promises `root` is a live table |
| `crates/user_rt/src/heap.rs:193` `GlobalAlloc::alloc` | unsafe because the trait method is |
| `kernel/src/arch/aarch64/mmu.rs:599` `set_ttbr0` | `aarch64-cpu` exposes `TTBR0_EL1.set` as **safe** |
| `kernel/src/arch/riscv64/mmu.rs:513` `activate_user` | forwards to `write_satp`, which is a safe fn |
| `kernel/src/drivers/gic.rs:149` `init` | takes two MMIO virtual addresses on trust |
| `kernel/src/drivers/ns16550.rs:55` `Ns16550::new` | takes an MMIO base on trust |
| `kernel/src/drivers/pl011.rs:88` `Pl011::new` | takes an MMIO base on trust |
| `kernel/src/drivers/plic.rs:83` `init` | takes an MMIO base and a hart context on trust |
| `kernel/src/sync.rs:263` `force_reset_ranks` | breaks lock-order bookkeeping, which is not a memory operation |

For these eleven the lint composition buys nothing, and that is not a defect in them. Their
unsafety is a **contract about meaning**, not a memory operation the compiler can point at: writing
`TTBR0_EL1` is the most consequential thing in the kernel and `aarch64-cpu` hands it over as a safe
call. The invariant lives in the `# Safety` section of the rustdoc and nowhere else, so **for a
third of the tree's `unsafe fn`s the doc comment is the only enforcement there is**. Read them
accordingly when you change one.

## BUGS: three things neither lint can reach

**1. A safe fn whose SAFETY comment discharges onto "the caller".** The comment names an obligation
the signature imposes on nobody, so any safe code may call the function without it and both lints
are satisfied. Four sites in `kernel/`:

| Site | The comment's claim |
|---|---|
| `kernel/src/virtio.rs:233` `pread` | "the caller passes addresses inside a device-mapped BAR or mmio window" |
| `kernel/src/stack.rs:121` `paint` | "the caller hands us a mapped, unused stack region" (`#[cfg(test)]`, so test builds only) |
| `kernel/src/arch/aarch64/mmu.rs:843` `switch_user_root` | "the caller passes either a live `AddressSpace`'s composed value or ..." |
| `kernel/src/arch/riscv64/mmu.rs:48` `write_satp` | "the caller guarantees `satp` names a well-formed Sv39 root" |

The last is also an ISA asymmetry: aarch64's equivalent, `set_ttbr0`, **is** an `unsafe fn`, so the
same register write is a contract on one architecture and an ordinary call on the other. Not fixed
in milestone 82, deliberately: turning these four into `unsafe fn`s puts an unsafe block (and a real
SAFETY comment) at every call site including the context switch, which is a change to the kernel's
soundness surface and deserves its own review rather than a ride on a lint milestone.

Not every "caller" in a SAFETY comment is this. `sched.rs`'s `ipc_call` and `user_rt`'s `cap_delete`
mean the calling *thread* and the calling *process*; `interrupts::enable` says outright that the
operation is sound and only the timing is the caller's problem. The pattern to look for is a safe
fn that would be unsound if the sentence were false.

**2. `#[cfg(kani)]` code is invisible to both lints.** `cfg(kani)` is set by the model checker and
by nothing else, so `script/lint` never compiles those modules and neither lint can fire in them.
The tree has 14 `unsafe {}` blocks under `#[cfg(kani)]`, in `crates/intrusive` and `crates/ipc`.
`intrusive`'s two both carry SAFETY comments. **Eleven of `ipc`'s twelve do not**, and the gate has
never said so. A real fix is a gate rather than a pass of comments (a clippy invocation with
`--cfg kani`, or `-D warnings` on the `script/verify` build); adding the comments alone leaves
nothing to stop the next harness from skipping them.

**3. Neither lint reads the comment.** `undocumented_unsafe_blocks` checks that a comment exists,
not that it is true, which is why DECISIONS §61 carries a BUGS note about a generated pass that
produced a comment false at its first site. Three comments in the tree are verbatim copies of each
other ("this function's own `# Safety` contract is exactly the one this call needs; it forwards, it
does not weaken", in `console.rs`, `sync.rs`, and `aarch64/mmu.rs`). All three are true: each is a
pure forwarding call whose callee's contract is implied by the caller's. Verbatim repetition is a
signal worth checking, not a verdict.

## Re-running the survey

```sh
# the whole gate, with the lint already in [workspace.lints.rust]
script/lint

# just the count, over every configuration, cache defeated
find crates kernel user xtask -name '*.rs' -exec touch {} +
cargo check --workspace --exclude kernel --exclude user --exclude user_rt --all-targets 2>&1 | grep E0133
cargo check -p kernel -p user -p user_rt --target aarch64-unknown-none-softfloat --all-targets 2>&1 | grep E0133
cargo check -p kernel -p user -p user_rt --target riscv64imac-unknown-none-elf --all-targets 2>&1 | grep E0133
```

Grep for `E0133`, not for the lint's name: rustc reports the error code and spells the lint
`unsafe-op-in-unsafe-fn` with hyphens in its trailing note, so a grep for the underscored form
finds nothing and looks exactly like a clean tree.

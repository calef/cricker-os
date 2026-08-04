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
both ISAs), with every `.rs` file touched first so nothing was served from cache. Plus one more that
`script/lint` did not build: `-p user -p user_rt` for riscv64. The gate compiles those two packages
for aarch64 only, which is worth knowing on its own, and is still true.

(Milestone 113 added a configuration, so `script/lint` now builds fourteen: the thirteen above plus
the clippy pass with `--cfg kani`. The riscv64 `user` gap is unrelated and still open.)

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

**2. `#[cfg(kani)]` code is invisible to both lints. FIXED in milestone 113**, and the section below
records what the gate found. `cfg(kani)` is set by the model checker and by nothing else, so
`script/lint` never compiled those modules and neither lint could fire in them. The tree has 14
`unsafe {}` blocks under `#[cfg(kani)]`, in `crates/intrusive` and `crates/ipc`. `intrusive`'s two
both carry SAFETY comments. **Eleven of `ipc`'s twelve do not**, and the gate had never said so. A
real fix is a gate rather than a pass of comments (a clippy invocation with `--cfg kani`, or
`-D warnings` on the `script/verify` build); adding the comments alone leaves nothing to stop the
next harness from skipping them.

**3. Neither lint reads the comment.** `undocumented_unsafe_blocks` checks that a comment exists,
not that it is true, which is why DECISIONS §61 carries a BUGS note about a generated pass that
produced a comment false at its first site. Three comments in the tree are verbatim copies of each
other ("this function's own `# Safety` contract is exactly the one this call needs; it forwards, it
does not weaken", in `console.rs`, `sync.rs`, and `aarch64/mmu.rs`). All three are true: each is a
pure forwarding call whose callee's contract is implied by the caller's. Verbatim repetition is a
signal worth checking, not a verdict.

## The gate over `cfg(kani)`, and the measurement that chose it (milestone 113)

Two candidates, and the brief said to measure before arguing. The measurement is one-sided enough
that there was nothing left to argue about.

| | clippy with `--cfg kani` | `-D warnings` on `script/verify` |
|---|---|---|
| Undocumented `unsafe` it finds | **13** | **0** |
| Other warnings it finds | **13** | 0 |
| Needs Kani installed | no | yes |
| Runs | every pull request, ~1 s | when someone runs the proofs, ~20 min |
| Compiles the harnesses truthfully | no, against a shim | yes, by definition |

**Why the second column is zero, which is the whole decision.** `cargo kani` drives a *rustc*, not a
clippy-driver. `undocumented_unsafe_blocks` is a `clippy::` lint and simply does not exist in that
compiler, so no amount of `-D warnings` can make it fire. This was measured rather than reasoned
about: `RUSTFLAGS="-D warnings" cargo kani -p ipc --only-codegen` compiles clean while thirteen
undocumented unsafe sites sit in the file. The same command *does* fail on a deliberately added
unused variable, so `RUSTFLAGS` reaches Kani and the gate would be real for **rustc** lints
(`unsafe_op_in_unsafe_fn` among them). It is only the clippy half, which is the half this milestone
is about, that it cannot reach.

So `script/lint` grew a fourteenth clippy configuration. The tree's `#[cfg(kani)]` modules are all in
`crates/`, so it is the host pass's package selection with three flags added:

```sh
cargo clippy --workspace --exclude kernel --exclude user --exclude user_rt --all-targets -- \
    --cfg kani --extern kani=target/kani-lint-shim/libkani.rlib -L target/kani-lint-shim -D warnings
```

### The shim, and what it does not promise

`--cfg kani` alone does not compile: the harnesses are written against Kani's intrinsics, and
without the crate that provides them rustc stops at `use of unresolved module or unlinked crate
kani`. `scripts/kani-lint-shim/` is that crate, built by `script/lint` with two plain `rustc`
invocations before the pass runs. The surface it has to cover is small, which is what makes this
cheap: across 21 crates and 108 harnesses the tree uses exactly **five** Kani items, `any` (258
uses), `proof` (108), `assume` (65), `unwind` (29) and `cover!` (19), and no `Arbitrary` derive, no
contracts, no `any_where`.

**It is two crates because an attribute macro can only come from a proc-macro crate.** The
one-crate route was tried and does not work: registering `kani` as a tool namespace with
`-Zcrate-attr=register_tool(kani)` loses to the extern crate the same code needs for `kani::any`, and
rustc reports `cannot find proof in kani`.

**It is deliberately looser than Kani in one place.** The real `any` requires `T: Arbitrary`; the
shim's takes any `T`. A lint gate must never reject code the model checker accepts, and the error
that remains possible (code only the *shim* accepts) fails under `cargo kani`, loudly, where anybody
would look.

**A clean pass here is not a proof**, and the shim is not a second implementation of Kani. It has no
semantics at all: `any` returns nothing, `assume` constrains nothing. `script/verify` remains the
thing that proves.

**When a harness reaches for Kani API the shim lacks, the lint pass breaks and the proof does not.**
The failure is a compile error naming the missing item, and the fix is to add the item, not to drop
the pass.

### What it found, and the correction to the count above

**26 warnings in 9 crates**, none of which any gate had ever printed.

Thirteen are the unsafe half, and the number in BUGS item 2 was **11, which was an undercount**. The
survey enumerated `unsafe {}` blocks; `undocumented_unsafe_blocks` also fires on an `unsafe impl`,
and there are two of those under `#[cfg(kani)]`, one in each crate, both undocumented. Counting by
hand found the population the lint's own rule would have found for free, which is the argument for
gates in one line.

| Crate | Sites | Shape |
|---|---|---|
| `ipc` | 11 blocks + 1 `unsafe impl` | the harness's `seed`, and every call into `send`/`recv` |
| `intrusive` | 1 `unsafe impl` | `Node for N` in the proof module (its two blocks were already commented) |

The other thirteen are ordinary clippy, in crates nobody suspected: `doc_markdown` (4),
`manual_range_contains` (4), `manual_let_else` (2), `len_zero` (2), `needless_range_loop` (1),
`assertions_on_constants` (1), across `asid`, `calendar`, `cred_proto`, `crickerfs`, `dma_validator`,
`paging`, `pci` and `slots`. That half is the answer to "does this find anything besides unsafe",
and it is yes: **half of what the pass finds has nothing to do with unsafe at all.** One of them,
`dma_validator`'s `assert!(RING_END <= RING_BLOCK)` over two constants, became a `const {}` assertion
and so moved from a proof-time check to a compile-time one.

All 26 are fixed. Every proof in the eight crates whose harness code changed was re-run and still
passes.

### Writing the eleven comments, which was the point of doing the gate first

`DECISIONS.md` §61 records why a generated pass is the wrong instrument here: the lint checks that a
comment exists, never that it is true, so a false comment passes the gate and misleads a reader who
now believes somebody checked. The eleven are worth reading as an example of the alternative.

Every `unsafe` call in `ipc`'s proof module discharges the same two obligations, and they are stated
once in the module's own doc rather than eleven times: **the nodes outlive the endpoint** (declared
in one `let` before `e`, and locals drop in reverse declaration order) and **no node is on a queue
when it is passed** (each `N::new()` starts with a null link, and no harness hands the same node to
two calls). The `#[cfg(test)]` module beside it had already chosen exactly this shape, which is why
its twenty-odd sites read as one argument and not twenty.

Each site's own comment then adds only what is particular to it, and the particulars are where the
real content is. `a_collected_sender_is_forgotten` carries a fourth node, `me2`, purely so its second
receive does not reuse `me`: `me` is provably not queued at that point, but a separate node makes the
site's obligation independent of that reasoning, and the comment says so rather than asserting the
conclusion. `send_rendezvous_iff_a_receiver_waited` takes `&mut r` once into a `receiver_ptr` it
keeps, so its comment records that no second pointer to `r` exists. `seed`'s two match arms are
exclusive, which is what makes "pushed at most once" true.

One warning fired only because the module doc grew: `mixed_attributes_style`, when the paragraph was
first written as `//!` inside a module that already had a `///` block above it. It belongs in the
outer doc.

## Re-running the survey

```sh
# the whole gate, with the lint already in [workspace.lints.rust], and (since milestone 113) the
# fourteenth clippy configuration that compiles the proof harnesses
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

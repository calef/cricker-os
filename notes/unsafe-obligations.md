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

**1. A safe fn whose SAFETY comment discharges onto "the caller". DECIDED in milestone 112**, and
the section below records what each site got and why. The comment names an obligation the signature
imposes on nobody, so any safe code may call the function without it and both lints are satisfied.
Four sites in `kernel/`:

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
cheap: across 23 crates <!--count:harness-crates--> and 129 harnesses <!--count:kani-harnesses-->
the tree uses exactly **five** Kani items, `any` (287 uses), `proof` (129) <!--count:kani-harnesses-->,
`assume` (71), `unwind` (33) and `cover!` (21), and no `Arbitrary` derive, no contracts, no
`any_where`.

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
`assertions_on_constants` (1), across `asid`, `calendar`, `cred_proto`, `nifefs`, `dma_validator`,
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

## The comments that bound nobody, decided (milestone 112)

BUGS item 1 above is this milestone. Four safe functions carried a `// SAFETY:` comment that
discharged an obligation onto "the caller" while their signatures imposed it on nobody, and both
lints were satisfied throughout, because there is no `unsafe fn` and no undocumented block for
either to fire on.

**Three converted, one did not, and the difference is not how strong the obligation is.** It is
whether anything closes the set of callers.

| Site | Decided | Why |
|---|---|---|
| `arch/riscv64/mmu.rs` `write_satp` | `unsafe fn` | aarch64's `set_ttbr0` already was, so the same register write was a contract on one ISA and an ordinary call on the other |
| `arch/{aarch64,riscv64}/mmu.rs` `switch_user_root` | `unsafe fn` | `pub`, called cross-module from `sched.rs`, and no type can carry the obligation (below) |
| `stack.rs` `paint`, and its sibling `high_water` | `unsafe fn` | `pub` in the crate, so any kernel code could have written the pattern over an arbitrary range |
| `virtio.rs` `pread` / `pwrite` | stayed safe fns | private to one module, so the compiler closes the caller set at twenty sites in one `impl` block |

### What makes an obligation binding, which is the whole distinction

`sched.rs`'s `endpoint_of` is the contrast that settles it. Its comment says the access is
"serialized by SCHED, which every caller holds", which reads exactly like the four. **It binds**,
because the parameter is `&Scheduler` and the only way to obtain one is through the lock guard. The
sentence restates a fact the type already enforces.

`switch_user_root(ttbr: u64)` says something that sounds similar and enforces nothing, because any
`u64` will do. So the question to ask of a SAFETY comment on a safe fn is not "does it mention the
caller" but **"could the parameter have been produced without meeting this?"** When the answer is no,
the comment is documentation of a type-level guarantee. When it is yes, the comment is the only thing
there, and `unsafe fn` is what puts it in front of somebody.

`virtio::pread` is the third case, and it is why the rule is not "convert everything a type does not
guarantee". Nothing about `phys: u64` enforces the invariant, but `pread` is private and every call
site is in one `impl` block passing a field of `Transport::Pci` that `pci.rs` resolved from a mapped
BAR. **A module invariant is a real way to be sound**, and the compiler is what makes it one.
Converting would have put twenty `unsafe` blocks in a single file, each restating one sentence, which
is the ritual the milestone block named as the thing to avoid, and it would have made nothing
checkable: an `unsafe fn` whose contract nothing verifies is still a contract nothing verifies.

### Why a newtype does not rescue the context switch

The obvious repair for `switch_user_root` is a `#[repr(transparent)]` newtype that only
`AddressSpace::ttbr0` and `reserved_root` can mint, which would make the function honestly safe
rather than merely honestly documented. It does not work, and the reason is worth keeping:

**The dangerous half of the obligation is liveness, and a `Copy` wrapper over a `u64` launders
exactly that.** An `AddressSpace` can be dropped and its frames recycled while a copy of its composed
value lives on. A borrow would carry liveness, and the scheduler cannot hold one: `sched::switch`
reads the root out from under the `SCHED` lock **on purpose**, so the lock is released before the
context switch, and a lifetime tied to the `AddressSpace` cannot survive that drop. The obligation
stays a sentence. Both call sites now carry the argument that makes it true (the incoming thread is
`Running` with `on_cpu` set before the lock drops, so nothing can reap it across the gap) rather than
a restatement of the contract.

### Two sites the survey missed, and the pattern that missed them

Milestone 82 found its four by looking for the word "caller". Two more had the identical defect and
did not use the word:

- **`virtio::pwrite`** says `// SAFETY: as above.` A comment by reference inherits the defect and
  none of the text a grep can match.
- **`stack::high_water`** says "a mapped stack region", in the passive voice. It names the obligation
  without naming anybody who owes it, which is the same defect stated in a way that reads like a
  fact.

**Passive voice and comment-by-reference are the two blind spots of any text search over SAFETY
comments**, and they are worth knowing before anyone trusts a count produced that way.

Two counts in the survey above are also wrong, from the same cause on the other side:

- "**33 `unsafe fn`s**" is 33 in `kernel/` and `crates/`, not in the tree. The tree had **46** before
  this milestone and **51** after it: `fs_server/` holds 9, `tools/redoxfs_host/` 2, `user/src/` 2.
- "**`user/src/` has none**" is wrong. `user/src/c_shim.rs` has two, `malloc` and `free`, and a regex
  that does not allow `extern "C"` between `unsafe` and `fn` misses both. They are the C ABI's
  contract and are correctly documented; only the count was wrong.

### The bug this found, which is the argument in one line

**Taking `pread`'s comment seriously found a path that made it false.** It claimed every address
reaching it was inside a device-mapped BAR. `Transport::Pci`'s `notify_addr[q]` is **zero until
`setup_queue` resolves it**, and the `NOTIFY` syscall checked only that the queue number was under
`MAX_QUEUES`. So a userspace driver holding a virtio capability could ring a queue it had never set
up, and the kernel wrote a `u16` through `phys_to_virt(0)`: a kernel store, inside no BAR, at a
moment the driver chose. `virtio::notify` now refuses that queue via `Transport::doorbell_ready`, and
a unit test builds the two transport values by hand so it runs on both ISAs and in the mmio-only
configurations that have no PCI function at all.

The mmio transport was never exposed, because it has one fixed notify register and nothing per-queue
to resolve. That is why the defect was invisible from the syscall and only appeared in the PCI arm.

### The worst SAFETY comment in the tree, and it passed every gate

`user/src/net_transport.rs`'s `w16` carried this, over a `write_volatile` into the DMA page:

```
// SAFETY: `invoke` traps to the kernel, which validates the capability and the method
// before acting (user_rt's contract). A caller cannot break an invariant by passing a
// bad slot or method; it gets an error back.
```

There is no `invoke` in the function. The comment was pasted from `mr`/`mw` a few lines below and
describes a different operation, on a different mechanism, with a different contract. Its five
siblings (`r8`, `r16`, `r32`, `w8`, `write_desc`) carry the correct DMA-page sentence, so the defect
is one line in a block of six. **`undocumented_unsafe_blocks` was green on it the whole time**,
because the property it checks is that a comment exists. DECISIONS §61 already carries a BUGS note
predicting this; this is the in-tree instance.

### What is not mechanically checkable, stated plainly

**The milestone's headline property has no gate, and should not be given one.** Whether a SAFETY
comment binds anybody is not a syntactic question, and the measurements say so rather than the
intuition:

- The tree has **937 `// SAFETY:` comment blocks**. **871** are inside a safe fn, which is the normal
  and correct case: an `unsafe` block in a safe fn whose soundness is discharged locally is what
  most correct Rust looks like.
- **36** of those mention a caller. Three are artifacts of this milestone's own prose quoting the
  string `// SAFETY:`, so **33** are real, and **19 of the 33 are legitimate** (the calling *thread*,
  the calling *process*, an IPC caller, or a fact the parameter type already enforces). A gate on
  "SAFETY plus caller in a safe fn" would be wrong more often than right.
- And it would have missed `pwrite` and `high_water`, which are two of the six real ones, for the
  reasons above. A check that is both noisy and incomplete is a nag.

An allowlist ratchet would fix the noise and not the incompleteness, at the cost of 19 entries that
each need a reason written and reviewed. Not worth it against a defect class this small. **This one
is a review discipline**: when you read a SAFETY comment on a safe fn, ask whether the parameter
could have been produced without meeting it.

### What is mechanically checkable, and shipped

A different property, adjacent to the milestone rather than the milestone itself: **every `unsafe fn`
states its contract in a `# Safety` section.** `script/lint` gained that check.

It earns its place because of the shape measured in the survey above: a third of this tree's
`unsafe fn`s contain **no unsafe operation at all**, so neither unsafe lint has anything to fire on
and the rustdoc section is the only enforcement there is. Nothing was checking that it existed.
`clippy::missing_safety_doc` is already on via `-D warnings` and does not cover it: that lint fires
only on an **exported** function, and the interesting ones here (`set_ttbr0`, `write_satp`) are
private to their module.

**It found one violation on its first run**, `fs_server`'s `file_page`, whose contract was written
but spelled `SAFETY:` in the doc comment instead of `# Safety`, so rustdoc rendered it as ordinary
prose and no tool recognised it as the contract.

Two things it deliberately does not do. **It excludes trait-impl methods**, because `GlobalAlloc`'s
`alloc` and RedoxFS's `Disk::read_at` are `unsafe fn` by the trait's declaration and the contract
belongs to the trait; twelve of the tree's 51 are that case, and without the exclusion the check is
twelve false positives out of thirteen. And **it checks that a contract is written, never that it is
true**, which is the same limit `undocumented_unsafe_blocks` has one level down. It is a low bar, and
it is the bar that was missing.

### The same defect outside `kernel/`, which is somebody else's lane

The milestone scoped to the four sites in `kernel/`. The survey pattern, run over the whole tree,
finds **14 more** of the same shape, and they are listed here so the finding lives somewhere a person
reads rather than in a report:

| Site | The comment's claim |
|---|---|
| `user/src/net_transport.rs` `r8` `r16` `r32` `w8` `w16` `write_desc` | "callers pass offsets inside it" (the DMA frame) |
| `user/src/fs_test_client.rs:854` `fill_page` | "the caller keeps within it" |
| `user/src/fs_file_caretaker.rs:77` `get` | "callers clamp `out` to the page" |
| `user/src/fs_nameset_caretaker.rs:107` `get_at` | "every caller clamps `out` and `off` to the page" |
| `user/src/sink.rs:133` `get` | "callers clamp `i` to the page" |
| `user/src/swish.rs:616` `put_page` | "every caller is behind a `dir.is_some()` check" |
| `user/src/line_editor.rs:217` `copy_in` | "offset+len is bounded by PAGE by every caller" |
| `patches/std-nife/overlay/std/src/sys/fs/nife.rs:161` `put` | "callers clamp to it" |
| `crates/user_heap/src/lib.rs:100` `effective_size` | "the caller provides the locking" (a data-race obligation, not an addressing one) |

Eight of the nine rows are the same clamp-to-a-page obligation, which suggests the answer there is
one shared page-slice type rather than nine conversions. That is a design question and wants its own
lane. `crates/user_heap`'s is a different flavour and should be judged separately. The `patches/`
one is in the vendored std overlay, which most gates exclude on purpose.

## The census, and which numbers have a direction (milestone 134)

Everything above is about whether an obligation is *written*. This section is about **how much
unsafe there is and which way it should go**, which calef raised on 2026-08-18 in one question:
*"How much unsafe code is there in a code base? Is that something we should be monitoring and
driving in a particular direction over time?"*

He approved folding it into milestone 134 rather than building it standalone, on the reasoning that
a standalone census produces another one-time number nobody re-takes. So every number here is
derived by `script/lint` on every build; none of them is typed. The register that holds them all,
with the test for what belongs in it, is notes/register-of-measures.md.

### The measurement, and the thing it found

Measured over the Rust that runs on nife, which is every tracked `.rs` file except `vendor/`,
`patches/`, and the host-side tooling in `bench/host/`, `xtask/`, `tools/`, `fuzz/` and `scripts/`.
Each exclusion's reason is in `script/lint` beside the derivation; `patches/` is a real hole rather
than a boundary and the register's BUGS says so.

| | 2026-07-15 | 2026-07-28 | 2026-08-04 | 2026-08-14 | 2026-08-18 |
|---|---|---|---|---|---|
| `unsafe {}` outside `kernel/src/arch/` | 171 | 426 | 728 | 763 | 747 |
| code lines outside it | 7,508 | 19,223 | 58,351 | 64,452 | 80,359 |
| **blocks per 10,000 lines** | 227.8 | 221.6 | 124.8 | 118.4 | **93.0** |
| `unsafe {}` inside `kernel/src/arch/` | 34 | 102 | 128 | 134 | 139 |
| `unsafe impl Send`/`Sync` | 7 | 12 | 15 | 15 | 17 |

(`script/lint` prints the density as an integer, truncated: 92 rather than 93.0. Truncated on
purpose, so a ceiling can never fail a tree that sits exactly on it.)

**The absolute count more than quadrupled and the density more than halved, falling at every
sample.** Both facts are true and only the second one is about this kernel's soundness: the first is
a system being built. That is the whole reason the gate below holds a ratio rather than a count.

Nothing was measuring either. The clearest evidence is a single commit two days before this was
written: `d5a969a2`, "user_rt: one trap instruction, not forty-eight", took the count from **863 to
769 in one change**, 10.9% of all non-arch unsafe, by lifting a panic handler that 48 binaries had
each inlined with two `unsafe` blocks and two SAFETY comments. Its commit message argues from §61
that a SAFETY comment is an assertion and not a formality, and it is exactly right; what it could
not say, because no instrument existed, is that the tree had been asserting that particular
invariant **96 times** and now asserts it once.

### What each number is held to, and why the answers differ

**At most 100** <!--count-at-most:unsafe-density-outside-arch--> unsafe blocks per 10,000 lines
outside `kernel/src/arch/`, which is one per hundred lines of code. The direction is down, because
unsafe outside `arch/` is not paying for hardware access: it is a raw syscall, a shared page, or a
hand-rolled data structure, and each of those has a safe wrapper somebody could write. The ceiling
is written at a threshold the tree crossed **the day before this was written** rather than at
slack: every sample before 2026-08-18 would have failed it, 2026-08-16 included at 111.7. That is
what makes it a ratchet instead of decoration.

**At most 18 `unsafe impl Send`/`Sync` claims** <!--count-at-most:unsafe-thread-safety-claims-->,
and this one has no headroom at all. Each is a hand-written assertion that the compiler is wrong
about a type, which is the most consequential unsafe in the tree: a wrong one is a data race that
no test reliably reproduces. The population moved twice in three weeks, so a zero-slack ceiling
costs a lane one line and buys a written reason for every addition. That is the same trade
`bench/baseline-aarch64.txt` makes and this tree already respects.

Raised from 17 to 18 by milestone 134's Tier A lane (2026-08-22): `kernel/src/bench.rs`'s
`Racy<T>` (E4, application working-set displacement) is a second instance of `sched.rs`'s existing
corruption-canary idiom, one `unsafe impl<T> Sync for Racy<T> {}` guarded by the same argument
that one already carries, a scratch buffer one thread at a time touches, serialized by the caller
rather than by a lock. Same shape, same reasoning, a different file.

**No target for `kernel/src/arch/`**, which is 139 blocks and rising. Driving that number down means
either writing assembly wrong or moving it out of `arch/`, and DECISIONS rule 1 says arch code
belongs there, so a ceiling would be a gate pushing against the architecture. An honest census with
no direction is the right answer. It is not left as prose, though, because prose is where numbers go
stale: `script/lint` prints it on every run, asserted never.

**No second `unsafe fn` count.** The `==> unsafe fn contracts` check above already derives one and
prints it, at **53 declarations** on 2026-08-18, and this file's own "the shape of the 33" heading
has been wrong for days with nothing to say so. Adding a second count on a slightly different scope
would be the exact drift this milestone exists to stop, so the register cites that line instead. The
33 heading is left standing: its table of eleven `unsafe fn`s with no unsafe operation is still the
finding, and renumbering a heading to chase a moving count is the maintenance tax the whole
convention refuses.

### `// SAFETY:` parity is deliberately not a gate

The obvious next check is that every `unsafe {}` block has a `// SAFETY:` comment, compared by
count. **It should not be built, and measuring it is what settles that**, in two ways that both
point the same direction.

`clippy::undocumented_unsafe_blocks` already enforces exactly this, per block rather than in
aggregate, as a hard error through `-D warnings` across all fourteen configurations this script
builds. A count check cannot be stronger than that; it can only disagree with it.

And it disagrees badly, in a way that gets worse the harder you try. A regex anchoring `SAFETY:`
to the head of the comment block above each `unsafe {}` reports **65** undocumented blocks in code
the gate compiles clean. Loosening it to accept the comment mid-line, which is how most of this
tree writes it (`// ... the frame was retyped with GRANT. SAFETY: svc.`), still reports **38**. The
ones read are all false positives: a `#[cfg]` attribute sits between the comment and the block, or
the comment covers a closure whose body holds the block, or it covers the first of two blocks on
one line. A gate whose failures are documents that are right is the gate somebody deletes, which
notes/counted-claims.md names as the way this convention dies.

One residue is worth knowing rather than gating: `patches/std-nife/overlay/` holds 37 blocks and
**15 of them carry no `SAFETY:` comment in any form**, because that code is compiled into `std` by
the farm and by no clippy configuration here. That is a coverage hole in the lint policy rather
than a comment shortage, and it is recorded in the register's BUGS.

### What `user/`'s share is actually made of

`user/` holds 287 of the tree's unsafe blocks, the largest share of any directory, which looks wrong
for userspace in a capability system. Reading the first token inside each block says what it is:

| shape | blocks | what it is |
|---|---|---|
| `invoke(...)` | 114 | one raw capability invocation, the userspace syscall |
| `read_volatile` / `write_volatile` | 102 | a byte or word through a granted shared page |
| `from_raw_parts` / `from_raw_parts_mut` | 25 | the same page as a slice |
| `core::arch::asm!` | 12 | entry stubs and the trap |
| everything else | ~34 | mixed |

So it is neither raw pointer arithmetic nor a missing abstraction in the usual sense. **Two
populations, and both are one wrapper away.** The 114 `invoke` sites all call one `unsafe fn` whose
own `# Safety` section says *"the kernel validates the capability and the method before acting; that
is its whole job. The caller is trusting the kernel, not the other way around"*, which describes an
obligation on nobody. It is not simply mismarked: a few methods (`aspace::MAP_INTO` among them) can
perturb the caller's own address space, so *some* obligation is real. But it is a per-method
obligation carried by a single all-methods signature, and 114 blocks assert it identically. The 127
volatile and slice accesses are the same story about granted pages.

Both are the shape `d5a969a2` already fixed once, in one commit, for the panic handler. Neither is
this milestone's work; the handoff in its lane report proposes it.

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

### EXAMPLES: finding a SAFETY comment that binds nobody

The `# Safety` check is part of `script/lint` and needs nothing:

```sh
script/lint 2>&1 | grep -A20 'unsafe fn contracts'
# unsafe fn contracts: 51 declarations (12 trait-impl methods, whose contract is the trait's),
#                      every other one has a `# Safety` section
```

To see it fail, take a section off and put it back:

```sh
# delete the `/// # Safety` line above `pub unsafe fn paint` in kernel/src/stack.rs, then:
script/lint
# lint: unsafe fn with no `# Safety` section in its rustdoc:
#   kernel/src/stack.rs:125  paint
git restore kernel/src/stack.rs
```

The judgement half has no gate, so it is a grep plus reading. This is the pattern that found the
four, with its two blind spots (a comment saying "as above", and one in the passive voice) named so
the next person does not repeat the undercount:

```sh
# Candidates: a SAFETY comment inside a fn that is not an `unsafe fn`, mentioning a caller.
# Expect ~33 hits and expect most of them to be legitimate; this is a reading list, not a verdict.
git grep -n 'SAFETY:.*caller' -- ':!vendor' ':!notes'

# The blind spots. Neither of these says "caller", and both were the real thing:
git grep -n 'SAFETY: as above'          # inherits the defect and none of the matchable text
git grep -nE 'SAFETY: (a|an|the) [a-z]' # passive voice: an obligation with nobody owing it
```

For each hit, the question is not whether it says "caller". It is **could this parameter have been
produced without meeting the obligation?** `sched.rs`'s `endpoint_of` takes `&Scheduler`, which only
the lock guard can produce, so its sentence restates a guarantee. `switch_user_root(u64)` took
anything at all.

## BUGS (milestone 112)

**The `# Safety` check parses Rust with a regex and a brace counter.** It matches an `unsafe fn`
declaration at the start of a line and tracks `impl ... for ...` blocks by nesting depth. A
declaration split across lines by `rustfmt` would be missed, and a brace inside a string literal or a
comment miscounts the depth. The tree has neither shape today and the check was verified against the
real declarations, but this is a text scanner, not a parser. The same caveat applies to `script/lint`'s
dead-code and `#[path]` checks, which are built the same way.

**It cannot see a contract in the wrong place.** A `# Safety` section on the enclosing `impl` block,
or in the module doc, does not count; the check wants it on the item. That is the intent (a reader
meets the function), but it means a legitimate arrangement could be flagged. Nothing in the tree is
arranged that way yet.

**Nothing checks that a SAFETY comment is true, relevant, or about the operation it sits over**, and
milestone 112 did not change that. `net_transport`'s `w16` carried a comment about capability
invocation over a raw store for as long as the file has existed, and every gate was green on it.
Fixing that one line does not make the next one visible.

**The `# Safety` count moves with the tree and must be taken from the merged tree.** 51 declarations
and 12 trait-impl methods were measured on milestone 112's branch on 2026-08-04. Two concurrent lanes
adding unsafe code would both report honest numbers that disagree, which is the failure CLAUDE.md
records for the Kani harness count.

**The riscv64 `user` gap noted at the top of this file is still open.** `script/lint` compiles
`user` and `user_rt` for aarch64 only, so nine of the fourteen sites in the handoff table above are
linted on one ISA.

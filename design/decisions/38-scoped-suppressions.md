# 38. A suppression is scoped to an item and carries a reason, or it does not ship (milestone 41)

**Status: DECIDED.**

**Decided 2026-07-30**, after triaging every `allow(dead_code)` / `allow(unused)` in the tree. This
extends §35's disposition rule from scanner alerts to compiler warnings, which is where the same
failure was already happening and nobody was counting.

## The rule

**A dead-code suppression is an item attribute (`#[allow(...)]`), never an inner attribute
(`#![allow(...)]`), and it says which configuration has no caller.** `script/lint` enforces the
first half; the second half is a review expectation, and the phrasing that satisfies it is a `cfg`
predicate rather than prose. Prefer, in order:

1. **Delete the code.** Nothing calls it, git remembers it, and a reader who trusts the comments
   should not have to find out the hard way that a function is decorative.
2. **`#[cfg_attr(<the configuration with no caller>, allow(dead_code))]`.** `not(test)` when the
   tests are the callers; `feature = "bench"` when a boot mode compiles the caller out;
   `target_arch = "riscv64"` when the item is one ISA's. The attribute then makes a *checkable*
   claim: if the caller disappears from the configuration that had one, the gate says so.
3. **A bare `#[allow(dead_code)]` with a written reason**, for the cases where nothing calls it in
   any configuration and it stays on purpose: an arch-contract stub, a diagnostic that is off by
   default. Rare, and the reason is the whole justification.

## Why an inner attribute is different in kind

An item allow is a decision about one item. An inner attribute is a decision about **every item the
module will ever contain**, including the ones written after it, by someone who never saw the
comment. That is not a suppression, it is a policy, and it decays the moment the module grows.

The measured cost was not hypothetical. Six files carried module-wide `#![allow(dead_code)]` over
**5,831 lines**, including `sched.rs` (3,166) and `arch/aarch64/mmu.rs` (1,275), and `main.rs`
carried `#![cfg_attr(target_arch = "riscv64", allow(dead_code))]`, which blindfolded the **entire
kernel crate** on one of two supported architectures. `script/lint` runs clippy with `-D warnings`
and reported success across all of it. Same class as the conflict markers that survived a full gate
run and §27's four-times-corrected record: **the tooling said fine because nothing was looking.**

## What the un-blindfolding actually found, which is the part worth keeping

Mostly not dead code. That is the honest result and it is why the ratchet matters more than the
cleanup: the value was never in the deletions, it was in learning that the claims were unchecked.

- `sched.rs`: five dead items out of 3,166 lines, but one of them (`spawn_balanced`) carried a doc
  comment asserting "which is why the SMP balance test uses it", and the test had moved to plain
  `spawn` when §28 landed. A false comment in a codebase commented this heavily is the expensive
  kind of dead code.
- `mmu.rs`: two. Four more functions were suppressed unconditionally while being exercised by tests,
  so the gate could not have noticed a test dropping them.
- **A parity gap on the second ISA**, found only because the crate-wide riscv allow came off:
  `user_can_read`/`user_can_write` had no caller anywhere on riscv64, because the confused-deputy
  test is `cfg(target_arch = "aarch64")`. The check that stands between U-mode and the kernel was
  proved on one ISA, and on the ISA where it matters *less*: RISC-V has one root register, so the
  same tables translate user and kernel addresses and the `U` bit is the only line of defence.
- **A vestigial input path**: `console::rx_read` and `Ns16550::read_byte` were dead in every
  configuration including `--features shell`, because the byte is read by the userspace input driver
  through its device capability. Milestone 20's kernel-side reader had outlived its own design.

## Rejected

- **Deleting `user_can_read`/`user_can_write` as unused.** They are the worked example
  notes/capabilities.md leans on, and a test can prove them, which is strictly better than either
  deleting them or allowing them. The rule's first preference is deletion; a *test* beats it.
- **Keeping a narrowed crate-wide allow for riscv64** (`all(target_arch = "riscv64",
  not(feature = "shell"))`). It would have been true, and it would still have covered every future
  item in the crate. The whole point is that scope, not accuracy, is what makes an inner attribute
  the wrong tool.

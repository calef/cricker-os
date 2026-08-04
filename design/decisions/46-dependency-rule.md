# 46. Thin primitives or whole subsystems; we write everything in between

**Decided 2026-07-30 (Chris), after the calendar crate made the absence of a rule visible.** The
practice was already unanimous and written down nowhere, which is the state that produces an
inconsistent decision the first time someone does not share the instinct.

## What the tree actually does

Five external dependencies exist outside `vendor/`:

| Crate | Where | Kind |
|---|---|---|
| `aarch64-cpu` | kernel | thin: architectural register definitions |
| `tock-registers` | kernel | thin: an MMIO register abstraction |
| `spin` | kernel | thin: spinlocks |
| `smoltcp` | user | **whole subsystem**: a TCP/IP stack |
| `redox_syscall` | fs_server | forced by the vendored engine |

Plus **RedoxFS**, vendored under `vendor/` with a pin and a divergence patch (§34). Against that,
**thirty crates have no external dependencies at all.**

The shape is sharp and unaccidental: **thin architectural primitives, or entire subsystems we would
never write. Nothing in between.** Everything in between is written here.

## The test, in order

1. **Is it on the verification path?** Then write it. This is the load-bearing reason and it is not
   about pride: **you cannot restructure someone else's crate to make a model checker tractable.**
   `crates/calendar` is the worked example: the parser gained a byte-level entry point because
   `from_utf8` made CBMC branch on length every step (ten minutes to seventeen seconds, and a better
   API); monotonicity was rephrased as its induction step over adjacent days (228s to 40s, same
   theorem); `div_euclid` on a symbolic timestamp had to be kept out of the harness entirely. Three
   other lanes hit the same wall the same day (`ntp_proto`'s fixed-point multiply, `gpt`'s
   table-driven CRC) and all three resolved it by changing code. None of that is available in a
   dependency.
2. **Is it a whole subsystem we would never write?** Then take it, vendored, under §34's conditions.
   RedoxFS and smoltcp are the model: a filesystem and a TCP/IP stack are each larger than the thesis
   they would serve, and confining them **is** the thesis.
3. **Does it touch the kernel, the ABI, or a capability?** Then write it. Rules 1 through 3 exist so
   that surface stays ours and stays narrow.
4. **Otherwise**, prefer writing when the specification is complete and checkable, and prefer
   depending when correctness is won by *exposure* rather than by reading the spec.

## Rule 4 is what stops this becoming "always write", and crypto is the case

The Gregorian calendar is **fully specified**: every rule is written down, and a proof over all
3,652,425 days in range settles it. Nothing about being widely used tells you more than the quantifier
does.

Cryptography is the opposite: **take it, do not write it.** Correctness there includes resistance to attacks not yet published and side-channel
behaviour no specification states, and that is bought by years of exposure and review. A proof that
our AES matches the spec would not make it safe to use.

So the distinguishing question is not size. It is **whether the spec is the whole of correctness.**

## Amendment (2026-07-31): taking is not vendoring, and crypto is a *dependency*

An earlier wording of rule 4 said crypto should be **vendored**, which conflated two decisions. Rule 4
is about **write versus take**. Whether a taken thing is *vendored* or *depended on* is separate, and
does not follow from it.

**The tree's actual trigger for vendoring is "we must patch it."** RedoxFS is vendored because it
needed a divergence patch to build `no_std`, and `script/vendor-verify` exists to prove exactly that:
"upstream **plus our recorded patches**". **smoltcp is a whole subsystem and an ordinary
dependency**, because nothing needed changing. So "subsystem, therefore vendor" is not what this tree
does, and never was.

RustCrypto's crates are already `no_std`, so no patch is needed and the trigger never fires. With no
divergence, `vendor-verify` has nothing to prove that `Cargo.lock` does not already.

**And for crypto in particular, vendoring is actively worse.** Advisories are the whole point in that
category, and `cargo-deny` / `cargo-audit` work against registry versions; a vendored copy is
invisible to an advisory until a human notices. Milestone 42 named that gap in general terms: "we
confine code we did not write; an advisory against it is invisible today", and crypto is where it
bites hardest. Vendoring it would take on the maintenance burden **and** give up the pipeline that
makes the burden survivable.

So: **crypto is an ordinary dependency**, pinned in `Cargo.lock`, gated by `deny.toml` and
`script/supply-chain`. Vendor only what must be patched.

## The honest costs of writing, recorded so they are not rediscovered as complaints

- `crates/calendar` is 1,538 lines and adds **~7 minutes** to `script/verify`, where it is now the
  largest single entry in a 95-harness suite. If that ratio repeats the gate becomes something people
  skip, and a skipped gate is worse than none, which is the same lesson `script/fmt` taught on
  2026-07-30 from the other direction.
- Battle-tested libraries have already found the subtle bugs. Our harnesses were validated by
  falsification (reducing the leap rule to `% 4 == 0` fails in 8s): that proves they would catch an
  error, **not that they caught one we had written**. The gain is a quantifier and a better API, not
  bugs found.

## The process failure that produced this entry

There was no decision to write `crates/calendar` rather than depend on `time` or `chrono`, both of
which support `no_std` and would have covered most of it. The lane brief said "build the crate", and
the choice was made **by omission, in a prompt**, rather than on the record. The outcome was
consistent with what this tree does everywhere; it was consistent with nothing anyone could point at.
That is the gap this entry closes.

## What is already in place and does not change

`deny.toml`, `script/supply-chain` and `script/vendor-verify` from milestone 44, and `vendor/`'s pin
plus divergence-patch discipline. A dependency taken under rule 2 goes through all of them.

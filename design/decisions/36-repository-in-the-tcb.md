# 36. The repository is part of the TCB (milestones 44 and 42)

**Status: DECIDED.**

**Decided 2026-07-30.** §14 promises a verified core that confines code we did not write. That
promise is only as strong as our ability to say *which* code we are running and *how it got in*, and
both of those are properties of a GitHub repository rather than of the kernel. So the repository gets
the same treatment as a kernel boundary: state what is claimed, make the claim checkable, and write
down what is not claimed.

This section covers milestone 44 (policy, private reporting, code scanning, pull requests) and
milestone 42's non-fuzzing half (advisories, licences, vendored integrity), because they are one
question wearing two milestone numbers.

## The scope line in SECURITY.md, which is the only interesting part of it

A security policy for a demonstrator is mostly a scope argument. `SECURITY.md` draws it at
**confinement**: capability forgery or widening, MMU escape, DMA escape, IPC confusion, a syscall
argument that panics or corrupts the kernel, TOCTOU across the shared pages every service contract
uses, and the foreign-language and vendored seams (§27, §31). Out of scope: that a demonstrator under
QEMU is not a production system, that a hardening feature on the roadmap
(design/roadmap/README.md) is missing, and
anything that requires already being init, which §14 already names as the privileged unverified
component.

**The distinction that carries the weight** is "a missing feature is a roadmap item; a defence that
is *claimed* and does not work is a vulnerability." That is the honest version, and it is also the
demanding one: every claim this project publishes becomes something a reporter may hold us to.

## Pull requests into `main`: a ruleset, because discipline is not a property

Today "merge when green" is a decision a human makes each time. The evidence that this is not enough
arrived on its own: `gh pr merge --auto` was used on 2026-07-30 and **silently did nothing**, because
GitHub only queues auto-merge when something is actually blocking the merge, and with no required
status checks nothing was. The merge went through immediately, unchecked, and looked identical to
one that had waited. A red `main` had already gone unnoticed for two days in exactly that way.

So: required status checks on `main`, applying to the repository owner, with linear history. The
"applies to the owner" part is the whole point on a solo repository; an exemption for the one person
who pushes is an exemption for every push. The gate is not there because the maintainer is
untrustworthy, it is there because `--auto` failing open is invisible and human vigilance is not
version-controlled. The exact ruleset is in notes/repo-hardening.md, because it is applied through a
web UI and nothing in the tree can enforce it.

**The cost is real and accepted:** every change becomes a branch and a PR, and a one-line typo fix
waits for a Kani job. The mitigation is that the checks are already fast enough to be tolerable and
the alternative has already failed twice this week.

## Code scanning: stay on default setup, and record the coverage number as the caveat

Default setup is running (§35) and finds all five cargo workspaces by itself, so the obvious argument
for an advanced (committed-workflow) setup, "it would see more of the tree", is **false** and was
checked rather than assumed: the extractor reports `176 out of 176 Rust files`.

What the same log shows is the caveat that matters more. **60 of those 176 files were extracted with
errors** and 116 without; the extractor ran with `cargo_target: None` (the host) and `cargo_features:
[]`. The kernel is `no_std` on two bare-metal targets and does not build for the host at all, so
CodeQL is analysing it in a configuration that does not exist, with macro expansion failing across
`assert_eq!`, `vec!` and friends. **"Zero alerts" therefore means less than it looks**, which is the
same honesty §35 applied to the gap between Kani and CodeQL, aimed at CodeQL itself.

Advanced setup could set `cargo_target` and exclude `vendor/**`. It is still not worth it yet:
the Rust extractor is in preview and moving (CodeQL 2.26.2, rust-queries 0.1.39), default setup
tracks its improvements for free, and a pinned workflow would freeze today's limitations while adding
a maintained file and a matrix over two ISAs. **Revisit on a stated trigger**, not on a feeling: an
alert lands in `vendor/**` (upstream's to fix, per SECURITY.md, and noise here), or the
extracted-with-errors fraction stops falling, or a query we want is unavailable by default.

## Supply chain: configure it deliberately, and expect the first run to find something

`deny.toml` is written rather than defaulted, with a reason next to every knob, because a default
config is wrong in both directions at once. It narrows the graph to targets we actually build (the
default drags `windows-sys`, `wasi` and RedoxFS's redox-native half into the verdict for code nothing
here compiles, and noise is how an alert list becomes wallpaper), and it tightens what remains:
`unmaintained = "all"`, `yanked = "deny"`, an allow-list of licences rather than a deny-list, and
`unknown-git`/`unknown-registry` denied so a dependency repointed at somebody's fork is loud.

First run: no advisories, no yanked crates, no unknown sources, everything permissive. Three real
findings: one duplicate (`getrandom` 0.2 and 0.4, both under redoxfs, host-side only, skipped with a
reason), three licences beyond MIT/Apache-2.0 that are genuinely needed (BSD-3-Clause, 0BSD,
Apache-2.0 WITH LLVM-exception), and two crates that could not be distinguished from a `version = "*"`
dependency until they declared `publish = false`.

**Vendored integrity is the half that changes a claim rather than adding a check.** §34 and milestone
32 say we run *upstream RedoxFS 0.9.1*, and vendor/README.md listed the divergence "exhaustively".
Nothing verified that sentence and it was already wrong: the vendored `Cargo.lock` had been deleted
and regenerated, re-resolving 25 dependencies. `script/vendor-verify` now hashes the published
tarball, applies a committed divergence patch with zero fuzz, and requires byte identity with the
tracked tree. Applying the patch *and then* comparing is what makes it airtight, since a hunk landing
at the wrong offset still exits 0.

**What none of this covers:** whether upstream 0.9.1 was trustworthy in the first place. That is a
trust decision made by reading the code (notes/redoxfs-audit.md), and a hash cannot make it for us.

## Deliberately not here

**Fuzzing** (milestone 42's third leg). Which parsers get harnesses, what a corpus is committed
against, and how a fuzzer's findings get triaged against §35's three dispositions is its own design
pass, and bolting a `cargo-fuzz` job onto this milestone would have produced a job nobody reads.

## Rejected

- **A `SECURITY.md` that promises a response time.** A one-person project that publishes an SLA it
  will miss has published a falsehood, not a policy. "About a week to acknowledge, no remediation
  timeline, no bounty" is worth more than a number nobody will hit.
- **Branch protection without required checks.** It would block direct pushes while still letting a
  PR merge red, which is the failure that already happened wearing a different hat.
- **Loosening cargo-deny until it passes.** The `getrandom` duplicate is skipped with a written
  reason and an expiry condition (the next redoxfs pin); the alternative, `multiple-versions = "warn"`,
  would have silenced every future duplicate to avoid explaining one.

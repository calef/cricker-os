# 81. A dependency stays upgradable; we suppress churn, never the upgrade

**Status: DECIDED.** calef decided this on 2026-08-13, on the pull request that corrected the
mistake described below: *"We want the upgrades on all of our dependencies. We don't want to
foreclose upgrades."*

§46 decides what we are willing to depend on. This decides what we owe a dependency once we have
taken one, and it exists because those two questions have different answers and the second had none.

## The rule

**A dependency we have taken stays reachable by an upgrade.** Nothing in this repository may put a
version permanently out of reach without saying, at the place it does it, that it is doing so and
why.

Update noise is a real problem and it has real fixes. The distinction that matters:

| Mechanism | What it does to the noise | What it does to the upgrade |
|---|---|---|
| `groups` | folds many pull requests into one | leaves it reachable |
| `schedule` | makes them arrive less often | leaves it reachable |
| `open-pull-requests-limit` | caps how many are open at once | leaves it reachable |
| `ignore` | removes them entirely | **forecloses it** |

The first three are ours to reach for freely. The fourth is a decision about the dependency graph
rather than about the queue, and it is calef's.

## The incident

On 2026-08-10 Dependabot opened four pull requests: `digest` 0.10 to 0.11, `hmac` 0.12 to 0.13,
`md-5` 0.10 to 0.11, `md4` 0.10 to 0.11. All four failed CI, and they had to. RustCrypto ships those
crates as one coordinated release, so bumping any one of them alone produces a graph that does not
build. They were re-proposed every week and could never go green.

The fix applied on 2026-08-13 was an `ignore` block over all four majors. It stopped the churn, and
it also froze `crates/ntlm`'s crypto stack until somebody thought to edit `.github/dependabot.yml`
again, which nothing would have prompted anyone to do. The justification written at the time
(protocol-fixed algorithms, no benefit to a bump) held for `md-5` and `md4`, whose algorithms NTLMv2
mandates and which nothing here chooses. It did not hold for `digest` and `hmac`, which are generic
trait crates that evolve for good reasons and were pinned here only by their coupling to the other
two.

The replacement is a group. One pull request bumping all four together is a consistent graph, it
builds once `crates/ntlm` moves to the new `Digest` and `Mac` APIs, and the choice of when to spend
that migration stays a choice.

**The mistake worth naming is not the `ignore`. It is that a standing policy decision about the
dependency graph rode in on a pull request whose stated purpose was unblocking CI.** A four-line
config block is exactly the size at which that happens without anyone noticing.

## Why this repository in particular cannot afford a frozen dependency

§46's amendment decides that crypto is an ordinary dependency rather than a vendored copy, and the
reason it gives is the advisory pipeline: `cargo-deny` and `cargo-audit` work against registry
versions, so a vendored copy is invisible to an advisory until a human notices.

That reasoning only pays out if we can take the fix when it comes. An advisory against `digest` 0.10
is worth exactly as much as our ability to move off `digest` 0.10 that week, and a project that has
foreclosed the major has a cold, unpracticed, unbuilt upgrade path at the moment it needs a warm one.
So the same argument that made these crates dependencies makes freezing them the wrong shape.

The general form: **an advisory pipeline is a detection mechanism, and a detection mechanism whose
remediation is unreachable is DECISIONS §35's wallpaper failure**, an alert nobody can disposition.

## What this does not cover

**Reproducibility pins are not foreclosures**, and the distinction is whether anything is trying to
move the number. `.qemu-version`, `.cargo-deny-version`, `.cargo-fuzz-version` and
`.cargo-mutants-version` fix a tool so that CI and a developer machine agree, which for
`.qemu-version` is load-bearing: the benchmarks are icount instruction counts and a different
emulator legitimately moves them. Those are raised deliberately, by a commit that says so.
`rust-toolchain.toml` is the same kind of pin and has a whole workflow devoted to proposing bumps to
it.

The test is not "is a version written down". It is **"if a newer version exists, does anything
here ever tell us?"**

## The mechanism

Prose is rung three of CLAUDE.md's ladder and this belongs on rung two, because the failure mode is
precisely that nobody remembers to check. `script/lint` refuses an `ignore:` in
`.github/dependabot.yml` that does not carry an `EXCEPTION (§81):` comment naming its reason.

The gate blocks the **silent** foreclosure, not the deliberate one. An exception is allowed here, the
same way CLAUDE.md allows an exception to any rung, and it has to say that it is one. If a
dependency genuinely must be held, write the marker and the reason and the gate stays quiet.

## BUGS

- **The gate sees one file.** A dependency can be frozen in ways `.github/dependabot.yml` never
  mentions: a caret constraint in a `Cargo.toml` that upstream has moved past, a vendored copy under
  `vendor/` whose pin nobody raises, an upstream that stopped publishing. None of those trip this
  check, and the only thing watching them is a person reading `cargo outdated` or an advisory.
- **A group converts many small failures into one large one.** Four unbuildable pull requests become
  one pull request that is also red until somebody does the migration. That is better, because it is
  red for a reason a human can act on rather than for a reason nothing could ever fix, but it is not
  free and the work does not disappear.
- **This says nothing about when to take an upgrade**, only that it must remain possible to. A major
  bump still has to earn its way through the queue like anything else, and nothing here makes a
  migration urgent that was not already.

# 44. GitHub repository hardening: policy, private reporting, code scanning, pull requests

**Status: PARTIAL.**

**The committable half is built 2026-07-30 (DECISIONS §36); the settings half is written down and
waiting on an admin (notes/repo-hardening.md).** `SECURITY.md` states the scope at confinement, with
the distinction that carries the weight: a missing feature on this roadmap is a roadmap item, a
defence that is *claimed* and does not work is a vulnerability.

**Code scanning: checked rather than assumed, and the answer was no.** The obvious argument for an
advanced (committed-workflow) setup is that it would see more of the tree; the extraction log says
otherwise, because default setup finds all five cargo workspaces by itself and reports 176 of 176
Rust files scanned. The number worth carrying forward is the other one: **60 of those 176 were
extracted with errors**, against the *host* target with default features, for a kernel that does not
build for the host at all. "Zero alerts" means less than it looks, and that belongs next to the claim
rather than in a footnote.

**Waiting on Chris**, both in notes/repo-hardening.md with exact steps: enable private vulnerability
reporting (the committed `SECURITY.md` currently points at a button that does not exist), and apply
the `main` ruleset with seven required checks, an empty bypass list, and *not* linear history. Apply
the ruleset only after this branch merges, because one required check does not exist yet and a
required check that never reports blocks every merge.

**In brief.** Four items, and they split into files we can commit and settings someone with admin has to toggle. **Files:** a `SECURITY.md` policy stating what is in scope (the kernel's confinement boundaries) and what is not (a demonstrator running under QEMU is not a production system), and a code-scanning workflow. **Settings:** private vulnerability reporting, and a ruleset requiring pull requests into `main`. Note the plumbing for the last one already exists, since CI runs on `pull_request`; what is missing is the branch protection that makes it mandatory. One thing to check rather than assume: **CodeQL's Rust support** has been moving through preview, so confirm its current state before committing to it; if it is not ready, the practical scanners are the clippy gate we already run, `cargo-audit`/`cargo-deny` from milestone 42, and a SARIF upload from whatever does work

**Signed commits: a fifth item, deferred on purpose and not previously written down here.** Added
2026-08-04 from `notes/repo-hardening.md:82`, which files it under **Do NOT enable** with its reason:
"Nothing here is signed today; turning this on would block every merge until signing is set up.
Worth doing eventually, as its own decision, not as a side effect of this one."

That reasoning is right about sequencing and it left the eventual decision with no home, so it lives
here now. Three things it needs, and the second is the one with a real cost:

- **A key and a signing method.** SSH signing is the cheap path (git supports it, GitHub verifies it,
  and the key already exists on the machine that pushes); GPG is the older path with more tooling
  around it.
- **Every automated committer signs too, or the rule blocks them.** This tree merges lane work
  constantly and takes Dependabot pull requests, and a required-signature rule applies to both. That
  is the part to check before turning the setting on, because the failure mode is the same one the
  ruleset note already warns about: a requirement nothing can satisfy blocks every merge.
- **A statement of what it buys here.** For a public repository with a security thesis, signatures
  say the commits are from who they claim to be, which is a supply-chain property adjacent to
  milestone 42's `cargo-audit`/`cargo-deny` work rather than a code-quality one. Say that plainly, or
  it reads as ceremony.

Sequence it after the ruleset lands and after the required checks are green, for exactly the reason
the ruleset itself is sequenced after this branch merges.

**Why it matters.** **a public repository with a security thesis should be able to receive a security report privately**, which today it cannot. The pull-request item also changes how this project is built: work currently lands by merging feature branches into `main` locally, and requiring PRs would put every merge behind the same gate rather than trusting the person merging, which is the discipline that caught the reap flake and the conflict markers only because I happened to run the gates by hand

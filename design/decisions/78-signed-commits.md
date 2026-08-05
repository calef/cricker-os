# 78. Signed commits: worth doing, and not as a side effect

**Status: PROPOSED.** (raised 2026-08-04 when §73 closed, so the deferral does not die with the
section that carried it.)

**What.** Require signed commits on `main`. `notes/repo-hardening.md:82` files this under **Do NOT
enable** with a reason that is about sequencing rather than merit: "Nothing here is signed today;
turning this on would block every merge until signing is set up. Worth doing eventually, as its own
decision, not as a side effect of this one."

That reasoning is right and it left the eventual decision homeless. Milestone 44 is BUILT without it,
so this is the one piece of that milestone deliberately carried forward rather than dropped.

**Three things it needs, and the second has the real cost.**

- **A key and a method.** SSH signing is the cheap path: git supports it, GitHub verifies it, and the
  key already exists on the machine that pushes. GPG is the older path with more tooling around it.
- **Every automated committer signs too, or the rule blocks them.** This tree merges lane work
  constantly and takes Dependabot pull requests, and a required-signature rule applies to both. **This
  is the part to check before turning it on**, because the failure mode is the one this repository
  already lived through on 2026-08-04: a requirement nothing can satisfy blocks every merge. Measured
  today, `git log --format=%G?` over recent `main` returns a mix of `E` and `N`, so nothing is
  uniformly signed and Dependabot's commits are GitHub's to sign, not ours.
- **A statement of what it buys here.** For a public repository with a security thesis, signatures say
  the commits are from who they claim to be, which is a supply-chain property adjacent to milestone
  42's `cargo-audit`/`cargo-deny` work rather than a code-quality one. Say that plainly, or it reads
  as ceremony.

**The recommendation.** Not yet, and for a reason that got sharper today: the repository just adopted
"require branches to be up to date before merging", which already serialised a ten-pull-request
backlog. Adding a second requirement that can block every merge, while the first one's cost is still
being measured, stacks two novel failure modes. Do it when the merge pipeline is quiet, verify
Dependabot's commits are accepted **before** making the rule blocking, and start with the rule in a
non-enforcing state if the ruleset supports it.

**Not blocked.** Nothing waits on this. It is recorded so that a deliberate deferral stays a decision
rather than becoming an omission.

# 73. Milestone 44's ten admin minutes, which only calef can spend

**Status: DECIDED.** calef applied it on 2026-08-04, the evening it was raised. This section records
what was applied and what turned out not to be applicable, because the *reasons* are what a reader
needs later and a closed section that only says "done" throws them away.

**Applied, measured against the repository afterwards rather than assumed:** private vulnerability
reporting is **enabled**, so `SECURITY.md` no longer points at a button that does not exist. The
`main` ruleset is **active** with seven required checks, **zero bypass actors**, and no
`required_linear_history` rule, which was the deliberate choice: this tree's merge commits are how
`git log --first-parent` reads as one entry per piece of work, and linear history would destroy that.
**Require branches to be up to date before merging** was turned on the same evening, after a merge
order put two individually-green pull requests onto a red trunk.

**One caveat below turned out to be moot.** `undefined-behavior check` (formerly `miri`) cannot be a
required check at all: it runs on a weekly cron and `workflow_dispatch`, never on a pull request. The
same is true of `mutation`. A check that does not run on a pull request cannot gate one.

**Five checks run on every pull request and deliberately do not block:** `cpu matrix`, `fuzz`,
`CodeQL` and the two `Analyze` legs. `cpu matrix` is the tree's known load-sensitive check
(notes/cpu-models.md), and making a load-sensitive check blocking means merges fail for reasons
unrelated to the diff.

**`verify (Kani proofs)` stays required**, and that was argued rather than assumed. It is the long
pole at 28 to 36 minutes, and under the new strict rule every stale branch pays it again. It stays
because `script/verify --affected-since` already skips it in **11 to 20 seconds** for changes that
cannot reach a proof, so the cost lands only on changes that can. For a project whose headline claim
is machine-checked verification, that is the right price.

**What.** Enable private vulnerability reporting (the committed `SECURITY.md` points at a button that
does not exist), and apply the `main` ruleset with its required checks, an empty bypass list, and
**not** linear history. Exact steps are in notes/repo-hardening.md.

**One caveat that moved today.** The check names changed: `miri` is now `undefined-behavior check`,
and the HVF leg is deliberately **not** a CI check (GitHub's hosted macOS runners have no nested
virtualization), so it must not appear in the required list.

**Milestone 44 is BUILT** as of 2026-08-04. Its fifth item, **signed commits, remains deliberately
deferred** and now has its own home in §78 rather than dying with this section.

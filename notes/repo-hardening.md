# Hardening the repository itself

Milestone 44 splits cleanly in two. The files are in the tree (`SECURITY.md`, `deny.toml`,
`script/supply-chain`, `script/vendor-verify`, the CI job); the rest is **repository settings**,
which live in GitHub's web UI and cannot be committed. This note is the exact procedure for those,
written to be followed rather than interpreted. The reasoning behind each is DECISIONS.md §36.

Everything below needs admin on `calef/cricker-os`.

## 1. Private vulnerability reporting

**What it does.** Adds a "Report a vulnerability" button to the repository's Security tab, which
opens a draft advisory visible only to the reporter and the maintainers, with a private fork for the
fix. Without it, the only way to report a bug in a project whose whole subject is confinement is a
public issue, which is the opposite of what you want.

Free on public repositories. `SECURITY.md` already tells reporters to use it, so this is the one item
where the committed file is currently ahead of the setting.

**Steps.** Settings → Advanced Security (older UI: "Code security and analysis") → find **Private
vulnerability reporting** → **Enable**.

Or, in one command:

```
gh api -X PUT repos/calef/cricker-os/private-vulnerability-reporting
```

**Verify:** open <https://github.com/calef/cricker-os/security/advisories> and confirm the "Report a
vulnerability" button is present.

## 2. The ruleset on `main`

This is the item with real consequences, and the argument for it is in §36. The short version: on
2026-07-30 `gh pr merge --auto` was used and **silently did nothing**, because GitHub only queues
auto-merge when something is actually blocking the merge, and with no required checks nothing was.
The merge landed unchecked and looked exactly like one that had waited. "Merge when green" is
currently a habit, not a property of the repository.

**Apply this only after the milestone-44 branch has merged**, because one of the required checks
below does not exist yet, and a required check that never reports blocks every merge forever. That
is the single most likely way to get this wrong.

**Steps.** Settings → Rules → Rulesets → **New ruleset** → **New branch ruleset**.

| Field | Value |
|---|---|
| Ruleset name | `main` |
| Enforcement status | **Active** |
| Bypass list | **empty** |
| Target branches | Include **default branch** |

**Leave the bypass list empty, deliberately.** This is the difference between a ruleset and classic
branch protection: with classic protection, admins bypassed by default unless you ticked "do not
allow bypassing", and it was easy to leave the exemption in place. A ruleset exempts nobody unless
you add them. On a solo repository an exemption for the one person who pushes is an exemption for
every push, and the gate exists because `--auto` fails *open* and invisibly, not because the
maintainer is untrustworthy.

### Rules to enable

- **Restrict deletions** — `main` should not be deletable by accident.
- **Block force pushes** — a rewritten `main` is the one mistake with no local copy to recover from.
- **Require a pull request before merging**
  - Required approvals: **0**. A solo repository that requires an approval is a repository that
    cannot merge. The gate here is the checks, not a second person.
  - **Require approval of the most recent reviewable push: OFF.** This one requires an approver who
    is not the pusher, so it is unsatisfiable for one maintainer.
  - Require conversation resolution before merging: **on**. Cheap, and it catches the review comment
    everyone forgot.
  - Allowed merge methods: leave all three enabled.
- **Require status checks to pass** — the list is below.
  - **Require branches to be up to date before merging: OFF.** With several branches in flight at
    once this turns every merge into a race to update first, and the value it adds (catching a
    semantic conflict between two green branches) is largely already covered, because a
    `pull_request` run tests the *merge result*, not the branch tip.

### Do NOT enable

- **Require linear history.** This repository's merge commits carry information: `merge
  bench/service-and-smp: the post-§28 benchmark follow-up` is a sentence about a milestone, not
  bookkeeping. Linear history would force squash or rebase and delete that. The property linear
  history is usually wanted for, "every commit on `main` was tested as a unit", is already supplied
  by required checks running against the merge result.
- **Require signed commits.** Nothing here is signed today; turning this on would block every merge
  until signing is set up. Worth doing eventually, as its own decision, not as a side effect of this
  one.

### The required checks, by exact name

These are the job names GitHub sees, taken from a real run rather than from the workflow file:

```
build + test (host + QEMU)
rustfmt
clippy
verify (Kani proofs)
bench (icount regression tripwire)
coverage (host crates)
supply chain (advisories, licences, vendored integrity)
```

All seven come from `.github/workflows/ci.yml`, which means the names are in the repository and a
rename is a diff someone reviews. That is the property that makes them safe to require.

**CodeQL is deliberately not on the list**, and the reason is operational rather than a judgement
about scanning. Default setup produces four checks (`CodeQL`, plus `Analyze (rust)`, `Analyze
(c-cpp)`, `Analyze (actions)`) whose names are chosen by GitHub, change when GitHub adds or drops a
detected language, and are not visible in any file here. A required check that stops reporting blocks
every merge until an admin edits the ruleset, and CodeQL's Rust support is still in preview.
Its findings are still dispositioned; that is DECISIONS §35's policy, and it does not need a merge
gate to work. Add `CodeQL` to the required list later if it proves stable, and expect to revisit it
whenever the detected-language set changes.

**Verify:** open a throwaway PR with a trivial change. It should show seven required checks, a
merge button disabled until they pass, and `gh pr merge --auto` should now actually queue rather than
merging immediately. That last one is the whole point, so it is worth confirming rather than assuming.

### One thing to fix first, or at least to know about

**`build + test (host + QEMU)` is currently flaky on the CI runner**, and requiring it will surface
that as blocked merges. Commit `91564e9` on `main` failed twice on 2026-07-30 in two *different*
places, both timing-shaped and both green on a developer machine:

- `kernel::smp::tests::every_secondary_runs_scheduled_work` — "secondary cores did not run scheduled
  work in time" (`kernel/src/smp.rs:239`).
- `kernel::user::tests::the_hardware_says_el0_cannot_read_the_kernels_memory` — "EL0 cannot read its
  own .text, so the check refuses everything and proves nothing" (`kernel/src/user.rs:5056`).

A four-vCPU shared runner emulating a four-core guest under TCG is a much slower and much noisier
machine than the laptop these deadlines were calibrated on. This is a real bug in the tests' timing
assumptions rather than an argument against the ruleset: a gate you have to re-run is annoying, and a
gate that was never there is how `main` stayed red for two days. Fix it, then require the check.

## 3. Code scanning: leave it on default setup

Already enabled and running (three languages: rust, c-cpp, actions). §36 records the reasoning for
staying on default setup instead of committing a workflow, along with the number that should be read
next to every "0 alerts": on 2026-07-30 the Rust extractor reported **176 of 176 files scanned, 60 of
them extracted with errors**, running against the *host* target with default features, for a kernel
that does not build for the host at all.

Nothing to do now. Revisit on a stated trigger: an alert lands in `vendor/**` (upstream's to fix, and
noise here), the extracted-with-errors count stops falling as the extractor matures, or a query we
want turns out to be unavailable by default.

To re-check the coverage number after a future run:

```
gh run list --repo calef/cricker-os --workflow CodeQL --limit 1 --json databaseId --jq '.[0].databaseId' \
  | xargs -I{} gh run view {} --repo calef/cricker-os --log \
  | grep -E "scanned .* out of|extracted with"
```

## 4. What is deliberately still missing

- **Fuzzing** (milestone 42's third leg). Which parsers get harnesses and how a fuzzer's findings
  are triaged against §35's three dispositions needs its own design pass.
- **Signed commits and signed tags.** Related to §36 and to milestone 22's measured boot, and a
  separate decision.
- **A published advisory workflow.** GitHub advisories become useful once there is something to
  advise *about*, i.e. once there are releases (milestone 39).

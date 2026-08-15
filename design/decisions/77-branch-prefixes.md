# 77. The branch-prefix list does not describe the tree

**Status: PROPOSED.** (raised 2026-08-04, found by `script/lint` refusing the branch the roadmap
corrections were on.)

**What.** `script/lint` check 4 accepts `milestone/`, `fix/`, `bench/`, `audit/`, `integration/`,
`finalize/` and `feature/`, and rejects everything else. It runs on the current branch only, and
skips a detached HEAD deliberately, because every CI `pull_request` run builds a merge commit and has
no branch to judge. That design is right and is documented in the script.

**The defect is the list.** Counting merge commits on `main` by the prefix they came from:

| prefix | merges | in the list |
|---|---|---|
| `milestone/` | 28 | yes |
| **`roadmap/`** | **25** | **no** |
| `integration/` | 23 | yes |
| `fix/` | 19 | yes |
| `toolchain/` | 3 | no |
| `feature/` | 3 | yes |
| `decisions/` | 2 | no |
| `ci/` | 2 | no |
| `bench/` | 1 | yes |
| `docs/`, `design/`, `dependabot/` | 1 each | no |

`roadmap/` is the **second most used prefix in the repository** and the lint rejects it. The check
would have refused about 35 of the merges that are already on `main`, and `audit/` and `finalize/`
are permitted while having never been used.

**Why nobody noticed.** The check cannot fail CI by design, so it only ever fires for whoever runs
`script/lint` locally, on whichever branch they happen to be on. A lane on `milestone/` never sees it.
This is a gate whose blind spot is the population it is aimed at.

**The question, which is a vocabulary one and therefore calef's.** Which of `roadmap/`,
`decisions/`, `toolchain/` and `ci/` become legitimate, and which of the tree's uses were mistakes
that should have been something else? The one-offs (`docs/`, `design/`, `dependabot/`) are a separate
call: `dependabot/` is not ours to choose, and `docs/` and `design/` each have a single use and may
simply be `roadmap/` or `notes/` under another name.

**The recommendation.** Add `roadmap/`, `decisions/`, `toolchain/` and `ci/`, since each is
established, each matches a real thing in the tree, and the alternative is a gate that is wrong more
often than the developer is. Exempt `dependabot/` as machine-generated. Leave `docs/` and `design/`
out and let their single uses stand as history. Consider dropping `audit/` and `finalize/`, which
have never been used, or keep them as declared intent and say which.

**No vocabulary change is made here**, because a name is calef's call even when the name is a branch
prefix. The measurement is recorded so the decision can be argued against numbers.

**Practical impact until answered:** `script/lint` exits 1 at its last check on a `roadmap/` or
`decisions/` branch, after all its real work has passed. CI is unaffected, so nothing is actually
blocked.

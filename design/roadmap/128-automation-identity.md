# 128. The automation gets its own identity: a GitHub App token replaces the personal one

**Status: NOT-STARTED.** Minted 2026-08-15 at calef's request, the same day he set
`TOOLCHAIN_BUMP_PAT` on the transferred repository and asked what other developers would need
(answer: nothing, and that answer is what surfaced this).

**Gate: NONE.** Nothing blocks a start; the app creation is calef's to perform, like milestone
120's organization was, because it needs owner rights on `crickertech`. What makes this a later
milestone rather than this week's is honest priority, recorded below.

**In brief.** The toolchain-bump workflow authenticates as a fine-grained personal access token
(PAT) on calef's account, because a PR opened by the ephemeral `GITHUB_TOKEN` triggers no CI
(GitHub's anti-recursion rule; the workflow's own comment records it). A PAT works and has two
structural flaws: it expires on a personal timer, with "bump PRs silently stop getting checks" as
the only symptom, and it couples the project's automation to one person's account, so the
automation breaks the day that account leaves the organization. A **GitHub App** owned by
`crickertech`, installed on `nife`, fixes both: installation tokens are minted fresh per workflow
run (nothing stored expires), and the identity belongs to the organization rather than to a
person. This is the `needs-architect` principle applied to credentials: name the role, not the
person.

## The work, which is small

1. Create the App under the `crickertech` organization (calef; Settings → Developer settings →
   GitHub Apps). No webhook, no public listing. Repository permissions: Contents read/write,
   Pull requests read/write, the same pair the PAT carries.
2. Install it on `nife` only, and store the App ID and private key as repository secrets
   (`AUTOMATION_APP_ID`, `AUTOMATION_APP_KEY`).
3. In `toolchain-bump.yml`, mint the installation token per run (the maintained
   `actions/create-github-app-token` action does exactly this, and taking it is a workflow-only
   dependency, not one in the shipping graph; note it in the §46 spirit anyway) and use it where
   `TOOLCHAIN_BUMP_PAT` is used today. Keep the `|| github.token` fallback: a fork without the
   App still opens PRs.
4. Delete the `TOOLCHAIN_BUMP_PAT` secret and revoke the PAT, in that order, and update the
   workflow's own explanatory comment, which is where this mechanism is documented for the next
   reader.
5. Any future workflow needing to trigger CI on its own PRs reuses the same App rather than
   minting another personal token; that reuse is the milestone's compounding value.

## Why not now, recorded so the deferral is a decision

With one architect, the PAT and the App fail in the same circumstances and the PAT already
exists. The App earns its setup cost at the first of: a second architect joining (the automation
should not be authored as either person), the PAT's first silent expiry (the App never has one),
or a second workflow needing the same authority (two PATs is how credential sprawl starts). Until
one of those arrives, this milestone is deliberately parked, and the PAT's expiry symptom plus
its fix are documented in the workflow comment where the person debugging it will look.

## BUGS

- **The private key is still a stored secret.** An App swaps a stored *token* for a stored
  *signing key*; the win is org ownership and per-run minting, not the absence of a secret. A
  leaked key is revoked in the App's settings, which is at least an org-level act rather than a
  personal-account one.
- **Bot-authored PRs change the byline.** Bump PRs would arrive as `<app-name>[bot]` rather than
  as calef; anything filtering PRs by author (none known in-tree today) would need updating.

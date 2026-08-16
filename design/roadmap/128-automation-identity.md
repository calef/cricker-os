# 128. The automation gets its own identity, and the agents get their own voice

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

## The second deliverable: attribution, added 2026-08-16

calef, reading a repository whose every pull request, comment and review carries his name:
**"it looks like I'm talking to myself a lot and the record would be nice to clarify who is
talking."** That is a documentation defect as much as a vanity one. This project's whole claim is
that a system of this size can be built by one architect and many agents; a timeline in which the
architect appears to write, review and merge his own work in a single voice is evidence *against*
the claim it should be evidence for. The provenance tenet applies to authorship the same way it
applies to names: the record should say who did a thing where a reader meets it.

**Same App, so the setup is shared.** An installation token authors as `<app-name>[bot]` with its
own avatar and badge, so a lane's pull request is visibly not the architect's. What exists today
is half a mechanism: commits carry `Co-Authored-By: Claude ...` and pull request bodies carry the
Claude Code footer, but the *author* of every pull request and comment is calef's account, and
that is the half a reader actually sees.

**The alternatives, recorded so the choice is one:**

- **A separate machine account** (`nife-agent` or similar) works today with no App and reads
  unambiguously. Cost: a second identity to secure, an org seat, and the same
  `GITHUB_TOKEN`-cannot-trigger-CI trap the App was minted to escape.
- **Convention only**, a `**Lane:**` line in the body naming the agent and its milestone. Free,
  cosmetic, and adopted immediately (2026-08-16) as an interim rather than as the answer, because
  a line of prose is rung four and an identity is rung two.

**What it does not fix, and should say so plainly:** an App cannot make the *commits* authored by
the agent, only the pull request and its comments; commit authorship stays with whoever holds the
git identity. `Co-Authored-By` remains the record there, and that split is honest rather than
awkward: the human owns the change, the agent is named as its co-author, and the conversation
around it is visibly the agent's.

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

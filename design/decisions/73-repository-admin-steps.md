# 73. Milestone 44's ten admin minutes, which only Chris can spend

**Status: PROPOSED.** (raised 2026-08-04; waiting on Chris, and nobody else holds the authority to
do it.)

**What.** Enable private vulnerability reporting (the committed `SECURITY.md` points at a button that
does not exist), and apply the `main` ruleset with its required checks, an empty bypass list, and
**not** linear history. Exact steps are in notes/repo-hardening.md.

**One caveat that moved today.** The check names changed: `miri` is now `undefined-behavior check`,
and the HVF leg is deliberately **not** a CI check (GitHub's hosted macOS runners have no nested
virtualization), so it must not appear in the required list.

**Blocked.** Milestone 44 stays PARTIAL until this is done.

# DECISIONS.md

**This file is a signpost, not the decisions.** They live in
[`design/decisions/`](design/decisions/), one file each, split there by milestone 114.

It exists because the tree cites them by their old home. `DECISIONS §14`, `§10`, `§19` appear in the
README, in kernel comments, in milestone blocks and in `CLAUDE.md`, and a reader who greps for the
filename those citations name used to find nothing at all. Milestone 117's first stranger run hit
this within two minutes, and resolved it by guessing at `ls design/`.

## How to read a citation

**`§N` is `design/decisions/N-*.md`.** So `§14` is
[`design/decisions/14-project-direction.md`](design/decisions/14-project-direction.md), and `§7` is
the one about pure logic living in host-testable crates.

- **The index**, with every decision's title and status:
  [`design/decisions/README.md`](design/decisions/README.md).
- **`script/decisions`** builds that index; `script/decisions --check` enforces the numbering and the
  status vocabulary, and verifies that every `§N` cited anywhere in the tree resolves.
- **`script/citations --check`** is the stricter one: it reads the target and fails when a glossed
  citation does not match that record's own title or quote its body. The first proves a citation
  resolves to *some* decision; the second proves it resolves to the right one.

## Why the citations were not rewritten instead

`§N` is short, it is what conversation and commit messages already use, and rewriting every one to a
path would make the tree noisier without making it clearer. The number is the stable name; this file
is how a newcomer learns to resolve it.

## BUGS

- **Nothing checks that this signpost stays true.** If the decisions move again, or the `§N`
  convention changes, this file is a fourth place to update and no gate reads it.

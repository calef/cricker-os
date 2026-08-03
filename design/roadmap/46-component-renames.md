# 46. Rename the components for what they are, and write down the naming rules

**Status: BUILT.**

**Built 2026-07-30, both ISAs.** Five renames in one mechanical commit: `netd` → `net_stack`,
`compd` → `compositor`, `gpud` → `display`, `termd` → `line_editor`, and the crate `crates/linedisc` →
`crates/line_editor`. The scope estimated here at 398 came in at **457 whole-word token replacements
across 4 file moves and 1 directory move** (`netd` 184, `linedisc` 93, `termd` 77, `gpud` 67,
`compd` 36); the estimate was measured before milestones 23 and 37 landed and the tree grew under it,
which is the ordinary way a count like this drifts. The conventions are notes/naming.md, indexed in
notes/README.md, and four of them are checked in `script/lint`: no name ending in `-d`, the word
"daemon" nowhere outside the documents that argue about it, one spelling for contract crates, and a
recognised branch prefix. Each was proved to fail before it was trusted, and the strongest of those
controls is that the `-d` check run against unmodified `main` reports exactly `compd gpud netd
termd`.

**Why it matters.** The rule and its argument are DECISIONS §39. The short version: a `-d` suffix
tells every reader "this is a daemon" before they see a line of code, and a Unix daemon is defined by
the ambient authority this OS deliberately lacks. `netd` holds five explicit capabilities, cannot name
its own callers, is supervised, and can be reaped by something that lacks the authority to build it.
The name is a false claim, which is the same defect as a stale comment except that every reader is
guaranteed to read it. `linedisc` failed the second half of the same test: it is the correct Unix term
of art, and the person who built this system did not recognise it.

**Execution discipline, because this is the change milestone 39 warns about.** One commit, nothing
else in it. **Whole-word tokens only**: `display` and `compositor` already appear as ordinary English
throughout the notes, so this replaces identifiers, not vocabulary. Count the `--bin` name/token
pairing before and after: this is the same `xtask` code where a union merge dropped a `--bin` flag on
2026-07-29 and where git silently duplicated a loop header. Then zero surviving references to any old
name, and the full gates on both ISAs. `script/lint`'s script-documentation check plus the roadmap and
decisions checkers catch prose stragglers.

**Sequencing (the reason this is a milestone and not an afternoon).** It must land *after* milestones
23 and 37, because 23's instance one is the console hot-swap and it is editing `termd`: the file this
renames away, plus `kernel/src/user.rs`, which both lanes share. Landing 398 token replacements
underneath an active branch turns a mechanical commit into a merge fight, which is precisely what it
must never become.

## Second half: the conventions, and checks for the ones a machine can check

Looking for the tree's naming conventions on 2026-07-30 turned up three real inconsistencies, none of
them anybody's decision:

- **Word separation in crate names is split down the middle.** `fs_proto`, `gfx_proto`,
  `dma_validator`, `user_rt` use underscores; `grant_plan`, `crickerfs`, `bitfont`, `line_editor`, `coremark`
  run the words together. Two habits, no rule.
- **The wire contract is spelled four ways**: `fs_proto` and `gfx_proto` (crates, underscore),
  `socket_proto` (a module, no underscore), and `line_editor::proto` (a submodule). One concept.
- **Branch prefixes contain a literal duplicate**: eight in use, including both `feature/` and `feat/`.

Write the *principle* in prose, because it needs judgement and no checker can evaluate it: name a
component for what it is, and prefer a word that parses without prior Unix exposure. DECISIONS §39
already carries the reasoning; the note should point at it rather than restate it.

Then **check the mechanical ones in `script/lint`**, because this project's own pattern is that a
convention which matters gets a checker rather than a paragraph: the roadmap status vocabulary,
DECISIONS numbering, script documentation, conflict markers and module-wide suppressions all became
checks today, and a rule with no enforcement decays (which is the entire argument the dead-code
ratchet was built on):

- **No `-d` suffix on a binary.** §39 made this a rule; without a check it lasts until the first
  inconvenient moment.
- **One spelling for contract crates.** Pick `*_proto` or `*proto` and fail the odd one out.
- **Branch prefixes from a fixed set**, which retires `feat/` versus `feature/`.

The note lands in `notes/` and is indexed in `notes/README.md`; `script/lint` already enforces that
every script has an entry in `notes/scripts.md`, so the precedent for gating documentation exists.

**Why both halves are one lane.** They share a landing point: the note must describe `net_stack`,
`compositor`, `display` and `line_editor` rather than names that are about to change, and the `-d` check
would fail until the rename lands. Splitting them would mean writing documentation that is stale on
arrival, or a checker that is red on arrival.

**Effort: 1 lane estimated**, almost entirely verification rather than editing.

# 97. Citations that name what they cite

**Status: BUILT.** Raised 2026-08-04, from a miscitation found while ratifying a program name, and
then measured. Built the same day as `script/citations`, gated in `script/lint`, documented in
notes/citations.md.

**The count was 23 and is 28.** The original survey matched single-line citations; several are
wrapped across two lines in doc comments, which is the same class of undercount CLAUDE.md already
records for the `#[path]` module survey. Counted from the merged tree with a pattern checked against
the real shapes.

**What happened, established from history rather than guessed.** DECISIONS §24 is "Interrupting the
foreground process: two-tier, shell-held, no new kernel surface". Milestone 24 is "A second aarch64
board: Virtualization.framework". Twenty-three comments across `user/`, `crates/`, `kernel/` and
`user/Cargo.toml` credit the `^C` work to "milestone 24". The first was written on 2026-07-28, and
the roadmap on that date **already** said milestone 24 was Virtualization.framework, so this was
never a renumbering: it was wrong at birth and spread by copy-paste. Several sites now read
"milestone 24, DECISIONS §24", citing one thing twice under two schemes, which is the tell.

**Why no gate caught it.** `script/decisions --check` verifies a cited `§N` resolves to *some*
section; `script/roadmap`'s citation check verifies `milestone N` resolves to *some* milestone.
Both pass here, because both numbers exist. CLAUDE.md already records this blind spot for `§N`
("a well-formed wrong citation is invisible to it") and the roadmap split's own gate repeats the
admission. This milestone is that blind spot's first measured cost.

**The deliverable, two halves:**

1. **The sweep**: correct all 23, by content and not by pattern (each one has to be read to know
   whether it means the decision or something else). Sequence it **after** any in-flight branch
   touching `user/src/system_initializer.rs`, `swish.rs`, `line_editor.rs` or `hello.rs`, because
   a 23-site comment sweep across files another lane is merging is a conflict for no reason.
2. **The enforcement, which is the durable half**: require a citation to carry a name, not just a
   number. `DECISIONS §24 (interrupting the foreground process)` and `milestone 24
   (Virtualization.framework)` are both self-checking: a lint can compare the parenthetical against
   the real title and fail on a mismatch, which is precisely the check neither existing gate can
   make. calef's own convention already says to cite by number and name; this makes the convention
   mechanical.

## Scope note

The name-matching lint needs a tolerance rule (titles are long, and a citation should be allowed to
quote a distinctive fragment rather than the whole thing), and that rule is the only design work
here. Everything else is a sweep and a grep. Related but distinct: milestone 91 links acronyms to a
glossary, milestone 93 audits claims for rot; this one is about a reference resolving to the thing
its author meant.

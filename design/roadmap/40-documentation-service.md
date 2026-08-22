# 40. Documentation as a system service: searchable, rendered, and installed by packages

**Status: PARTIAL.** Phases 1 and 2 are built; phase 3 is not started and is the only thing left
that this block calls a phase. The status said NOT-STARTED until calef noticed the tree had moved
past it (2026-08-16, the week's fifth §76-shaped misrecording), and **it did not move on 2026-08-18
either**, which is the honest answer for a lane that fixed a renderer, corrected a record and built
a front door without starting the phase that is missing.

**Gate: NONE.** Its one stated prerequisite, milestone 31's phase 2 per-file grants, is built.

What exists: `crates/manual` (a streaming markdown renderer, the byte layout of a search index, and
the query that reads one, all pure and host-tested in `tests/render.rs`), `user/src/doc.rs`, the
viewer program, which the shell can spawn as `Prog::Doc`, the store the image installs, `apropos` at
the nife prompt, and `script/apropos` over the whole repository. The renderer was written against
`line_editor`'s contract rather than `pulldown-cmark`, which the block's own text anticipated as the
alternative and which the crate's header records.

**Phase 2 landed 2026-08-16.** The store is built on the host from the `DOC_BUNDLES` table
(`cargo xtask manual`), installed into the filesystem image as `doc/<bundle>/` with `doc/bundles`
naming what is there, and searched from the prompt by **`apropos <word>`**, a shell builtin. The
whole of it is gated by `script/shell-check` on both architectures: `apropos capability` names the
pages, and the line after it grants `wc` exactly one of them. Search produces **names, never
capabilities**, which is why it may be a builtin: a searching *program* would have to be handed the
whole store to read every shard in it, which is more authority than the answer needs. See
notes/manual.md.

**2026-08-18: the record was the defect three times over, and all three are fixed.**

*In the renderer.* A fenced code block opened inside a block quote **never closed**: the closing
test ran against the raw line, so a quoted closing fence never matched its own opener and every line
to the end of the document rendered as quoted code. It survived because the corpus test cannot see
this class of failure at all (verbatim output loses no characters, and the check is a subsequence),
and because the one symptom it *could* see had been quarantined behind an honest `BUGS` comment
that refused to widen the filter until somebody answered the question. Answering it found the bug.
That refusal is the mechanism working, and it is worth reading before the next one is waved through.

*In the prose.* Three of notes/manual.md's `BUGS` entries described a system that no longer existed:
`doc <page>` was said to deadlock (it is refused, by `grant_plan::check_chain`, with a sentence that
names the fix), `doc <page> | wc` was said to render an empty stream (the head stage's input comes
off the plan now), and `MAX_TEXT_CHUNKS = 32` was said to truncate a page to 512 bytes (it is
`MAX_OUTPUT_CHUNKS = 4096`). Two of the three were closed by other lanes and nobody came back. The
correction is three lines in `script/shell-check` rather than three paragraphs, because this is the
milestone least allowed to describe a system that is not there.

*In the index.* `normalize` folds a query by dropping every non-alphanumeric byte, so a reader who
types `line_editor` looks up `lineeditor`; `tokens` split the *text* on that byte, so the builder
only ever wrote `line` and `editor`, and **the term the query asked for was one no page could ever
have**. In a repository whose prose is full of `snake_case` identifiers that is most of what anyone
would search for: `apropos fs_proto` answered "nothing says that" while dozens of pages said it. The
test that should have caught it asserted the property in its own first comment and then checked a
word with no underscore in it, which is the fence's shape again. Fixed in the writer, so
`apropos editor` keeps working and `apropos line_editor` starts; narrowing the reader instead would
have made it silently search for `line`, which is worse than no answer.

**`script/apropos`, and it is milestone 117's finding rather than a nicety.** Three stranger runs
have measured what a newcomer cannot reach by following this tree: `notes/net.md`,
`notes/capabilities.md`, **any** `design/decisions/` file, and `crates/abi/src/lib.rs`, which is
four syscall numbers and the whole design on one screen. None of them is hidden; nothing a person
would type led to them. This milestone already owned the machinery, pointed at the six pages a guest
can hold, so it is now also pointed at the 512 pages a checkout holds: the same
`manual::index::build` and `manual::index::search`, one shard per part of the tree, and a result
that names a repository path rather than a store location. Crate and program **module headers** are
indexed as pages, which is what makes `crates/abi/src/lib.rs` findable at all: the document a reader
wants about the ABI *is* that file's header, and no markdown page was ever going to fix it.

**What remains, and it is now one fork rather than a list.** There is still **no line a person can
type that renders a page at the prompt**, and the reason is not the shell's scheduling. A process
has one wait point, so a shell that feeds a chain cannot also read from it, and no interleaving
fixes that. What would fix it is somewhere for the viewer's output to go that is **not the shell**,
and the tree already has that thing: `terminal_sink_caretaker`, the terminal's own sink adapter,
where a declared second stream goes by default. Putting it in a tail stage's *output* slot is a
spawn-protocol decision, it is the same decision the pager and the colour bit need, and
notes/pipes.md has been holding it open since milestone 50: *"a shell that wanted a program to print
straight to the screen rather than through its own result endpoint could hand it over, and would
lose the ability to redirect that program at all."* Phase 3, the graphical viewer, waits on the
display ladder as the block says below.

**2026-08-22: the fork is worked through, not just named.** DECISIONS §101 (notification objects),
decided 2026-08-20, ratified the *direction*: the `terminal_sink_caretaker` narrowing is "the right
short-term move" and stays valid as the permanent shape even once the notification object lands,
but it explicitly left milestone 40's own fork undecided: "That is milestone 40's fork, and this
decision does not take it." notes/tail-output-narrowing.md answers CLAUDE.md's six questions
against it: confirms the premise against `grant_plan::check_chain` rather than trusting this
block's own framing, prices the two previously-refused alternatives (a pull-based source, a
buffering stage) against measured numbers already in notes/pipes.md, and finds two things not
previously in the tree. The shell can reuse DECISIONS §26's already-built fault endpoint as its
completion signal instead of reading the child's bytes, and moving output off the shell's own read
loop opens a narrower race than notes/manual.md named, between a child's exit and its own trailing
delivery through the caretaker.

**Decided (§106, 2026-08-22): take it.** Checking `SINK_BIT`'s existing contract
(`crates/grant_plan/src/spawnproto.rs`) found the fork's own write-up had overpriced it: the child's
output slot is already opaque to the program ("the shell delegates an endpoint and init puts it
where the result endpoint would have gone, so the child writes to a pipe or a file sink without
knowing which"), so no program's manifest changes, only shell-and-init default-routing logic. **The
status does not move yet**: this decision authorizes phase 3's caretaker-narrowing increment to a
concrete spec (the narrowing rule, the §26 fault-endpoint reuse, the caretaker-hop race carried as a
known interim per BUGS below); 40 stays PARTIAL until that increment is built.

**In brief.** Markdown authored, **rendered** for display rather than shown raw, searchable locally, and installed by the package that owns it. Reuse `pulldown-cmark` for parsing (CommonMark is a fiddly spec worth taking from someone else) and write the ANSI renderer against `line_editor`'s contract, because `termimad`/`mdcat` sit on `crossterm` and assume a POSIX terminal we do not have. Phase 1 is a terminal viewer and pager, phase 2 a host-built inverted index shipped as a per-package shard, phase 3 a graphical viewer riding the display ladder. Two constraints found while scoping: **`readdir` refuses and the §27 contract has no such verb**, so nothing can walk a tree for documents, and **font rendering is still milestone 29's remaining increment**, so the terminal comes first

**Why it matters.** **the OS explains itself, on itself.** The project's whole argument is already markdown (DECISIONS, thirty-plus notes, this roadmap), so a capability-confined viewer serving them is a better milestone-23 demonstration than another synthetic test and costs the documentation nothing. The missing `readdir` turns out to be a feature: **enumeration is authority**, so indexing at package-build time is both the way around the gap and the more honest shape, which is the same answer `apropos` reached for a different reason. And `doc notes/ipc-naming.md` granting exactly one readable file is milestone 31's designation-is-authorization made into something a person uses

**calef's direction, 2026-07-30.** Markdown as the authored format, rendered for display rather than
shown raw, searchable on the local machine, and installed *by the package that owns it*, so a
component brings its documentation with it.

**Why this belongs on a demonstrator's roadmap rather than being a nicety.** The project's own
argument is written in markdown: `design/decisions/`, thirty-plus notes, this roadmap. A nife that
serves its own design notes, on itself, through a capability-confined viewer, is a better
demonstration of milestone 23's component story than another synthetic test, and it costs the
documentation nothing because it already exists. It is also the first *application* on the display
ladder that anybody would actually use.

## Two constraints found while scoping, both real

1. **There is no directory iteration.** `readdir` refuses in the std PAL and the §27 file contract has
   no such verb, so nothing can walk a tree looking for documents. Adding one is a decision, not a
   detail, and **the capability model argues against it anyway: enumeration is authority.** A viewer
   that can list a directory can discover what it was not given. So the design below indexes at
   *package build time* and ships the index, which sidesteps the missing verb and is the more honest
   shape. Unix reached the same answer for a different reason: `apropos` reads a prebuilt `mandb`
   because scanning was slow.
2. ~~**There is no font rendering yet.**~~ **There is now** (milestone 29, 2026-07-30): a bitmap
   font, a VT engine, and a display terminal that is a compositor client. A *graphical* documentation
   browser is therefore unblocked in principle, though the honest limits still argue for the terminal
   first: an 18x8 grid, no scrollback, and no UTF-8 (notes/glyphs.md).

## Reuse: take the parser, write the renderer

CommonMark is a fiddly specification with a large conformance suite, and parsing it is exactly the
kind of work worth taking from someone else. Rendering to *our* terminal contract is ours and small.
That split is the reuse judgment, and it is the same one milestone 32 made about RedoxFS.

| Piece | Option | Judgment |
|---|---|---|
| Parse | **`pulldown-cmark`** (pure Rust, CommonMark, event-stream API, few dependencies) | **Take it.** The event stream is the right shape for a renderer that emits ANSI. Milestone 27's `std` is what makes this buildable at all. |
| Parse | `comrak` (GFM: tables, strikethrough, footnotes) | Consider later if GFM tables matter; more dependencies. |
| Render | `termimad`, `mdcat` | **Do not take.** Both sit on `crossterm`, which assumes a POSIX terminal (termios, ioctl). Porting that is more work than emitting ANSI against `line_editor`'s contract, which we own and already speak (§21). |
| Search | `tantivy` | **Too heavy.** It assumes a filesystem and mmap. |
| Search | A host-built inverted index shipped in the package | **Take this shape.** Built by `xtask` where there are no constraints, merged by the viewer across installed packages. |
| UI | `ratatui` | Possible for a pager later; needs a backend against our terminal contract first. |

## Shape

- **A doc bundle is part of a package**: rendered-source markdown plus a small index shard, installed
  into a documentation store when the component is installed. This is where milestone 39's packaging
  observation pays: manifest, hash, version, and now a doc bundle.
- ~~**The viewer holds a directory capability to the doc store** and nothing else.~~ **It should
  not, and does not** (phase 1's finding, notes/manual.md): `doc`'s manifest is byte-identical to
  `wc`'s, its cspace holds two endpoints, and the shell is what resolves a page name. A viewer that
  could open the page it renders could open any page. Phase 2 kept that true by making search a
  builtin rather than a program, for the same reason `ls` is one.
- **The index is a merge of shards**, one per installed package, so installing a component makes its
  documentation searchable without a reindex pass and without any component being able to see
  another's files.
- **`apropos <word>`** and `doc <page>`, shell verbs. (Written as `doc search` / `doc view` when
  this was scoped; phase 2 split them because `doc` is a program and a builtin sharing its first
  word would make the parse ambiguous.) Milestone 31's grant expression makes this a demonstration
  rather than a convenience: `doc notes/ipc-naming.md` passes exactly one readable file capability,
  a viewer invoked with no argument can read nothing, and `apropos` in front of it passes nothing at
  all.

## Phasing

- **Phase 1, the terminal viewer.** `pulldown-cmark` to an ANSI renderer over `line_editor`'s contract:
  headings, emphasis, lists, block quotes, code blocks, and a pager. Works on the serial console
  today and inherits the display terminal for free when 29's glyph work lands. Host-tested in
  milliseconds like every other pure-logic piece: markdown in, styled bytes out.
- **Phase 2, search. BUILT** (2026-08-16). The host-built index, the shard merge, and the search
  verb, which is **`apropos <word>`** rather than the `doc search` proposed above: `doc` is a
  program and a builtin sharing its first word would make the parse ambiguous, and `man`/`apropos`
  is the split this design already borrowed everywhere else. The merge is
  `manual::index::Ranked`, a fixed sixteen-result table with no allocator, so a shell with one
  stack page can hold it.
- **Phase 3, the graphical viewer.** Rides the display ladder: needs 29's font rendering and sits as a
  client of 33's compositor. Rung three of the ladder is where this becomes a real application.

**Prior art worth reading:** `man` plus `apropos` plus `mandb` for the split between format, index and
pager, which is the architecture this proposes minus the troff. Dash/Zeal *docsets* (a bundle with its
own index) for the packaging shape. `cargo doc`'s HTML output as the road not taken, since HTML would
need a browser engine, which is a mountain with no thesis behind it.

**Sequencing.** Phase 1 wants milestone 31 phase 2 finished (per-file grants make `doc <file>` the
demonstration it should be) and nothing else; it can precede the packaging work and be wired into it
later. **Effort: 1 lane estimated per phase**, three phases, and they can land separately.

## BUGS

- **The caretaker-hop display race is a known, accepted interim, not fixed by §106.** Once the
  `terminal_sink_caretaker` narrowing ships, a page's last line and the shell's next `$ ` prompt can
  interleave under contention: kernel exit-delivery (DECISIONS §26) tells the shell a child is dead,
  but the caretaker's own trailing `CALL` to `line_editor` is a second, independent call with no
  ordering primitive against the shell's next prompt. A display glitch, not a confinement or
  correctness failure (no capability changes hands, no byte reaches the wrong reader). Tracked at
  milestone 151 (notification objects), DECISIONS §101's kernel build, which lets the shell `WAIT`
  on "the caretaker's queue for this client has drained" instead of racing it. See
  notes/tail-output-narrowing.md for the full finding.

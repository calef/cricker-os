# 40. Documentation as a system service: searchable, rendered, and installed by packages

**Status: NOT-STARTED.**

**Gate: NONE.** Its one stated prerequisite, milestone 31's phase 2 per-file grants, is built, so
phase 1 (the terminal viewer and pager over `pulldown-cmark`) is unblocked. Phases 2 and 3 follow
it inside the block.

**In brief.** Markdown authored, **rendered** for display rather than shown raw, searchable locally, and installed by the package that owns it. Reuse `pulldown-cmark` for parsing (CommonMark is a fiddly spec worth taking from someone else) and write the ANSI renderer against `line_editor`'s contract, because `termimad`/`mdcat` sit on `crossterm` and assume a POSIX terminal we do not have. Phase 1 is a terminal viewer and pager, phase 2 a host-built inverted index shipped as a per-package shard, phase 3 a graphical viewer riding the display ladder. Two constraints found while scoping: **`readdir` refuses and the §27 contract has no such verb**, so nothing can walk a tree for documents, and **font rendering is still milestone 29's remaining increment**, so the terminal comes first

**Why it matters.** **the OS explains itself, on itself.** The project's whole argument is already markdown (DECISIONS, thirty-plus notes, this roadmap), so a capability-confined viewer serving them is a better milestone-23 demonstration than another synthetic test and costs the documentation nothing. The missing `readdir` turns out to be a feature: **enumeration is authority**, so indexing at package-build time is both the way around the gap and the more honest shape, which is the same answer `apropos` reached for a different reason. And `doc notes/ipc-naming.md` granting exactly one readable file is milestone 31's designation-is-authorization made into something a person uses

**calef's direction, 2026-07-30.** Markdown as the authored format, rendered for display rather than
shown raw, searchable on the local machine, and installed *by the package that owns it*, so a
component brings its documentation with it.

**Why this belongs on a demonstrator's roadmap rather than being a nicety.** The project's own
argument is written in markdown: `design/decisions/`, thirty-plus notes, this roadmap. A cricker-os that
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
   first: a 16x8 grid, no scrollback, and no UTF-8 (notes/glyphs.md).

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
- **The viewer holds a directory capability to the doc store** and nothing else. It cannot read the
  rest of the filesystem, which is the point, and it does not need to because the index tells it what
  exists.
- **The index is a merge of shards**, one per installed package, so installing a component makes its
  documentation searchable without a reindex pass and without any component being able to see
  another's files.
- **`doc search <term>`** and **`doc view <topic>`**, shell verbs. Milestone 31's grant expression
  makes this a demonstration rather than a convenience: `doc notes/ipc-naming.md` passes exactly one
  readable file capability, and a viewer invoked with no argument can read nothing.

## Phasing

- **Phase 1, the terminal viewer.** `pulldown-cmark` to an ANSI renderer over `line_editor`'s contract:
  headings, emphasis, lists, block quotes, code blocks, and a pager. Works on the serial console
  today and inherits the display terminal for free when 29's glyph work lands. Host-tested in
  milliseconds like every other pure-logic piece: markdown in, styled bytes out.
- **Phase 2, search.** The host-built index, the shard merge, and `doc search`.
- **Phase 3, the graphical viewer.** Rides the display ladder: needs 29's font rendering and sits as a
  client of 33's compositor. Rung three of the ladder is where this becomes a real application.

**Prior art worth reading:** `man` plus `apropos` plus `mandb` for the split between format, index and
pager, which is the architecture this proposes minus the troff. Dash/Zeal *docsets* (a bundle with its
own index) for the packaging shape. `cargo doc`'s HTML output as the road not taken, since HTML would
need a browser engine, which is a mountain with no thesis behind it.

**Sequencing.** Phase 1 wants milestone 31 phase 2 finished (per-file grants make `doc <file>` the
demonstration it should be) and nothing else; it can precede the packaging work and be wired into it
later. **Effort: 1 lane estimated per phase**, three phases, and they can land separately.

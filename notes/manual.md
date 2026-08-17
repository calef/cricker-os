# The manual: documentation as a system service

*(Milestone 40. Markdown authored, rendered for display rather than shown raw, searchable, and
installed by the package that owns it. The pure logic is `crates/manual`; the program is
`user/src/doc.rs`; the store is built by `cargo xtask manual`. Names are provisional.)*

The project's own argument is written in markdown: 328 files, three megabytes, `design/decisions/`
and a hundred notes. A nife that serves them, on itself, through a viewer that can name
nothing is a better demonstration of the component story than another synthetic test, and it costs
the documentation nothing because it already exists.

Three words carry design weight here, and each turned out to be a capability question rather than a
feature question.

## Rendered

`crates/manual` is a **streaming** renderer: bytes in, styled terminal bytes out, no allocator, no
document held anywhere. That shape is not an optimisation, it is what the program's capabilities
already are. `doc` receives its input as `sink_proto` messages of sixteen bytes each and writes its
output the same way, so a renderer that needed the whole document would need somewhere to put it,
and somewhere to put it is a memory grant the program can otherwise do without. The test
`framing_does_not_matter` pins it: one byte at a time and the whole document at once produce
identical output.

It handles ATX headings, wrapped paragraphs, fenced code, block quotes, nested lists, tables with
computed column widths, thematic breaks, and the inline set (`**strong**`, `*emphasis*`,
`` `code` ``, `~~strike~~`, links and images).

### The roadmap said take `pulldown-cmark`. Two facts overrule it.

**It is not `no_std`.** Version 0.13's `lib.rs` carries no `#![no_std]` and `parse.rs` uses
`std::collections::HashMap`. Taking it means either a permanent fork of somebody else's parser or a
std program.

**And a std program on this system cannot be this program.** The nife PAL's `Stdin::read` returns
`Ok(0)` unconditionally (`patches/std-nife/overlay/std/src/sys/stdio/nife.rs`) and there is no
argv. A std viewer could neither read a keypress to page nor be told which page to show. The
roadmap's premise that "milestone 27's std is what makes this buildable at all" is true of the
parser and false of the program.

What the roadmap could not weigh is the third fact: **the corpus is closed and in-tree.** A renderer
for this repository's own markdown does not need CommonMark conformance; it needs the constructs
these files actually use, and unlike conformance that is checkable directly.
`every_character_survives` does exactly that: every letter and digit of every note, decision and
roadmap page reaches the rendered output, in order. It found three real defects while it was being
written (an escaped pipe read as a column boundary, a table tail dropped when the buffer filled, a
doubled blank line), which is three more than a conformance suite over invented documents would have
found here.

DECISIONS §46 rule 1 then settles it: this is on the verification path, so we write it. The crate is
`no_std`, zero-dependency, allocation-free on the guest path, and reachable by Kani.

### Two rendering decisions worth knowing

**`_` is never emphasis.** CommonMark reads `__rust_alloc` as an opened strong span. This repository
writes `snake_case` identifiers in running prose constantly (`fs_proto`, `line_editor`, `c_seam`),
so honouring the spec here would misrender far more than it would style. Only `*` and `**` open
emphasis, and a closer must not be preceded by a space.

**A code span is consumed before anything else looks at the line.** There are 11,281 of them in the
corpus, many full of the exact characters the other rules hunt for. If that ordering were wrong,
`*ptr` inside backticks would open emphasis and eat the rest of the paragraph.

## Installed

**A doc bundle is a package's pages plus its index shard, installed as a unit.** `doc/<bundle>/` in
the filesystem image, with `doc/bundles` listing the names, built by `cargo xtask manual` from the
`DOC_BUNDLES` table in `xtask/src/main.rs` and imported into the RedoxFS image by `mkredoxfs`.

The table names paths that already exist rather than copying notes into crate directories, and that
is deliberate: **a second copy of a note is a copy that can drift**, and in-tree documentation earns
its keep by there being one. A bundle that lists a page which has moved fails the build.

### What a doc-holding capability designates, which is nothing

The roadmap proposed that "the viewer holds a directory capability to the doc store". It should not,
and does not.

`doc`'s manifest is byte-identical to `wc`'s: `InputSpec::Required`, `OutputSpec::Bytes`, and
`Forbidden` for argument, memory, file and directory. Its cspace holds two endpoints. `doc glob.md`
is the **shell** resolving that name against the directory capability *it* holds and streaming the
bytes in; nothing in the program names a file, a directory or the filesystem, and there is no
message it can send to find out what it is reading.

That matters more here than it did for `wc`, because a documentation viewer is precisely the program
a reader would expect to go and fetch things. A `doc` that opened the page it renders would be a
`doc` that could open any page. `doc glob.md`, `doc < glob.md` and `something | doc` are one
behaviour with three sources, and the program cannot tell them apart.

So there is no ambient authority to arrive by accident, because there is no authority at all. The
concentration is in the shell, where it already was.

## Searchable

**There is no directory iteration in this system**, and that is a feature rather than a gap.
`readdir` refuses in the std PAL and the §27 file contract has no such verb, and the capability
model argues against adding one: *enumeration is authority*, and a viewer that can list a directory
can discover what it was not given. So "what pages exist" is not discoverable at runtime. It is
computed on the host at image time and shipped, which is what Unix's `mandb` does for a different
reason (scanning was slow).

### The layout is designed for a reader that holds one page

A client of the file contract shares exactly one 4 KiB frame with the FS server, and a shell that
had to buffer a whole index would need a memory grant to search. So every section of the index
starts on a page boundary and every record divides 4096 evenly, which together mean **a reader with
one page in hand never sees half a record**. A lookup is then a binary search over *pages*: each
probe reads one page and compares the term it starts with, and the last page is searched in memory
for free.

```text
  page 0            header
  page_off          page records, 128 bytes each, 32 per page
  term_off          term records, 32 bytes each, 128 per page, sorted by term
  post_off          postings, 4 bytes each, 1024 per page
```

### The guest searches with `apropos`, and it is a builtin

Phase 2's other half, and the decision in it is *where the search runs* rather than how.

**A builtin, not a program**, which is exactly the argument `ls` already carries in this shell: a
listing program would have to hold the power to read everything it lists. Search is an enumeration,
so a searching program would have to be handed a capability to the **whole documentation store** in
order to read every shard in it. That is a new principal holding more than the answer needs, for a
command that moves no authority whatsoever. The shell already holds enumeration over what it can
see, so `apropos` is the shell reading a file it could already read.

**What comes back is names, never capabilities.** A result is a store location a person can type:

```text
$ apropos capability
    32  doc/swish/pipes.md            Pipes and redirection: `>`, `<` and `|` are one
    11  doc/kernel/ipc-naming.md      Who does IPC name?
$ wc doc/kernel/ipc-naming.md
  163 1556 9503
```

(Those three numbers move whenever that note is edited. What does not move is that the name the
search printed resolved, which is the claim.)

The second line is where a capability moves, and it moves because a person typed a name. So search
cannot widen what its caller could already reach, and `doc notes/ipc-naming.md` granting exactly one
readable file survives having a search in front of it. A search *program* would have moved the
authority one line earlier and silently.

The split follows the tree's usual one. The **reading** is `manual::index::search`, and it is the
single point at which the writer and the reader are proved to agree: `cargo xtask manual capability`
on the host and `apropos capability` at the prompt call that same function, over the same bytes,
through the same one-page-at-a-time `Pages`. The **rendering** is `swish::write_apropos`, host-tested
with the rest of what the prompt says. What is left in `user/src/swish.rs` is four filesystem
requests and a 4 KiB page buffer.

### The store's own layout is a thing two programs agree on

`doc/bundles` lists what is installed, one name per line; `doc/<bundle>/index` is a shard;
`doc/<bundle>/<page>.md` are the pages. Those three names are `manual::index::STORE_DIR`,
`MANIFEST` and `SHARD`, in the crate both sides depend on, because the host writes them and the
guest opens them (AGENTS.md rule 7). The manifest is a **file rather than a directory listing**,
which is this whole milestone in one constant.

### What it costs, measured

`cargo xtask manual` over the current bundles:

| bundle | pages | terms | postings | markdown | index | probes |
|---|---|---|---|---|---|---|
| `manual` | 1 | 872 | 872 | 20625 | 40960 | 4 |
| `swish` | 2 | 1730 | 2046 | 88088 | 73728 | 5 |
| `kernel` | 2 | 1829 | 2115 | 63639 | 81920 | 5 |
| `glob` | 1 | 861 | 861 | 20019 | 40960 | 4 |

**192,371 bytes of markdown produce 237,568 bytes of index**, which is 1.24x, and that is the number
worth arguing with rather than the pleasant ones. (It was 1.56x when phase 1 measured it, and the
improvement is not an optimisation: the notes it indexes grew, and page alignment's fixed floor is a
smaller share of a bigger bundle.) Two things pay for it. A term record stores its
term **inline** in 24 bytes so a probe is one page read rather than two, which is most of the bulk.
And page alignment puts a four-page floor (16 KiB) under every bundle however small, so a bundle of
one short page still costs 16 KiB to index.

The `manual` row indexes **this page**, so editing it moves its own numbers. Rerun
`cargo xtask manual` for current ones; the ratio is what is stable.

The number that justifies the layout is the last column: **a lookup is at most five page reads**,
20 KiB of IO, with no allocation and a 4 KiB working set.

## EXAMPLES

Build the store and search it from the host, with the same reader the guest runs:

```text
$ cargo xtask manual capability
documentation store: target/redoxfs-tree/doc

  bundle     pages   terms postings  markdown    index probes
  manual         1     872      872     20625    40960      4
  swish          2    1730     2046     88088    73728      5
  kernel         2    1829     2115     63639    81920      5
  glob           1     861      861     20019    40960      4

  192371 bytes of markdown, 237568 bytes of index

search: capability
    32  doc/swish/pipes.md            Pipes and redirection: `>`, `<` and `|` are one   notes/pipes.md
    15  doc/manual/manual.md          The manual: documentation as a system service   notes/manual.md
    11  doc/kernel/ipc-naming.md      Who does IPC name?                              notes/ipc-naming.md
     8  doc/glob/glob.md              The glob matcher                                notes/glob.md
     3  doc/swish/line-discipline.md  The line discipline as a userspace component    notes/line-discipline.md
```

The host prints a fourth column the prompt does not: the page's path in the **source tree**, which is
provenance rather than something to open. The store location beside it is computed by the searcher
from the shard it opened, so no byte of the index carries it and the two cannot disagree.

Search the same store from the prompt, where the answer is what a person acts on:

```text
$ apropos capability
    32  doc/swish/pipes.md            Pipes and redirection: `>`, `<` and `|` are one
    11  doc/kernel/ipc-naming.md      Who does IPC name?
     8  doc/manual/manual.md          The manual: documentation as a system service
     8  doc/glob/glob.md              The glob matcher
     3  doc/swish/line-discipline.md  The line discipline as a userspace component
$ caps wc doc/kernel/ipc-naming.md
  wc would grant the new process, and nothing else:
    cap 0  endpoint  result   report its answer back
    output   this shell's result endpoint (it reads the bytes and prints them)
    input    ipc-naming.md  (this shell reads it and streams it in; the program
             holds an endpoint, not a file)
    arg    (none)
  reading the command is reading its whole authority.
```

Those two lines together are the milestone: the first names pages and grants nothing, and the second
grants exactly the one page a person named out of what the first said. `apropos` itself has no
`caps` preview to print, because there is nothing to preview.

Render a page at the prompt, and prove the viewer is an ordinary pipeline stage rather than
something that reached for a file:

```text
$ echo # The terminal contract | doc
THE TERMINAL CONTRACT
$ doc gate.md | wc
1 3 22
$ doc
doc: reads an input stream: name a file, redirect with '<', or pipe into it
```

## BUGS

- **`doc <page>` on its own deadlocks at the interactive prompt.** Found by running it, and it is a
  property of the shell rather than of the viewer. `swish` sends a spawned stage its **whole** input
  and only then drains that stage's output, and `sink_proto` is a rendezvous `SEND`, so a program
  that writes while it is still reading blocks against a shell that is still writing. `wc` never
  meets this because it produces nothing until end of stream; a renderer cannot do that without
  holding the whole document, which is exactly the memory grant this design exists without. So `doc
  page.md | wc` and `doc page.md > out.txt` work and `doc page.md` hangs.

  `MAX_TEXT_CHUNKS = 32` in `user/src/swish.rs` is the second half of it: even with no deadlock the
  prompt would print only the first 512 bytes of a rendered page. **A shell that can show a document
  is a lane of its own**, and it is a scheduling change (drain while writing) rather than a
  capability one.
- **Neither workaround for that deadlock delivers the file.** `doc gate.txt | wc` and
  `doc gate.txt > page.txt` both run and both answer `0 0 0`, which is the viewer rendering an
  *empty* input: the named file reaches a stage only on the plain `doc gate.txt` path, the one that
  deadlocks. So there is currently **no line a person can type that shows a rendered page**, and the
  boot gate asserts only what is true: `doc` is in the image, is spawnable, and is refused at the
  prompt when given no stream.

  All three of these are the shell, not the viewer, and they are one lane: teach `swish` to drain a
  stage while it is still writing to it, raise `MAX_TEXT_CHUNKS`, and carry the file source into a
  pipeline and a redirection. The renderer underneath is proven on 100+ real pages by
  `every_character_survives` and does not change.
- **No pager, and the reason is authority rather than effort.** Paging needs a keypress; a keypress
  needs `line_editor::proto::OP_READLINE`; and that opcode rides on the terminal endpoint whose read
  side *is* the keyboard. The spawn protocol has no way to hand a child the right to read one line
  without handing it the terminal, which is the exact thing `terminal_sink_caretaker` exists to
  prevent. So a long page scrolls off. The fix is a decision about the spawn protocol, and it is the
  most interesting thing this milestone found.
- **`doc` emits plain text even at a terminal.** The renderer can colour, and the shell has no way
  to tell a stage "you end at the terminal", for the same reason the sink contract is a good
  contract: a writer cannot tell what is underneath it. Unix answers this with `isatty`, which is a
  sniff; the honest answer here is a wiring bit the spawn protocol does not carry yet.
- **A search keeps sixteen results and counts the rest**, saying "16 of 43 pages, strongest first"
  when it dropped any. Sixteen is the reader's number rather than the index's: a search answer is
  read at a prompt before deciding what to open, and one of this system's two terminals is sixteen
  rows tall. A term nearly every page mentions therefore answers with the sixteen that mention it
  most, which for `capability` is a fair answer and for `the` would not be.
- **Ranking is occurrence count and nothing else.** A long page that mentions a word in passing can
  outrank a short page about it. Dividing by document length would be one division and needs the
  page's length, which the layout does not store.
- **A search answer is up to 86 columns wide** when a long location meets a long title, so it wraps
  on the 80-column serial console and wraps hard on the 32-column graphical one. The location is
  never truncated to fit, deliberately: it is the name the reader is meant to type, and a wrapped
  line beats an unusable one.
- **A negative example cannot be written into this page.** This note is in the `manual` bundle, so
  every word here is a word the store then says, and writing `apropos <nonsense>` with its answer
  would make that answer wrong on the next build. The boot gate holds the negative control instead,
  with a word chosen to appear in no bundled page. That is funny and it is also the honest shape of
  a system that documents itself: the documentation is data.
- **A shard whose version is not the reader's is refused, not migrated.** Every shard in the tree is
  a build artifact regenerated by `cargo xtask manual`, so the format and its reader ship together
  and there is nothing yet to migrate. The day a shard arrives from somewhere the build did not
  produce, that is the decision to revisit.
- **`apropos` searches from the root of what the shell holds, not from the cwd.** The store is
  installed at that root and a `cd` does not move the manual. A shell granted a *subtree* that does
  not contain `doc/` therefore cannot search at all, and says so with the filesystem's own errno.
- **The index is 1.25x the markdown it indexes**, per the table above.
- **A source line longer than `manual::LINE_MAX` (2048) loses its tail.** The longest line in this
  repository is 1835 bytes, so the corpus fits; a document from elsewhere may not, and
  `Renderer::truncated` reports it while `doc` does not print it.
- **Table cells are truncated to their column width**, so a wide table on an 80-column terminal
  loses text. This is a formatting choice, not a parsing failure, and the corpus test runs at 4000
  columns to keep the two apart. A table too large for the renderer's buffers spills into a second
  aligned chunk rather than losing rows; this repository's largest table is 117 rows.
- **Setext headings and reference links are not recognised**, and no HTML is interpreted. There is
  one reference link in the corpus and no setext heading; `---` on its own line is a thematic break
  here 64 times, so reading it as a heading would misrender all of them to catch none.

## Where this goes next

The guest-side `apropos` that used to head this list is built (phase 2, above), and it went to the
builtin the entry predicted. What is left, in the order it pays off:

1. **A shell that can show a document.** The three limitations at the top of `BUGS` are one lane and
   they are the reason there is still no line a person can type that renders a page: teach `swish` to
   drain a stage while it is still writing to it, raise `MAX_TEXT_CHUNKS`, and carry the file source
   into a pipeline and a redirection. This is now the milestone's biggest gap, because `apropos`
   hands a reader a page name and the next thing they type does not work.
2. **A wiring bit in the spawn protocol** that says "this stage's output ends at the terminal", which
   turns colour on and is the honest replacement for `isatty`.
3. **The pager**, which is the same protocol decision seen from the other side: what it takes to
   grant a child one line of input without granting it the keyboard.
4. **The store as something a package installs**, rather than a table in `xtask`. `DOC_BUNDLES` is
   the shape milestone 40 asked for minus a package manager, and milestone 39 is where the manifest,
   the hash and the version it should hang off already live.

Phase 3 of the roadmap (a graphical viewer as a compositor client) is untouched and still wants
milestone 33's rungs.

## Prior art

`man` plus `apropos` plus `mandb` for the split between format, index and pager, which is this
architecture minus the troff. Dash and Zeal *docsets* for the bundle shape. `cargo doc`'s HTML as
the road not taken: it would need a browser engine, which is a mountain with no thesis behind it.

# The manual: documentation as a system service

*(Milestone 40. Markdown authored, rendered for display rather than shown raw, searchable, and
installed by the package that owns it. The pure logic is `crates/manual`; the program is
`user/src/doc.rs`; the store is built by `cargo xtask manual`. Names are provisional.)*

The project's own argument is written in markdown: 328 files, three megabytes, `design/decisions/`
and a hundred notes. A cricker-os that serves them, on itself, through a viewer that can name
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

**And a std program on this system cannot be this program.** The cricker PAL's `Stdin::read` returns
`Ok(0)` unconditionally (`patches/std-cricker/overlay/std/src/sys/stdio/cricker.rs`) and there is no
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

### What it costs, measured

`cargo xtask manual` over the current bundles:

| bundle | pages | terms | postings | markdown | index | probes |
|---|---|---|---|---|---|---|
| `manual` | 1 | 3 | 3 | 26 | 16384 | 2 |
| `swish` | 2 | 1596 | 1900 | 73187 | 69632 | 5 |
| `kernel` | 2 | 958 | 1137 | 20764 | 49152 | 4 |
| `glob` | 1 | 845 | 845 | 19712 | 40960 | 4 |

**113,689 bytes of markdown produce 176,128 bytes of index**, which is 1.55x, and that is the number
worth arguing with rather than the pleasant ones. Two things pay for it. A term record stores its
term **inline** in 24 bytes so a probe is one page read rather than two, which is most of the bulk.
And page alignment puts a four-page floor (16 KiB) under every bundle however small, which is why
the one-page `manual` bundle costs 16 KiB to index 26 bytes.

The number that justifies the layout is the last column: **a lookup is at most five page reads**,
20 KiB of IO, with no allocation and a 4 KiB working set.

## EXAMPLES

Build the store and search it from the host, with the same reader the guest runs:

```text
$ cargo xtask manual capability
documentation store: target/redoxfs-tree/doc

  bundle     pages   terms postings  markdown    index probes
  manual         1       3        3        26    16384      2
  swish          2    1596     1900     73187    69632      5
  kernel         2     958     1137     20764    49152      4
  glob           1     845      845     19712    40960      4

  113689 bytes of markdown, 176128 bytes of index

search: capability
    31  Pipes and redirection: `>`, `<` and `|` are one   notes/pipes.md
     3  The line discipline as a userspace component  notes/line-discipline.md
    11  Who does IPC name?        notes/ipc-naming.md
     8  The glob matcher          notes/glob.md
```

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
- **The guest cannot search yet.** The index is built, installed and queried by the same `no_std`
  reader the guest would run, but only from the host: binding it to `fs_proto` needs a program or a
  builtin that holds the store's directory, and enumeration being authority is exactly why that is a
  decision rather than a detail. `xtask manual` is the proof that the reader and the writer agree;
  the IO is one lane's work.
- **The index is 1.55x the markdown it indexes**, per the table above.
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

Three lanes, in the order they pay off.

1. **A guest-side `apropos`**, holding the store's directory and speaking `fs_proto`. The reader is
   written and proven; this is the IO and a decision about whether it is a program or a builtin.
   Search *is* enumeration, and the shell already holds enumeration over what it can see, which is
   the argument for a builtin.
2. **A wiring bit in the spawn protocol** that says "this stage's output ends at the terminal", which
   turns colour on and is the honest replacement for `isatty`.
3. **The pager**, which is the same protocol decision seen from the other side: what it takes to
   grant a child one line of input without granting it the keyboard.

Phase 3 of the roadmap (a graphical viewer as a compositor client) is untouched and still wants
milestone 33's rungs.

## Prior art

`man` plus `apropos` plus `mandb` for the split between format, index and pager, which is this
architecture minus the troff. Dash and Zeal *docsets* for the bundle shape. `cargo doc`'s HTML as
the road not taken: it would need a browser engine, which is a mountain with no thesis behind it.

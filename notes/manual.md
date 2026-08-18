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

### The bug the corpus test could not see, and the honest filter that kept it findable

**A fenced code block opened inside a block quote never closed**, from phase 1 until 2026-08-18. The
closing test ran against the raw line, so this:

> A transcript quoted from somewhere else, with its own fence inside the quote:
>
> ```text
> $ doc glob.md | wc
>   1 4 26
> ```
>
> and prose after it, which used to render as code.

left the renderer in code mode to the end of the document. One quoted transcript misrendered every
line after it.

**`every_character_survives` could not find it**, and the reason is worth keeping: verbatim output
loses no characters, so a page rendered entirely as code still passes a subsequence check. What the
test *did* see was the opening fence's info string going missing, because its filter excluded a
fence by looking for backticks after `trim_start` and a quoted fence does not start with one. That
is the renderer being **right** about one line inside a page it was then ruining.

The filter was left wrong on purpose, with a `BUGS` comment saying why: widening it would have
silenced one false failure by hiding three hundred misrendered lines, and nobody had answered
whether the renderer kept its quote state across a nested fence. It did not. So the answer came
first and the filter second, which is the order that entry existed to enforce.

**And the corpus test still cannot guard it**, which was measured rather than assumed. Reverting the
fix leaves this very page ruined from the block above onward, and `every_character_survives` passes:
verbatim output loses no characters, and `Renderer::unclosed_fence` (added here, and the strongest
thing the corpus check can assert) misses it too, because a bare closing fence three sections later
matches the stuck one and lets the renderer out. A unit test is the guard. The lesson generalises
past this bug: **a subsequence check proves nothing was dropped and nothing about what was ruined**,
so a renderer wants both kinds of test and this one had only the first.

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

### The writer and the reader must agree on what a word is, and did not

`manual::index::normalize` folds a query by dropping every byte that is not a letter or a digit, so
a reader who types `line_editor` looks up `lineeditor`. `manual::index::tokens` split the *text* on
that same byte, so the builder only ever wrote `line` and `editor`. **The term the query asks for
was one no page could ever have.** In a repository whose prose is full of `snake_case` identifiers,
that is most of what anybody would search for: `apropos fs_proto` and `apropos grant_plan` both
answered "nothing says that" while dozens of pages said exactly that.

The test that should have caught it asserted the property in its own first comment, *"the builder's
tokeniser and the reader's query normaliser must agree, byte for byte, on what a term is"*, and
then checked a word with no underscore in it. Same shape as the fence: a claim in prose, a weaker
thing checked.

The fix is in the writer rather than the reader, and that is the choice worth recording.
`line_editor` now yields three terms, `line`, `editor` **and** `lineeditor`, so `apropos editor`
keeps working and `apropos line_editor` starts. Narrowing `normalize` to stop at the underscore
would have been smaller and would have made `apropos line_editor` silently search for `line`, which
is a worse answer than no answer.

**Only the underscore joins.** It is unambiguous in this tree and the hyphen is not: joining across
`-` would manufacture a term out of every hyphenated phrase in the corpus, and `notes/glob-grant.md`
is a filename rather than a word. The renderer narrowed emphasis on exactly this reasoning and
records it in its own `BUGS`.

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

### And the same index, pointed at the repository

`script/apropos <word>` is the guest's builtin with a checkout underneath it instead of a
filesystem image. **It is milestone 117's finding rather than a convenience.** Three stranger runs
have measured what a newcomer cannot reach by following this tree while doing ordinary work, and it
is a list rather than an impression: `notes/net.md`, `notes/capabilities.md`, **any**
`design/decisions/` file, and `crates/abi/src/lib.rs`, which is four syscall numbers and the whole
design on one screen. None of them is hidden. Nothing a person would type led to them.

```text
$ script/apropos capability
512 pages, 5049711 bytes of documentation in this repository

searching for: capability

     46  notes/capabilities.md                             Capabilities, and why the kernel has no `open()`
     43  notes/README.md                                   Concept notes
     37  design/roadmap/47-navigation-and-naming.md        47. Navigation and naming: `cd`, `pwd`, `ls`, `m
     33  notes/std.md                                      Rust `std` on the native ABI
     32  notes/pipes.md                                    Pipes and redirection: `>`, `<` and `|` are one

  16 of 302 pages, strongest first
```

Every number in that block moves whenever anything in the tree is edited, this page included, and
that is the same property the store's own table has: **the documentation is the data.** What does
not move is which page came first.

Three things about it are deliberate.

**It is the same code**, `manual::index::build` and `manual::index::search`, one shard per part of
the tree and the same merge across shards the shell does with one 4 KiB page. Not a second
implementation, so a defect in the layout shows up in both places and a fix lands in both. What
differs is what a result *names*: a guest result names `doc/<bundle>/<page>`, because that is what a
shell there can designate, and this one names a path in this repository, because that is what a
person with a checkout opens. Both come out of the same `Found`; the store location and the origin
are two fields it already carried.

**Crate and program module headers are pages.** That is the half that makes `crates/abi/src/lib.rs`
findable at all, and no markdown page was ever going to do it: the document a reader wants about the
ABI *is* that file's header. A `//!` block is markdown already, so it indexes with no conversion and
no copy, and the result names the source file, which is the thing to open. A header shorter than a
paragraph is skipped, because indexing it would put noise in front of the pages that answer.

**There is no cache**, and a run is about a second over five megabytes. A cache that can be stale is
worse here than a second of work, for the same reason `script/catch-up` and `script/names` are
derived views: a maintained one rots and nothing says so.

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
| `manual` | 1 | 1119 | 1119 | 29690 | 53248 | 5 |
| `swish` | 2 | 1807 | 2130 | 88772 | 81920 | 5 |
| `kernel` | 3 | 2461 | 3369 | 101022 | 106496 | 6 |
| `glob` | 1 | 882 | 882 | 20019 | 40960 | 4 |

**239,503 bytes of markdown produce 282,624 bytes of index**, which is 1.18x, and that is the number
worth arguing with rather than the pleasant ones. (It was 1.56x when phase 1 measured it and 1.24x
in the middle, and the improvement is not an optimisation: the notes it indexes grew, and page
alignment's fixed floor is a smaller share of a bigger bundle. It went the other way on 2026-08-18,
from 1.14x, and that one *is* a cost: underscore-joined terms are a third term for every
`snake_case` identifier in the prose, which is what makes those identifiers findable.) Two things pay for it. A term record stores its
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
  manual         1    1119     1119     29690    53248      5
  swish          2    1807     2130     88772    81920      5
  kernel         3    2461     3369    101022   106496      6
  glob           1     882      882     20019    40960      4

  239503 bytes of markdown, 282624 bytes of index

search: capability
    46  doc/kernel/capabilities.md    Capabilities, and why the kernel has no `open()`  notes/capabilities.md
    32  doc/swish/pipes.md            Pipes and redirection: `>`, `<` and `|` are one   notes/pipes.md
    14  doc/manual/manual.md          The manual: documentation as a system service   notes/manual.md
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
    46  doc/kernel/capabilities.md    Capabilities, and why the kernel has no `open()`
    32  doc/swish/pipes.md            Pipes and redirection: `>`, `<` and `|` are one
    14  doc/manual/manual.md          The manual: documentation as a system service
    11  doc/kernel/ipc-naming.md      Who does IPC name?
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

- **`doc <page>` on its own is refused, and the refusal names the fix.** It used to deadlock; that
  was true when phase 1 shipped and stopped being true when milestone 50's chain check landed, and
  this entry said it anyway until 2026-08-18. What happens now is
  `grant_plan::check_chain` answering before anything is spawned:

  ```text
  $ doc glob.md
  refused
    doc: writes while it reads, and this shell can only wait on one thing at a time: give it a
    reader that is not this shell, as in '| wc'
  ```

  The constraint underneath is the kernel's rather than the shell's, and it is worth reading
  before reaching for a scheduling fix. A process has **one wait point**: `SEND` blocks until a
  receiver takes the message, `RECV` blocks until one arrives, and there is no select and no timed
  wait. So a shell feeding a chain cannot also be receiving from it, and **no interleaving schedule
  fixes it**: alternating one send with one receive deadlocks whenever the stage reads twice before
  it writes, and the other way round deadlocks whenever it writes twice before it reads. The shell
  cannot know which, because the whole point of the sink contract is that neither end knows anything
  about the other.

  So the fix is not "drain while writing", which this note used to prescribe. It is **somewhere for
  the viewer's output to go that is not this shell**, and the tree already has that thing:
  `terminal_sink_caretaker`, the terminal's own sink adapter, which is where a declared second
  stream goes by default with no `2>` on the line. Handing it a tail stage's *output* slot is a
  spawn-protocol decision rather than a shell change, and notes/pipes.md has been carrying it as an
  open question since milestone 50: *"a shell that wanted a program to print straight to the screen
  rather than through its own result endpoint could hand it over, and would lose the ability to
  redirect that program at all."* That trade is the milestone's remaining fork. See
  **Where this goes next**.
- **`doc <page> | wc` and `doc <page> > out.txt` deliver the file now.** Both
  answered `0 0 0` when phase 1 measured them, because a pipeline's head was wired off the `Line`,
  which carries no `<`, so the planned input operand was dropped and the stage counted an empty
  stream. That was fixed in `user/src/swish.rs` (the head's input comes off the plan now) and the
  fix is pinned at the real prompt on both architectures:

  ```text
  $ wc gate.txt
    2 4 24
  $ doc gate.txt | wc
    1 4 26
  ```

  Two source lines are one paragraph re-flowed to one output line, and the two extra bytes are the
  body indent, so the counts are what separate a rendered page from silence. `MAX_TEXT_CHUNKS = 32`
  was the other half of this entry and is also gone: the shell drains `MAX_OUTPUT_CHUNKS = 4096`
  messages, which is 64 KiB rather than 512 bytes.
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
- **A hyphenated name is not one term, and a path is not searchable.** Only the underscore joins,
  so `apropos line-discipline` searches `line` and then `discipline` separately (as two queries;
  this verb takes one word), and `apropos notes/glob.md` folds to `notesglobmd`, which no page
  says. A search is for words, and the location a result prints is what you hand to `doc`.
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
- **The shell reads at most 256 bytes of the bundle manifest**, which is ten times what the shipped
  store needs and is a quarter of a page in a program with one stack page. A manifest that fills the
  buffer says so and searches what it read, because a silent "no page says that" about bundles
  nobody looked in is the one failure a search must not have.
- **`apropos` searches from the root of what the shell holds, not from the cwd.** The store is
  installed at that root and a `cd` does not move the manual. A shell granted a *subtree* that does
  not contain `doc/` therefore cannot search at all, and says so with the filesystem's own errno.
- **The index is 1.18x the markdown it indexes**, per the table above, and it was 1.56x when
  phase 1 measured it. The floor is what moves it: page alignment costs every bundle 16 KiB
  however small, so the ratio improves as the bundles grow rather than because anything got better.
- **A source line longer than `manual::LINE_MAX` (2048) loses its tail.** The longest line in this
  repository is 1896 bytes <!--count:longest-markdown-line-->, so the corpus fits; a document from
  elsewhere may not, and `Renderer::truncated` reports it while `doc` does not print it. The number
  carries a marker because it drifted: these three places said 1835 for as long as the two gated
  ones said 1841, which is the margin this milestone is measured against going stale in the prose
  that explains it.
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

1. **One spawn-protocol decision, and it is three of this list's old entries at once.** Two of them
   have been closed by other lanes since this list was written (the file does reach a pipeline's
   head, and the prompt does not truncate at 512 bytes), and what is left is not a shell change.
   There is still no line a person can type that renders a page, because the shell would be both
   the writer and the reader of it and has one wait point. The fix is **somewhere for a tail
   stage's output to go that is not the shell**, and `terminal_sink_caretaker` is already that
   thing for a declared second stream: init endows it from the manifest, the bytes reach the screen
   without passing through the shell, and a child holding it is the *default* for `2>` today, so
   the authority increment for a program that already declares diagnostics is zero.

   The same bit turns colour on, which is the honest replacement for `isatty`, and the pager is the
   same decision seen from the other side: what it takes to grant a child one line of *input*
   without granting it the keyboard. notes/pipes.md has been holding the question open since
   milestone 50 and states the cost plainly: *"a shell that wanted a program to print straight to
   the screen rather than through its own result endpoint could hand it over, and would lose the
   ability to redirect that program at all."* The obvious narrowing is that the shell hands it over
   **only** on a line with no `>` and no `|`, which it can decide from the plan before it spawns
   anything, so nothing loses its redirection. That narrowing is a proposal and not a decision:
   what a spawned program holds at this prompt is calef's, and it is the syscall-adjacent kind of
   choice the *move fast on what can be undone* tenet puts on the expensive side.

   **And there is a cost that is not the one notes/pipes.md names**, which is the part worth
   deciding rather than discovering. A child holding the terminal's sink can write to the screen
   *after* its line has ended, so a stage that outlives its command can paint a prompt nobody
   typed. Today that cannot happen through this path because only a declared second stream reaches
   it and the shell drains the tail before printing again; a tail stage whose output goes straight
   to the screen gives the shell nothing to wait on, so it would have to wait for the child to
   exit instead. That is a second question inside the first one, and it is a security question
   rather than a plumbing one.
2. **The store as something a package installs**, rather than a table in `xtask`. `DOC_BUNDLES` is
   the shape milestone 40 asked for minus a package manager, and milestone 39 is where the manifest,
   the hash and the version it should hang off already live.
3. **Ranking that divides by length.** `script/apropos` made this matter: over six pages occurrence
   count is fine, and over 512 a long page that mentions a word in passing can outrank a short page
   about it. A page record is 128 bytes and holds 122, so a token count fits in the spare six with
   no format growth at all. Not taken here because it changes what the guest answers and this lane
   was already correcting a record rather than writing one.

Phase 3 of the roadmap (a graphical viewer as a compositor client) is untouched and still wants
milestone 33's rungs.

## Prior art

`man` plus `apropos` plus `mandb` for the split between format, index and pager, which is this
architecture minus the troff. Dash and Zeal *docsets* for the bundle shape. `cargo doc`'s HTML as
the road not taken: it would need a browser engine, which is a mountain with no thesis behind it.

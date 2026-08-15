# 121. `ripgrep` on nife: enumeration as a capability, and what the walk costs

**Status: NOT-STARTED.** Minted 2026-08-13 by calef. A third "somebody else's real application"
target beside milestone 66's Vaultwarden and milestone 99's git, chosen for a reason neither of those
has: it is the workload that pushes on **enumeration**, which is the one authority this system treats
as dangerous.

**Gate: MILESTONE 64, MILESTONE 122.** 64 measured the crates.io surface and bound `read_dir`; 122
gives `std` a directory handle it can hold, without which a walker cannot open what it lists. Every
dependency this milestone needs was already measured as building, so the gate is about the platform
rather than about the crates.

## Why this workload rather than another

**Enumeration is authority, and nothing in the tree currently walks.** Milestone 40 met this a week
ago and wrote it down: its search index is built on the host because there is no `readdir` for a
viewer to call, and adding one "would hand a viewer the power to discover what it was not given". A
program that can list a directory can learn what exists, which is a different and larger power than
being able to read a file it was handed.

That is not a wall, it is a right. **`ENUMERATE` is one of §47's six directory rights**, it is
implemented in `crates/fs_proto`, and its absence answers `EPERM` rather than an empty listing, which
is milestone 42's truthfulness rule applied to the one refusal most easily faked. `rm -r` already
composes it (`REMOVE_TREE = ENUMERATE | DESCEND | REMOVE`).

So the machinery exists and one workload uses it, to **delete**. Nothing uses it to **walk**, and
walking is where the interesting costs and the interesting confinement both live.

## What is already in place, measured rather than assumed

| Piece | State | Where |
|---|---|---|
| the `ENUMERATE` right, and `EPERM` when withheld | built | §47, `crates/fs_proto` |
| rights that cannot be widened by a child | built | §47 |
| `read_dir` bound in the `std` PAL, **one level only** | built, see the correction below | milestone 64 |
| `regex`, `walkdir`, `ignore`, `crossbeam-channel`, `memchr` | **built with no change** | milestone 64's probe of 50 crates |

That last row is the surprising one. Every significant dependency in ripgrep's tree is in 64's
**"Built with no change (35)"** list, including `ignore`, which is the crate that does the gitignore
parsing and the directory walking and which 64's table annotates "fs plus threads". So this milestone
is closer to a port than to a construction, which is what §46's boundary predicts: the reuse boundary
is the TCB boundary, and userspace should actively prefer porting.

### Correction (2026-08-13): a recursive walk does not work today, and that is the milestone

The paragraph above was written believing that `read_dir` being bound made the walk nearly free. It
does not, and the first draft of this block was wrong about the central thing it proposes to build.

**The `std` PAL grants one name, not a path.** `one_name` in `patches/std-nife` refuses absolute
paths, `..`, **and nested paths**. The overlay is careful about the flat case: `read_dir(".")` yields
`./name`, `one_name` accepts it, and feeding an entry's `path()` straight back to `File::open` works
exactly as a caller expects.

Descend one level and it stops. `read_dir("./sub")` lists, because `./sub` reduces to one component.
Its entries' `path()` is `./sub/name`, which is two, which is refused. **A program can list a
subdirectory and cannot open what it finds there.**

So `walkdir` and `ignore` build and cannot walk, and "built with no change" turns out to mean
compiles rather than works. The distance between those two is this milestone.

**What that makes the real work, and a second correction.** An earlier draft of this paragraph said
the descent model was unbuilt. It is not. `fs_proto` has **`OPENDIR`** (op 8), which resolves one name
under the directory handle in `req_handle`, requires `DESCEND` on the parent, and attenuates the
child's rights so that no descendant can exceed its ancestor. `rm`, `swish`, both `fs_*_caretaker`
programs and the FS server use it today, so §47's descent is built and exercised by native programs.

The gap is one layer up. **The `std` PAL calls `OPENDIR` only inside `read_dir`, against
`proto::ROOT`, and does not retain the handle**, so a `std` program has no directory object to
descend into and `one_name` has nothing to resolve a second component against. The contract is fine;
the binding is missing.

That work is milestone 122, and this milestone is gated on it.

This correction is why the benchmark below is worth more than the port: the number prices a design
that does not exist yet, rather than confirming one that does.

## The demonstration

`rg pattern src/`, where the process holds a directory capability over `src/` carrying
`ENUMERATE | READ | DESCEND` and **cannot see, name, or read anything outside it.** Not by policy and
not by a check the program performs on itself, but because the authority it holds does not reach.
Milestone 48's shell already rebinds what it holds, so the grant is the shell's ordinary act rather
than a special case built for this.

The negative half is the load-bearing one and should be a test, in the shape milestone 108 used: the
same command, run against a directory capability that lacks `ENUMERATE`, must be **refused loudly**
rather than returning nothing. A search that silently finds no matches because it could not look is
the worst possible failure for this tool, and `fs_proto` already chose `EPERM` over an empty listing
for exactly that reason.

This is a better first demonstration than git for one reason worth stating plainly: **everybody has
run grep.** The confinement claim needs no explanation to anyone who has ever typed a search.

## The benchmark, which is the part worth the lane

**Every `read_dir` is IPC to the filesystem server.** A recursive walk is therefore the workload that
most directly prices this system's central architectural bet, and the roadmap does not currently have
that number. ripgrep is a good instrument for it because it is famous for speed, its performance is
publicly compared against other tools, and the comparison against Linux runs the same Rust logic on
both sides rather than two different programs.

What to measure: wall time and instruction count over a fixed tree, and the per-entry cost of the
walk separated from the per-byte cost of the search, because those two exercise different halves of
the system and a single number hides which one is expensive.

**The caveat that makes it honest, and it is not small.** This kernel has **no threads sharing an
address space** (§43), so a process is single-threaded, and `ignore`'s parallel walker and
`crossbeam-channel` will build but cannot be used as designed. ripgrep must run with one thread here,
so the Linux side must be `rg --threads 1` or the comparison is a lie. State it next to the number,
the way the map "tie" and the spawn caveat are stated.

An honest loss is a result. §14's framing is that a recorded loss is worth more than an overclaimed
win, and a microkernel paying IPC per directory entry may well lose this one. That is worth knowing
precisely, and it is the input to whether the FS contract wants a batched or streaming listing.

## Prior art

The three questions, against `notes/prior-art.md`'s ecosystems.

**Code to use:** ripgrep and its dependency tree, unmodified. This is the case §46 rule 2 and the
prior-art note's TCB boundary both point at, and 64 already measured that the tree builds.

**A design to copy:** Fuchsia is the closest existing system, because its directory handles carry
rights and enumeration is one of them rather than an ambient consequence of having a path. Worth
reading for how it handles rights reduction when a handle is re-opened, which is the same question
§47 answers with "a child can never exceed its parent".

**A mistake to avoid:** the general shape of a Unix `readdir`, where the ability to list follows from
being able to name the directory at all. Redox is the neighbour that keeps Unix ergonomics, and the
thing to take from it is the ergonomics without the ambient reach. Getting this wrong looks like
ripgrep working beautifully and confinement being decorative.

## BUGS

- **`ReadDir` reads a listing whole rather than streaming it**, which milestone 64's PAL documents as
  a deliberate choice. A directory with very many entries therefore costs memory proportional to its
  size, and a recursive walk meets many directories. Unmeasured, and this milestone is what would
  measure it.
- **Single-threaded only**, per §43. Any published number that does not say so is dishonest, and any
  comparison that does not pin the other side to one thread is worse.
- **ripgrep memory-maps large files by default** and this system has no `mmap`, which milestone 99's
  block also names as a gap. `--no-mmap` is the workaround and its cost is unmeasured.
- **The benchmark measures this tree, not a class of systems.** One microkernel's IPC cost is not
  "microkernels are slow at walks", and the note that records it must say so.
- **`ignore` building is not `ignore` behaving.** 64's probe proved it compiles. Whether its
  metadata-heavy paths and gitignore semantics behave identically here is a separate question that
  only running it answers. The correction above is the sharp version of this: it compiles, and it
  cannot walk past one level, and no probe that only builds a crate would have found that.
- **A walker's cost is per component and invisible.** Whatever milestone 122 chooses, descending a
  path is one IPC round trip per component, and a tool that walks a deep tree pays it on every open.
  That is part of what the benchmark below is for, and it is a cost `ripgrep` on Linux does not have.

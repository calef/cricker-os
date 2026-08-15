# 99. `git` on nife: the tool this project is built with, hosting its own history

**Status: NOT-STARTED.** Raised 2026-08-04 by calef. A second "somebody else's real application"
target beside milestone 66's Vaultwarden, chosen for a different reason: not the hardest workload,
but the one whose success statement is unanswerable. **A capability microkernel that can hold its
own source history is a machine that does real work**, and the demo needs no explanation to any
audience that has ever used a computer.

**Gate: MILESTONE 64.** The first fork is decided (gitoxide, calef, 2026-08-04), and the block
sequences the work after 64 measures what a real crate needs, because 64's probe crates are the
cheap version of this milestone's first week.

**Why this is a better *first* real workload than Vaultwarden**, which the roadmap already calls
the largest single item on it. Local git needs **no network, no threads, no async runtime, and no
SQLite**: `init`, `add`, `commit`, `log`, `status`, `diff` are a filesystem, a hash, a compressor,
a clock, and a place to put bytes. Every one of those is something this tree either has or is
building, and the filesystem half is precisely what milestone 57's write-half just finished. Where
Vaultwarden's gap list names five subsystems that do not exist, this one's names mostly widths of
things that do.

**The first fork, and it is the milestone's biggest decision: gitoxide, or C git.**

- **`gitoxide` (Rust)** rides the `std` PAL milestone 27 built and milestone 64 will widen, so the
  work lands as PAL surface this tree wants anyway, and every gap is a Rust `Unsupported` with a
  known owner. It also keeps the whole workload inside the language the verification story is
  written in.
- **C git** is the real thing, and would prove the C seam (`c_shim`, `c_confiner`) at a scale far
  past anything it has carried, but it wants a libc surface, `fork`/`exec` semantics this kernel
  deliberately does not have (git spawns itself constantly: hooks, pagers, editors, `git` calling
  `git`), and `mmap` for packfiles.

**Decided 2026-08-04 (calef): gitoxide first.** Its gaps are this project's own roadmap rather
than a compatibility project, so every `Unsupported` it hits is PAL surface milestone 64 wants
anyway and lands with a known owner. C git becomes a later, harder claim rather than a
prerequisite, and the day it is attempted the `fork`/`exec` question is a design fork of its own
(this kernel spawns by capability, and git spawns itself constantly), not a porting task.

**The measured gap, so nobody starts blind.** `std::fs` answers `Unsupported` for **32 of its 54**
functions today (milestone 64's table). Git's floor needs, at minimum: create and open with the
right modes, read, write, rename (git's atomicity story is write-a-temp-then-rename, everywhere),
`unlink`, `mkdir` recursive, `stat` with sizes and mtimes, and directory iteration. Milestone 47
already notes `rename`, `unlink` and `rmdir` are now **binding gaps rather than missing verbs**,
which is the good kind of gap.

**The staging**, each stage a claim someone can check:

1. `git init` and `git commit` of one file, then read the repository back with the host's real git
   over the same disk image, which is milestone 57's proof shape reused: the host is the referee.
2. `git log`, `git status`, `git diff` against a repository the *host* created, so the reading half
   is proven against bytes this OS did not write.
3. A repository with real history: this project's own tree, committed on the target.
4. Deferred, each its own decision: network (`clone`, `fetch`), hooks and any subprocess use, and
   packfile `mmap`.

## Scope note

Not a git implementation and not a compatibility project: the deliverable is *running an existing
one*. If a stage needs a change to gitoxide, that is a dependency decision under §46 and a patch
under `patches/`, the same road milestone 57 took with RedoxFS. Sequenced after milestone 64
measures what a real crate actually needs, because 64's probe crates are the cheap version of this
milestone's first week, and after milestone 91's glossary if it lands, since a reader meeting
"packfile", "ref" and "object database" deserves the same treatment acronyms get.

# 122. A directory handle `std` can hold: `OPENDIR` reaches the PAL

**Status: NOT-STARTED.** Minted 2026-08-13, extracted from milestone 121 once the actual blocker was
found. It is small, it is gated on nothing, and it currently stands in front of 121, §84's entire
first preference, and any real port.

**Gate: NONE.** Every verb it needs exists in the contract and is in production use.

## The finding

**Descent is built.** `fs_proto`'s `OPENDIR` (op 8) resolves one name under the directory handle in
`req_handle`, requires `DESCEND` on the parent, and attenuates the child's rights so no descendant can
exceed its ancestor. `rm`, `swish`, `fs_subtree_caretaker`, `fs_nameset_caretaker` and the FS server
all use it. §47's model is real and native programs walk with it.

**`std` cannot reach it.** The PAL calls `OPENDIR` in exactly one place, inside `read_dir`, against
`proto::ROOT`, to produce a listing, and then lets the handle go. Nothing in `std` holds a directory,
so `one_name` has no object to resolve a second component against and refuses nested paths.

The consequence is the sharp one milestone 121 records: `read_dir(".")` yields `./name` and feeding it
back to `File::open` works, while one level down an entry's `path()` is `./sub/name`, which is two
components, which is refused. **A `std` program can list a subdirectory and cannot open what it finds
there.** `walkdir` and `ignore` build and cannot walk.

## What to build

A directory object in the PAL that **holds** an `OPENDIR` handle, with `OPEN`, `READDIR` and `MKDIR`
issued against it rather than against `ROOT`. No contract change: every verb already takes a
`req_handle`, and the PAL already knows how to obtain one.

## The design fork, which is the real content

`std`'s filesystem API is path-shaped. `File::open("a/b/c")` composes names, and this system does not
compose names. There are two ways to answer that and they are not exclusive.

**Option A: the PAL walks internally.** `File::open("a/b/c")` becomes `OPENDIR a`, `OPENDIR b`,
`OPEN c`, each hop attenuated. Unmodified `walkdir`, `ignore` and most `std`-shaped software then
work.

The authority story is unchanged and worth stating carefully, because it looks like a retreat and is
not. Every hop still resolves under the granted directory, `..` and absolute paths are still refused,
rights still only narrow, and there are no symlinks to escape through. **The grant is exactly as tight
as before**; what changes is only whether the program has to spell the descent itself. This is what
`cap-primitives` does on Unix, except that there it is defending against a hostile namespace and here
the safety is structural.

The costs are real: one IPC round trip per component on every open, a cost the caller cannot see, and
a handle held per level while a walk is in progress.

**Option B: expose the directory object.** Programs take a directory and open one name under it, which
is `cap-std`'s `Dir` and this system's actual model. Honest about cost, aligned with §84's first
preference, and it leaves path-shaped `std` programs where they are.

**Recommendation: build B, then A on top of it**, and measure A. B is the primitive and the thing
`cap-std` would bind to; A is compatibility built out of B, and it should exist because §84's second
preference (a faithful port plus a small patch) is worthless if nothing runs at all before the patch.
Milestone 121's benchmark is what prices A, and if per-component IPC turns out to dominate, that is
the input to whether the contract should grow a multi-component resolve.

## Why it is worth its own milestone rather than a line in 121

Because three separate things are waiting on it and only one of them is `ripgrep`. §84's first
preference is software already written against `cap-std`, and `cap-std` has nothing to bind to until a
directory object exists. Milestone 64's "35 of 50 crates built with no change" means *compiles*, and
the distance to *works* runs through this. And a walker is the shape of most real filesystem software,
so this is the difference between porting one tool and being able to port tools.

## Prior art

**Code to use:** none directly, but `cap-primitives` is the design to read, because it solves exactly
this problem against a far more hostile substrate.

**A design to copy:** `openat` semantics, which is what `OPENDIR` already is. The question this
milestone answers is how a path-shaped standard library sits on top of an `openat`-shaped contract,
and Unix answered it by keeping both and letting programs choose.

**A mistake to avoid:** making Option A the only answer. If path composition is the only interface, no
program is ever written to hold a directory, and the system's model becomes invisible to the software
running on it. That is how a capability system quietly becomes an ambient one with extra steps, which
is §82's stated failure mode.

## BUGS

- **Handle exhaustion is unbudgeted.** `fs_proto` has a `MAX_HANDLE`, and a deep walk under Option A
  holds one handle per level. Nothing today says what happens when a walker is deeper than the budget,
  and the honest answer is that nobody has tried.
- **Lifetime and revocation are unspecified.** When a retained directory handle is closed, and what a
  program observes if the directory it holds is revoked underneath it, are both open. The revocation
  half is milestone 108's question one level up.
- **Per-component IPC is invisible at the call site.** A `std` program pays a round trip per path
  component and has no way to know. That is a performance cliff of exactly the kind this project
  otherwise insists on measuring, and it will not appear in any host test.
- **This does not make `cap-std` run.** It builds the object `cap-std` would bind to. The backend work
  is separate and §84 records that it is unmeasured, including whether `cap-primitives` has a seam a
  third backend can use at all.

# 122. A directory handle `std` can hold: `OPENDIR` reaches the PAL

**Status: BUILT** on 2026-08-18 (PR #320). Both options were built, in the order this block
recommends, and both are proven end to end on both ISAs by the `std_exerciser` transcript. What the
recommendation also asked for and this milestone did **not** do is measure A; see "What it did not
do" below, which also names the one contract question it turned up.

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

## What was built

**Option A, the walk.** `walk` in `sys/fs/nife.rs` resolves a path to the directory its last name
lives in, one `OPENDIR` per intermediate component, and every name-taking verb goes through it:
`open`, `create`, `mkdir`, `unlink`, `rmdir`, `rename` and `readdir`. Every hop asks for
`DESCEND | <what the final verb needs>`, which reads like a widening and is not: a child's rights
are its parent's intersected with the request, so a right an ancestor lacks is one no descendant of
it could have had either, and the walk therefore asks for exactly the maximum the grant could give
at the depth it is going to. At most two handles are open at once, because each hop drops the last.

**Option B, the directory object, and it was not an interface this project had to invent.**
`std::fs::Dir` already exists upstream behind `#![feature(dirfd)]` (rust-lang/rust#120426) and *is*
`openat`. nife was getting std's generic fallback, which stores a `canonicalize`d path, so
`Dir::open` on this system failed at its first call and the std type most aligned with what this OS
*is* was the one type it could not offer. It is now a held `OPENDIR` handle, and it is what
`cap-std` would bind to (§84's first preference).

**And `remove_dir_all` needed no code at all.** std's generic implementation is written in terms of
`read_dir`, `remove_file` and `remove_dir` on paths it composes with `DirEntry::path`, so it started
working the moment those paths resolved. It had been refused here with a note explaining that the
recursion has to descend and a nested path is refused, which was true when it was written. That is
the fourth refusal in this PAL found to have outlived its own reason, after milestone 64's three.

**Two live bugs the walk found**, both invisible to a flat path and both in the tree before this
milestone:

- **A created file was unwritable one directory down.** A file handle carries
  `parent & (READ | WRITE)`, so a descent that asked only for `CREATE` mints a file nobody can
  write: `std::fs::write("a/b")` created the file and then failed `EROFS` on its own first write.
  Against the granted directory it could never show, because nothing is requested there.
- **`dir::EPERM` was being reported to std programs as `Unsupported`.** The PAL read a `-1` reply as
  the kernel refusing the invoke, on the recorded ground that `EPERM` is not in the FS server's
  vocabulary. It has been since milestone 47. So the one answer meaning "this capability does not
  carry that right" arrived as "this platform cannot do that", which is §42's silent degradation and
  would have made a narrowed grant indistinguishable from no grant at all.

## What it did not do

- **The per-component IPC is still unmeasured**, which is the half of the recommendation this
  milestone owes. Milestone 121's benchmark is what prices it, and 121 has not started. Nothing here
  is slower than it was; what is unknown is what a deep walk costs against a flat open, and whether
  the answer argues for a multi-component resolve in the contract.
- **The rights discipline is exercised only under a full-rights grant.** Every std test grants the
  mount root, so a walk that over-asked would pass all of them, which is the exact trap this PAL has
  recorded nearly falling into once already. The negative controls that do run are structural (`..`
  at any position, a path through a file, `..` through a held `Dir`). A std program spawned on an
  `fs_subtree_caretaker` endpoint is the test that would close it and is proposed as its own
  milestone.
- **It turned up one contract question, and it is calef's.** `OPENDIR` has no way to say "attenuate
  to whatever you have": the request is refused with `EPERM` when the intersection comes up short,
  which is right for a verb asking for a minimum and wrong for an object that has no single verb.
  A held `Dir` therefore asks for `dir::ALL` and, when a narrowed grant refuses, finds out what is
  there by asking for one right at a time (six messages, at most once per `Dir::open`). That works
  and is recorded as a workaround where a reader meets it. The fix is a sentinel in the rights word,
  which is a wire-format change and not a lane's to make.

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

- **Handle exhaustion is unbudgeted, and the walk turned out not to need a budget.** The forecast was
  one handle per level held for the length of a walk; the implementation holds **two**, because each
  hop closes the one before it, so a path's depth costs round trips rather than handle-table slots. A
  held `Dir` is one handle for as long as the program keeps it, which is the same shape as an open
  `File` and is bounded by the same `EMFILE`. What is still untried is a program holding many `Dir`s
  at once.
- **Lifetime and revocation are unspecified.** When a retained directory handle is closed, and what a
  program observes if the directory it holds is revoked underneath it, are both open. The revocation
  half is milestone 108's question one level up.
- **Per-component IPC is invisible at the call site, and is still unmeasured.** A `std` program pays a
  round trip per path component and has no way to know. `File::open` on a path that must be created
  pays for the walk **twice**, once for the `OPEN` that reports `NotFound` and once for the `CREATE`,
  because the two ask for different rights. That is a performance cliff of exactly the kind this
  project otherwise insists on measuring, and it will not appear in any host test.

- **A revoked FS endpoint now reads as `PermissionDenied` rather than `Unsupported`.** The two error
  spaces overlap (`-1` is both the kernel's `NoSuchSlot` and the server's `EPERM`), and this
  milestone moved which one wins, because `EPERM` is reachable every day and revocation of the FS
  endpoint is milestone 108's open question. The clean fix is the contract's: a tag or an offset in
  the reply word.

- **A held `Dir` asks for `dir::ALL` and probes on refusal**, six extra messages under a narrowed
  grant, because `OPENDIR` cannot be asked to attenuate rather than refuse. See "What it did not do".
- **This does not make `cap-std` run.** It builds the object `cap-std` would bind to. The backend work
  is separate and §84 records that it is unmeasured, including whether `cap-primitives` has a seam a
  third backend can use at all.

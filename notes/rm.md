# `rm`, `rm -r`, and why removal needs a directory

Milestone 47. Built 2026-07-31. The contract side is `fs_proto::fs::RMDIR`; the program is
`user/src/rm.rs`; the grant is `grant_plan::DirSpec`.

## Removal is an operation on the directory, not on the file

The thing that shapes everything here: **no per-file capability can express "take this name away."**
A name lives in a directory, so removing it is an operation on the directory that holds it. `wc
report.txt` can be granted the file; `rm report.txt` cannot.

So `rm` is the first program granted a **directory** rather than a file, which is why `Manifest`
grew `DirSpec` alongside `FileSpec`. The child receives a capability to the directory the name is
*in*, plus the name.

## `-r` widens the grant, and that is the whole safety property

`DirSpec::Required { subtree_flag: Some(b'r') }`. Without the flag, the capability carries the
authority to take names out of one directory **and nothing else**: it cannot list what is under a
subdirectory and cannot descend into one. With the flag, it widens to walking and listing underneath,
which is what a recursive removal needs at every level.

The consequence is worth stating precisely, because it is the difference between this and every other
`rm`:

> A program run without `-r` holds no way to descend, so **its recursion is not disabled by a branch
> anybody has to get right.**

Unix's `rm` decides not to recurse. This one *cannot*. And because the widening happens at the
prompt, `caps rm -r logs/` shows strictly more authority than `caps rm logs/`: **typing `-r` is
visibly handing over more**, rather than setting a flag whose consequences are elsewhere.

## `RMDIR` is empty-only, which is Unix's choice and the reason the rest is safe

`RMDIR` removes an empty directory, requires `REMOVE` on the parent, and answers `ENOTEMPTY`
otherwise. **No single call in the contract can take a subtree away.** The recursion lives in
userspace as a loop of individually safe single-step operations: walk, unlink files, remove empty
directories bottom-up.

It is **not** revocation, for §48's reason: the FS server's handle table is per *server*, so handles
cannot be invalidated for clients the server cannot enumerate. Removing a name is not destroying an
object.

## The walk stops where the capabilities stop

`rm -r` needs `ENUMERATE` to see, `DESCEND` to recurse, and `REMOVE` to delete, **at every level**. So
the bound is structural rather than a check.

`rm(1)` ships a literal special case: "it is an error to attempt to remove the files `/`, `.` or
`..`", because Unix needs one. **We have no such case and need none**: a shell holding a subtree
cannot name the root, so there is nothing to special-case. If a future change makes such a guard feel
necessary, that is a signal something else broke, not a licence to add it.

## Unix semantics, checked against `rm(1)` rather than remembered

- **Silent on success.** `-v` exists precisely because the default prints nothing.
- **Failure is a diagnostic plus a non-zero exit.** Exit 0 only if everything named was removed.
- **`-f`** ignores a nonexistent file, does not prompt, and for a missing name suppresses *both* the
  diagnostic and its effect on the exit status. Its real value is **idempotency**: `rm -f maybe-there`
  succeeding is what makes a script re-runnable, and "absence is the desired state" is not a lie about
  failure. An earlier draft of milestone 47 argued `-f` should not exist, on the reasoning that with no
  prompting its only meaning is suppressing errors. That was wrong about what `-f` does.
- **`rm` on a directory without `-r` is a refusal** (`EISDIR`), never a silent escalation.
- **An interrupted `rm -r` leaves a partial tree**, reports what failed, and exits non-zero. There is
  no transaction spanning requests, and adding one would mean the server holding a transaction open
  across receives, which breaks the serve-loop-runs-one-request-to-completion property §47 relies on
  for concurrency atomicity.

## The fixture detail that makes `-f` testable

`RM_MISSING` is deliberately a name the fixture **never stages**. Without that, the `-f` run and the
plain run would be indistinguishable, because the whole of `-f` is that a name which is not there is
not an error. A fixture that accidentally shipped that name would make the test pass while proving
nothing, and `fs_proto` has a host test asserting it is never staged.

## `rm *.txt`: the operand can be a set, and then it is the namespace

Milestone 47's globbing lane, [glob-grant.md](glob-grant.md). `rm old.txt` grants the directory
holding one name; `rm *.txt` grants that directory **attenuated to the names the pattern matched**,
served by `user/src/fs_nameset_caretaker.rs`. The over-grant this note used to declare (the
capability could remove anything else in that directory) is closed for a pattern operand and remains
for a literal one, which is the honest state: a single name still travels through
`fs_subtree_caretaker`.

A set does not fit in the two argument words a name rides in, so `rm` is started with a grant whose
name is **zero bytes long** (`fs_proto::grant::WHOLE_NAMESPACE`). It means "the operand is your
namespace", and `rm` learns the names by enumerating the capability it holds, which reveals exactly
what the command line already printed.

It sweeps in **one listing with no rounds**, and the contrast with the recursive walk is a fact about
the namespace rather than an optimization. That walk must re-read from cursor 0, because removing a
name shifts a real directory's entries; a set namespace is fixed, so re-reading would hand the loop
the names this run has already taken away.

## BUGS

- **A verdict word must not look like a byte count.** The report channel carries text frames (first
  word = a byte count, at most 16) and one verdict. The receiver reads "the first message is the
  verdict" as "the run printed nothing", which is what makes `rm(1)`'s silence-on-success checkable.
  `fs_proto` asserts `VERDICT > 16` at **compile time** (`const _: () = assert!(…)`), so a change that
  broke it fails the build rather than waiting for the suite.
- **No `rm -i`.** There is no prompting anywhere in this system, so the flag has nothing to mean.
- **Recursion depth is real stack**, since the program has no allocator and each level holds a listing
  buffer by value. `rm` asks for 4 stack pages. A deep enough tree will exhaust it, and the failure
  will be a data abort rather than a diagnostic.
- **`rm` removes nothing this contract has since grown**, and that is now checked rather than
  assumed. Milestone 61 made the caretakers dispatch off `fs_proto::verb`, so a verb added to the
  contract is forwarded by them from the day its row exists; `rm` still holds `REMOVE` and nothing
  else, so what it can *send* is unchanged. The two facts are independent and it is worth saying so:
  the caretaker got wider, the grant did not.

## See also

- DECISIONS §47 (the directory-capability keystone and its six rights), §48 (navigation, and why `rm`
  was a builtin before this), §42 (a filesystem declares what it offers and must be truthful).
- Milestone 47's "`rmdir` and `rm -r`: Unix already made the safe choice" in `design/roadmap/47-navigation-and-naming.md`.
- `notes/dir-capability.md` for the rights ladder and `fs_subtree_caretaker`, and
  `notes/glob-grant.md` for the nameset caretaker a pattern operand is served by.

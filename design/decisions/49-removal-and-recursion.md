# 49. Removal is a directory operation, and `-r` widens the grant rather than setting a flag

**Built 2026-07-31** (milestone 47). Concept note: notes/rm.md. Rests on §47's rights ladder and §48's
navigation.

## Why `rm` needed new grant surface

**No per-file capability can express "take this name away."** A name lives in a directory, so removal
is an operation on the directory that holds it. `wc report.txt` can be granted the file; `rm
report.txt` cannot. So `rm` is the first program granted a **directory**, and `Manifest` grew
`DirSpec` beside `FileSpec`. The child gets a capability to the directory the name is *in*, plus the
name.

This also moved `rm` from a **builtin** (where §48 left it) to a **program**, which is Unix's shape.
`cd`/`pwd`/`ls` stay builtins because the shell is rebinding what it already holds; `rm -r` is a
destructive loop. A builtin would run with the shell's **entire endowment**, while a program takes an
explicit attenuated grant.

## The result worth the whole milestone

`DirSpec::Required { subtree_flag: Some(b'r') }`. Without the flag the capability carries authority to
take names out of one directory **and nothing else**: it cannot list beneath a subdirectory and
cannot descend into one. Typing `-r` widens it to walking what is underneath.

> **A program run without `-r` holds no way to descend, so its recursion is not disabled by a branch
> anybody has to get right.**

Unix's `rm` *decides* not to recurse. This one **cannot**. And because the widening happens at the
prompt, `caps rm -r logs/` shows strictly more authority than `caps rm logs/`: **typing `-r` is
visibly handing over more**, rather than setting a flag whose consequences live elsewhere. That is the
grant expression (§14, milestone 31) reaching a case where the flag *is* part of the grant.

## `RMDIR` is empty-only, which is what makes the recursion safe

Requires `REMOVE` on the parent, answers `ENOTEMPTY` otherwise. **No single call in the contract can
take a subtree away**; the recursion is a userspace loop of individually safe single-step operations.
Not revocation, for §48's reason: the handle table is per *server*, so handles cannot be invalidated
for clients the server cannot enumerate.

`rm -r` needs `ENUMERATE`, `DESCEND` and `REMOVE` **at every level**, so the walk stops exactly where
the capabilities stop: structurally, not by a check.

**`rm(1)` ships a literal special case** ("it is an error to attempt to remove the files `/`, `.` or
`..`") because Unix needs one. We need none: a shell holding a subtree cannot name the root, so there
is nothing to special-case. If such a guard ever feels necessary, that is a signal something else
broke.

## Unix's semantics, kept, and one correction to this project's own reasoning

Silent on success (`-v` exists because the default prints nothing); failure is a diagnostic plus a
non-zero exit; `rm` on a directory without `-r` refuses with `EISDIR`; an interrupted `rm -r` leaves a
partial tree, reports, and exits non-zero, because a transaction spanning requests would break §47's
one-request-to-completion property.

**`-f` stays, and an earlier draft of milestone 47 was wrong about it.** That draft argued it should
not exist, reasoning that with no prompting its only remaining meaning is suppressing errors, which
§42 forbids. But `-f` ignores *nonexistent* files and suppresses their effect on the exit status; its
real value is **idempotency**, and "absence is the desired state" is not a lie about failure. Recorded
because the wrong reasoning is the reusable part: a divergence has to be checked against what the
thing actually does, not against what its name suggests.

## The fixture detail that makes `-f` testable at all

`RM_MISSING` is a name the fixture **never stages**, asserted by a host test. Without that, the `-f`
run and the plain run are indistinguishable: the whole of `-f` is that a name which is not there is
not an error, so a fixture that accidentally staged it would make the test pass while proving nothing.

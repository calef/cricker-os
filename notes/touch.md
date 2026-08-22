# `touch`: create if absent, and the mtime half that is not built

Milestone 47. Built 2026-08-22 (the create half only). The contract side is `fs_proto::fs::CREATE`,
already milestone 31 phase 2's; the builtin is `Nav::touch` in `user/src/swish.rs`, parsed by
`grant_plan::Command::Touch`.

## What this does

`touch <name>` creates an empty file at `<name>` if it is not there. If it is already there,
whatever kind it is, `touch` does nothing and reports success.

A **builtin**, in `mkdir`'s category rather than `rm`'s: it takes no more than the directory
capability this shell already holds (`fs::CREATE`, the same right `mkdir` needs), so there is
nothing to attenuate and nothing gained by confining it to a program with its own grant. `mkdir`
already established the shape (`fs_proto::fs::MKDIR` mints a name and hands the capability straight
back); `touch` is the same call with the handle closed instead of kept, because there is nothing
useful to do with a handle to an empty file the moment after making it.

## EXAMPLES

```
$ touch report.txt
$ ls
  report.txt
$ touch report.txt
$ ls
  report.txt
```

The second `touch` is silent and changes nothing: no diagnostic, no new content, no error.

```
$ mkdir logs
$ touch logs
$ ls
  logs/
```

`touch` on a name that already exists as a directory is also a no-op, because "the name is already
there" is answered before this builtin ever asks what kind of thing it is. Contrast `rm`, whose
`EISDIR` reports the kind precisely because unlinking a directory is a mistake worth naming; `touch`
never inspects the kind, because there is nothing for it to do differently.

## What this does not do, and why

Unix's `touch` does two other things, and neither is built:

- **Bumping the modification time of a name that already exists.** `fs_proto` carries no verb for
  it: `notes/std.md` records that the FS server keeps an mtime internally but the contract does not
  expose one to set. That gap has closed for the excuse it used to have (milestone 51 landed a wall
  clock, so "there is nothing to set it to" is no longer true), but closing the excuse is not the
  same as building the verb.
- **`touch -t`, setting an arbitrary time.** This is a sharper question than the one above: it is
  the ability to *lie about history*, which matters for anything reasoning from mtime, a backup
  target (milestone 55) not least. Whether "set to now" is the write right this shell already holds,
  or a separate authority the way `date`'s clock capability is separate from everything else this
  shell can do, is design/roadmap/47-navigation-and-naming.md's open question. It has not been
  decided, so nothing here answers it by default.

Building the create half now and leaving the mtime half open follows the same split this milestone
already made twice: `rm` (unlink) and `RMDIR` (structural bound) shipped as two verbs rather than
one that tries to do both, and `ln`'s hard-link and symlink halves are tracked as two separate
decisions for the same reason. A verb that is unambiguous should not wait on one that is not.

## Tests

`crates/grant_plan` and `crates/swish` host suites: `touch` parses to itself, is reserved from the
program namespace like every other builtin, and appears in `help`. `kernel::user::shell_navigation_tests`,
both ISAs: the navigating shell touches an absent name (`TOUCH_CREATED`), writes a body into it, and
touches it again to prove the second call is a no-op rather than a truncate (`TOUCH_PRESERVED`);
the pair is what makes either bit non-vacuous, the same shape `rm`'s `NAME_GONE_AFTER_UNLINK` and
`HOLDER_KEPT_READING` already use. Witnessed from the host by `xtask`'s `shell_navigation_landed`,
which confirms the name reached the platter and did not leak to the parent directory.

## BUGS

- **No mtime verb at all**, so `touch` on an existing file cannot do the one thing Unix's `touch`
  is usually typed for. A script that relies on `touch`-to-bump-mtime (make-style staleness checks,
  for instance) will not observe any change here.
- **The authority question for `-t` is unresolved**, not merely unbuilt: it is unclear whether
  setting an arbitrary mtime should ride the write right this shell already holds or need something
  narrower. Building the verb before that is answered would ship a decision by accident.
- **No `-c` (don't create)**, because Unix's `-c` exists to suppress the create half, and there is
  nothing else here to fall back to.

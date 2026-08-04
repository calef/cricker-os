# 57. Extended attributes forward through the caretakers, and the server enforces direction

Milestone 61. See `notes/xattr.md` and `notes/dir-capability.md`.

All three caretakers forward `GETXATTR`, `SETXATTR`, `LISTXATTR` and `REMOVEXATTR`. Before this they
answered `EOPNOTSUPP`, so **a program behind a per-file grant could not read its own file's
attributes** while every other holder could.

## Which side enforces the direction, and why it differs

- **`fs_file_caretaker` enforces it itself**, because it holds one handle opened once with the
  directory's rights, so there is no later moment at which a server could check.
- **The other two do not enforce it at all.** Their one `OPENDIR` minted a handle the server
  restricted, and every attribute request rides that handle, so the server refuses a write on a
  read-only grant without the caretaker having a branch. That is `fs_subtree_caretaker`'s design
  property (§56) holding for a capability it had never seen.

Stated because the asymmetry looks like an inconsistency and is the opposite: **the same rule, that
attenuation lives in the narrowest thing that can hold it**, applied where each of them can.

## BUGS

- **A read-only grant is refused by the server rather than by the caretaker**, so the refusal arrives
  one hop later than a reader might expect. It is the same errno either way.
- Everything in §54's list still stands: no type code survives a host recovery, `MAX_VALUE` at 3 KiB
  is untested against real Time Machine traffic, and an unlinked-but-open file loses its attributes
  immediately, unlike POSIX.

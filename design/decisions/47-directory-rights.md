# 47. A directory capability carries six rights, and a child can never exceed its parent

**Status: DECIDED.**

**Built 2026-07-31** (milestone 47's keystone). Concept note: notes/dir-capability.md.

Milestone 47's own finding was that `cd`, `mkdir` and per-process namespaces all converge on one
missing primitive: **a verb that returns a directory capability rather than bytes**. It is the first
place the §27 contract hands back *authority* instead of *data*, which is why it got §32's level of
care rather than being added as another opcode.

## The rights, and why there are six

`ENUMERATE`, `READ`, `WRITE`, `CREATE`, `REMOVE`, `DESCEND`. Milestone 47 named five; **`DESCEND` is
the one it did not, and it earns its rung**. If descending came bundled with reading, granting a
directory would transitively grant its entire subtree, and **the shape of the tree would decide how
much authority a grant carried**: ambient authority reintroduced by recursion. A file handle inherits
its directory's `READ`/`WRITE`, so "open, read versus write" is structural: what may be done to a file
was decided when the directory was granted.

## The refusal errno is part of the decision, because it decides what the holder learns

- A **naming** right withheld (`READ`/`WRITE` for `OPEN`, `DESCEND` for `OPENDIR`) answers `ENOENT`:
  *in this scope there is no such name*. `fs_file_caretaker`'s sentence, for `fs_file_caretaker`'s
  reason.
- A **mutating** right withheld (`CREATE`, `REMOVE`, `WRITE`) answers `EROFS`, **not `EACCES`**, per
  §27: `EACCES` implies a policy that could have said yes, and there is no policy here.
- `ENUMERATE` withheld answers `EPERM`, the one rung where neither works. "No such name" is nonsense
  when you hold the directory, and an empty listing would be **a lie about the directory rather than a
  fact about the capability**: §42's silent degradation exactly.

## Attenuation is by construction, not by check

`Rights::attenuate` is `parent & requested` and is **the only constructor** for a non-root rights set
(`Rights::root` is called once, by the mount). There is no widening path to forget. The server also
refuses when the intersection is smaller than the request, but that is *truthfulness, not safety*:
delete it and the property still holds. Proven three ways: Kani at one and two levels, host tests,
and a guest probe, and falsified first: returning `Rights(requested)` turns 3 host tests and 2
harnesses red.

## The structural finding: the handle is the authority, the endpoint is the boundary

The FS server's handle table is per **server**, not per client. So a rights-carrying handle
attenuates only its holder: **anyone holding the FS-service endpoint can name `fs::ROOT` and be back
at the image root.** That is why `fs_subtree_caretaker` exists, and it is the same wall
`fs_file_caretaker` hit (no badged endpoints, one receive per server).

`fs_subtree_caretaker` performs **no rights checks at all**: one `OPENDIR` at startup, then pure
handle-namespace translation. The attenuation lives entirely in the handle the server minted. **A
stronger story than `fs_file_caretaker`'s**, which does inspect requests: there is no check to get
wrong.

## The bug this shipped with, and the general rule it produced

`fs_subtree_caretaker` panicked in its own `_start` on riscv64 and passed on aarch64. Three
processes share one frame, justified by `fs_file_caretaker`'s argument that every request on both
hops is a blocking `CALL`, so a client is parked inside its own call while the caretaker uses the
page. **That holds once the caretaker is serving and not at startup**, where the caretaker stages
the granted name and then blocks, and a confined program that already exists overwrites it.

In the failing case it is not even a race: when the wiring call also wires the FS service, the
server is parked in its readiness `SEND`, so the caretaker's descent cannot be answered until
someone drains it, and the client owns that entire window. **So draining a readiness sentinel is
sequencing, not merely an assertion.** `fs_file_caretaker` carried the same latent bug and took the
same three lines. The fix is ordering, not a second page.

## `RENAME`

`REMOVE` on the source, `CREATE` on the destination, checked before anything resolves.
**Concurrency-atomic** because the serve loop runs one request to completion, so there is no
concurrent observer inside the server. **Crash-atomic, and measured rather than asserted**: it is the
final operation of `crash_consistency.rs`'s workload, so the sweep that cuts the device at every write
cuts inside it, with both names in `NAMES` so "both" and "neither" fail. Milestone 55 depends on this
(`fruit:posix_rename`).

Not offered, each refused loudly rather than silently: `renameat2`'s `EXCHANGE`/`NOREPLACE` (§42),
cross-filesystem move, and moving a **directory** between directories (`EINVAL`: POSIX's cycle guard
is a path-prefix test and this contract has no paths). Two kind checks are ours rather than the
engine's: RedoxFS's `rename_node` will rename a file over a directory.

## What the proof looks like from outside

Three runs against three rights sets, each other's controls. The attacker is told nothing about its
grant beyond a run index and reports a bitmap the test checks exactly; `OPENED_ITS_OWN` and
`GRANTED_ACCESS_FAILED` stop a caretaker that refuses *everything* from passing. Then from outside
the guest entirely, the host tool reads the image the run left and asserts the fixture intact, no
attacker-made name at the root, its creations in `sub`, and `sub` holding **both a renamed and an
unrenamed name**. **No in-guest verdict could report that.**

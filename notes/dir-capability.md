# The directory capability (milestone 47)

Milestone 47's keystone. A directory used to be **one** authority: hand a program somewhere to
write its logs and you also handed it the power to read everything already there and to delete it.
This note is the design that splits that into separable rights, the verb that hands a *directory*
back rather than bytes, and the process that makes a subtree grant checkable from outside the
program being confined.

The contract lives with its code in `crates/fs_proto` (the `dir` and `dirent` modules, and the
`OPENDIR`/`READDIR`/`MKDIR`/`RENAME` verbs in `fs`). The engine-side implementation is
`fs-server/src/lib.rs`; the caretaker is `user/src/dwarden.rs`; the wiring and the attacks are
`kernel/src/user.rs` (`fs_service::start_granted_dir` and `dir_capability_tests`). This note is the
argument around them. Read [fs-server.md](fs-server.md) first for the contract this extends.

What milestone 47 goes on to build (`cd`, `pwd`, `ls`, `mkdir`, `rm`, globbing, completion) is the
easy part once this exists, and it is deliberately not here. The five commands are built:
[shell-navigation.md](shell-navigation.md). Four of them were indeed easy; `rm` was not, because
"remove a name" and "destroy an object" are one operation in the engine underneath until you make
them two.

## The ladder, and why there are six rungs

The roadmap named five separable rights. There are six.

| Rung | What it is | The verb that consults it |
|---|---|---|
| `ENUMERATE` | list the names in it | `READDIR` |
| `READ` | open a name in it for reading, and read what you opened | `OPEN`, then `READ` |
| `WRITE` | open a name for writing, and write or truncate it | `OPEN`, then `WRITE`/`TRUNCATE` |
| `CREATE` | make a new name in it | `CREATE`, `MKDIR`, `RENAME`'s destination |
| `REMOVE` | take a name out of it | `RENAME`'s source (and `UNLINK`, later) |
| `DESCEND` | walk into a child directory | `OPENDIR`, and `MKDIR` alongside `CREATE` |

`ALL` is the six together, and it is what the mount binds its root with. Nothing below the root can
ever be constructed with more.

### `DESCEND` earns its own rung, and this is the finding

The roadmap's five did not separate walking in from reading. Bundle them and **granting a directory
transitively grants its whole subtree**, to any depth. The authority a grant carries would then be
decided by *the shape of the tree* rather than by the grant: the same words, `here is somewhere to
put your logs`, hand over a lot or a little depending on what happens to be underneath. That is
ambient authority reintroduced by recursion, which is the exact thing this milestone exists to
refuse.

With `DESCEND` separate, a capability can be exactly one directory deep, and a program that holds
one cannot even learn that a subtree is there.

`MKDIR` needs `CREATE` **and** `DESCEND` together, for the same reason: making a directory you
could not have walked into would be a way to mint a capability out of a right that was withheld.

## Attenuation is by construction, not by a check

```rust
pub const fn attenuate(self, requested: u64) -> Self {
    Rights(self.0 & requested)
}
```

That `&` is the whole monotonicity property. `Rights::attenuate` is the **only** constructor for a
non-root rights set (`Rights::root` is the other one, and its single caller is the code that binds a
server to its mount), and `a & b` is a subset of `a` for every `b`. There is no code path that
widens, so there is no check to forget, no branch to get wrong, and no ordering to preserve. Three
Kani harnesses say so for every mask: at one level, at two, and that a root cannot be built carrying
a bit this contract has not defined.

The server *separately* refuses a request whose intersection came up short (`EPERM`), and it is
worth being clear about what that refusal is for. It is not the safety property. Delete it and the
intersection above still holds and the child is still bounded. What it does is refuse to hand back
less than was asked for **without saying so**, which is DECISIONS §42's rule against silent
degradation: a caller that asked for `CREATE` and got a capability without it should find out now
rather than at its first write.

The sabotage check confirms it is load-bearing: replacing the body with `Rights(requested)` turns
three host tests and two Kani harnesses red.

## The refusal errno is part of the design, because it decides what the holder learns

A missing right could always answer `EACCES` and be done. It must not, and which word it uses is a
decision rather than a detail.

- **A naming right withheld answers `ENOENT`.** `READ`/`WRITE` for `OPEN`, `DESCEND` for `OPENDIR`.
  *In this scope there is no such name.* Nothing consulted a permission, and a holder that may not
  reach a name must not be able to learn that the name is there. This is the sentence `fwarden`
  already says, for the same reason (DECISIONS §27).
- **A mutating right withheld answers `EROFS`.** `CREATE`, `REMOVE`, and `WRITE` on a file handle.
  *Through this capability, that directory is read-only.* `EACCES` was rejected on purpose, and §27
  had already rejected it for files: it implies a policy that could have said yes, and there is no
  policy here, only what the capability is.
- **`ENUMERATE` withheld answers `EPERM`**, and it is the one rung where neither of the other two
  works. "No such name" is nonsense when you are holding the directory. An empty listing would be a
  statement about *the directory* rather than about the capability, and it would be false, which is
  §42's silent degradation exactly: a verb that is not offered has to fail loudly rather than
  degrade into a plausible lie.

Two more, which are facts about a handle rather than about a right: reading through a write-only
handle is `EBADF` (POSIX's own answer), and opening a directory as a file is `EISDIR` rather than a
handle onto a directory's raw bytes.

## Handle 0 is the bound directory

The bound directory used to be a private field the server consulted. It is now an ordinary entry in
the handle table at `fs::ROOT`, which is **0** because every client that ever sent an `OPEN` already
sent 0 in that field and meant exactly this. So there is no separate "current directory" for a verb
to forget to consult: every name-taking verb resolves under a handle, always. File handles start at
1, and the root cannot be closed (`EINVAL`, because it is not something the client opened).

That is Plan 9's answer in one number, and it lines up with the roadmap's "every shell has its own
root": `/` is the root of *your* namespace, and two clients on two endpoints both say `0` and mean
different directories.

A file handle **inherits** the `READ`/`WRITE` bits of the directory it was opened under, so what may
be done to a file was decided when the directory was granted rather than by the code that opened it.
That is the roadmap's "a program handed a directory to write logs into should not thereby be able to
delete what is there", made structural.

## The structural finding: the handle is the authority, the endpoint is the boundary

This is the most important thing in the milestone, and it was not obvious going in.

**The FS server's handle table is per *server*, not per client.** Two clients sharing one endpoint
share those handles. A rights-carrying handle therefore attenuates only *its holder*: anyone holding
the FS-service endpoint can name `fs::ROOT` and be back at the image root, whatever narrow handle
they were also given. Rights on a handle are not confinement.

So confining a program to a subtree is not "give it a narrow handle". It is **give it an endpoint
that reaches nothing else**, which means a caretaker process, exactly as a per-file grant needs one
(§27's amendment, notes/grant-expression.md). The narrowing is an address space, not a branch.

Serving a second, narrower endpoint from the FS server itself would need a receive over a *set* of
endpoints, which this kernel does not offer; adding it means giving endpoint capabilities a badge
(seL4's answer), which is a design fork and is recorded as the alternative rather than taken.

### `dwarden`, and why it checks nothing at all

```text
  FS server ──file IPC──► dwarden ──narrowed file IPC──► the confined program
  (the image root)                  (one subtree, one rights set)
```

`fwarden` has to inspect requests, because a file capability and a directory capability speak
different protocols and it is translating between them. **`dwarden` performs no rights checks
whatsoever**, and that is the design rather than an omission.

At startup it sends exactly **one** `OPENDIR`, asking for the granted name with the granted rights.
The FS server intersects those with its own and refuses if the intersection came up short, so a
wiring that asked for more than exists dies at the warden's first request instead of coming up
serving a capability nobody meant to hand out. Everything the client can reach afterwards, it
reaches *through the handle that request minted*. The attenuation lives in the handle the server
minted, and there is no branch in the warden that could be wrong about it.

What the process actually does is **translate a namespace**. The client numbers its handles in its
own space starting at `fs::ROOT`, which is the granted directory; the warden maps each to the FS
server's number and forwards the request otherwise unchanged. A client that guesses a handle is
guessing in a table with a handful of inhabitants, none of which it chose, and a number the warden
never minted is `EBADF` from one check.

It costs no memory: the granted name and the rights mask ride in the three `START` argument words
(`fs_proto::grant`), and one frame is shared by all three processes.

### One frame, and the startup ordering that argument does not cover

The one-frame argument is `fwarden`'s: every request on both hops is a blocking `CALL`, so the
client is parked inside its own call for the whole time the warden is using the page, and a second
frame would buy a copy and no isolation.

That holds once the warden is **serving**. It does not hold at **startup**, and this cost a debugging
round. The warden stages the granted name in the shared page and then blocks in a `CALL` to the FS
server; a confined program that already exists writes its own first name over that page, and the FS
server resolves whatever it finds there.

In the case that actually failed it is not even a race. When the wiring call is the one that wires
the FS service, the FS server is parked inside its readiness `SEND`, so the warden's descent cannot
be answered until somebody drains it, and the client has that entire window. The warden then died
rather than serve a hole, and its client blocked forever on a call nobody would answer: a userspace
`ebreak` followed by the 60 s lost-wakeup watchdog. It passed on aarch64 and failed on riscv, which
is the shape of a timing bug and was one.

The fix is ordering, not a second page. **Draining the readiness sentinels is sequencing, not only
an assertion**: each server is parked inside its blocking announcement until somebody receives it,
so nothing it serves can be answered first. `fs_service::start_granted_dir` now drains the service,
waits for the warden's own sentinel, and only then spawns the confined program. `fwarden` had the
same latent bug and took the same three lines.

## `RENAME`

`mv`'s verb, and the one that made `REMOVE` real: until it existed nothing on the wire consulted
that rung, and a right nothing enforces is a right the contract is lying about.

**The wire.** The only verb here that names two directories, so it is the only one whose second word
is not a scalar. The source is the request word's handle and length as usual; the destination rides
in the second word packed by `fs::rename_dst` with the same two fields. Both names are in the shared
page, source first, back to back. A pair longer than the page is `EINVAL` rather than a clamp,
because clamping a name renames something else.

**Rights.** `REMOVE` on the source (its name goes away) and `CREATE` on the destination (a name
appears), each refused with `EROFS`. Within one directory a rename needs both on that one. The
rights are checked **before** anything is resolved, so a capability that may not move a name cannot
use the verb to find out whether one is there.

### The two atomicities, stated apart (DECISIONS §42)

§42's point is that saying "atomic" and letting the reader assume the stronger one is the mistake
POSIX made, so:

- **Concurrency-atomic: yes.** No observer sees the state where the name is in both places or
  neither. The reason is structural rather than a lock: the FS server's serve loop runs one request
  to completion before it receives the next, so inside the server there is no concurrent observer at
  all.
- **Crash-atomic: yes, and measured.** The whole rename runs inside one `fs.tx`, which reaches the
  platter through one commit in RedoxFS's header ring. That is a design claim until something cuts
  the power, so the rename is now **the last operation of the workload in
  `fs-server/tests/crash_consistency.rs`**, whose sweep cuts the device at every write the workload
  makes and mounts what is left. A recovery holding the file under both names, or under neither, is
  a state that never existed and fails the sweep. Both names are in that test's `NAMES`, so a
  snapshot reads both and cannot miss either case.

### What is deliberately not offered, and why each refusal is loud

§42's operative rule is **no silent degradation**: a verb that is not offered fails loudly and the
application decides, and what it must never receive is an unsafe operation wearing a safe one's
name.

- **`renameat2`'s `EXCHANGE` and `NOREPLACE`.** They work on ext4, btrfs, XFS, f2fs and tmpfs and
  nowhere else, and emulating `NOREPLACE` with link-then-unlink is racy. Offering them would make
  behaviour backend-specific, which §42 forbids.
- **Cross-filesystem move.** A different verb: copy-then-unlink, a different object with a different
  identity, non-atomic by nature. It cannot be reached through this verb anyway, because both
  handles are minted by one server bound to one image.
- **Moving a *directory* between two directories** is `EINVAL`; a directory renames in place fine.
  POSIX's guard against making a directory its own descendant is a path-prefix test, and this
  contract has no paths to take a prefix of. The equivalent here is an ancestry walk inside the
  server, which is real recursion in a process whose stack is measured at about three quarters used
  (§27). Renaming within one directory cannot create a cycle, because the node's parent does not
  change, and that is exactly where the line is drawn.

### Two checks that are ours rather than the engine's

RedoxFS's `rename_node` passes the *destination's* own type to `remove_node`, so it never compares
the two kinds: it will happily rename a file over a directory. POSIX will not, and §42 says an
offered verb means one thing on every backend, so the kind comparison is done at our boundary: file
over directory is `EISDIR`, directory over file is `ENOTDIR`. A non-empty destination directory is
the engine's `ENOTEMPTY`. Renaming a name onto itself is a successful no-op, which is POSIX's answer
and also the safe one: the alternative removes the only link and then has nothing left to relink.

Both names go through `check_component`, so `..` means nothing here either.

## What the guest tests prove, and from where

Three `#[test_case]`s, one module for both ISAs rather than an aarch64 test with a riscv twin:
nothing in them is architecture-specific, so the parity gate (§19) is met by literally the same test
running twice.

Each wires a `dwarden` holding a capability to the fixture's `sub` with one rights set, and runs the
`ROLE_DIR_ATTACKER` role of `fsclient` against it. The image carries:

```text
  /            motd  scratch  sub/  other/
  /sub         inner  deeper/          <- the granted capability is here
  /sub/deeper  leaf
  /other       secret                  <- never reachable from the grant
```

**The attacker is told nothing about its own grant** beyond a run index, which only keeps the names
it creates distinct across three runs that share one image. It attempts every verb and reports a
**bitmap of what got through**; the kernel test asserts the *exact* expected set for that
configuration. So the specification lives in the test rather than in the program under test, and the
three runs are each other's controls: a warden that refused everything fails the wide run, and one
that allowed everything fails the narrow ones.

| Run | Rights | Expected |
|---|---|---|
| read-only | `DESCEND｜READ｜ENUMERATE` | opened its own file, enumerated, descended |
| full | `ALL` | those three, plus created, wrote, renamed, made a directory |
| append-only | `READ｜WRITE｜CREATE` | opened its own, created, wrote |

The append-only run is the roadmap's motivating sentence made a test, and two rungs in it are the
interesting ones. `DESCEND` withheld while `CREATE` is held means it can make a file and cannot make
a directory. `REMOVE` withheld while `CREATE` is held is "add to this, destroy nothing" exactly: it
creates a name and then cannot move it, through the same code the full run moves it with.

What the attacker attempts, and what makes each attempt real: `motd` is in the granted directory's
**parent**, `other/secret` is in its **sibling**, and both are on the image and one directory entry
from the warden, which could open either on any request it liked. So each refusal is a fact about the
capability rather than about the filesystem. It also tries `..` at every rights setting, asks for a
right its capability does not carry (which must be refused, not quietly narrowed), descends asking
for **nothing** and checks that the resulting capability can do nothing at all, and guesses handle
numbers past anything the warden could have minted.

Two bits exist to stop the whole thing being vacuous. `OPENED_ITS_OWN` is the control: without it,
every refusal above is equally consistent with a warden that answers no to everything or a grant
that reaches nothing. `GRANTED_ACCESS_FAILED` fires when the thing it *should* be able to do did not
work, so a capability that reaches nothing reports itself rather than passing as perfectly confined.
`ENUMERATED_A_STRANGER` catches a listing that contains a name from outside the grant, because a
listing is a rendering of authority and a stranger in one is an escape even though nothing was
opened.

### And from outside the guest entirely

The bitmap is a statement by the thing being tested. The other kind of evidence is a different
process, on the host, with the pinned engine, reading the image the run left behind
(`xtask::redoxfs_subtree_was_confined`). It asserts:

1. every fixture name is still in the image root (a capability granted on `sub` can remove nothing
   above itself, so a missing name is an escape too);
2. **no name of the attacker's making is in the root**, which is the upward escape;
3. its creations **are** in `sub`, which stops claim 2 from being true of a capability that reaches
   nothing, and `sub` holds both a renamed name and an unrenamed one, which is the `REMOVE` rung
   witnessed from out here: one capability moved a name and another, running the same code against
   the same directory, could not;
4. `other/secret` and `sub/inner` read back byte for byte.

A program that broke out and then lied about it would still have left the file on the disk.

## The FS server's stack, measured before and after

§27 records that this server's stack is sized by measurement rather than chosen, after `CREATE` and
`TRUNCATE` added a level of tree recursion and left it **528 bytes short**, which presented as a
mystery 900-second test. So the four verbs this milestone adds are exactly the kind of change that
warrants looking, and `RENAME` most of all: it is `find_node` twice, then `link_node` and
`remove_node`, all in one transaction.

The high-water mark went **down**, by 3,776 bytes on aarch64 and 3,696 on riscv64:

| | aarch64 | riscv64 | of a 397,312-byte grant |
|---|---|---|---|
| the four verbs, before `RENAME` | 135,744 | 135,856 | 34% |
| with `RENAME` | 131,968 | 132,160 | 33% |

**Not attributed**, deliberately. A verb that recurses more cannot make the deepest path shallower,
so this is the compiler laying the serve loop out differently, not `RENAME` being cheap; measuring
the frame it costs would need an instrument aimed at that request rather than a per-boot maximum.
Milestone 37 saw the same shape (an 8 KiB drop it also declined to explain) and §27 records both
numbers rather than a story. What matters for the gate is the headroom, and two thirds of the grant
is free either way, comfortably above the quarter-left floor the test fails under.

## BUGS

Known limitations, next to the feature rather than only in a tracker.

- **`std::fs::rename` is still `Unsupported`.** The verb exists on the wire and in the server; the
  std PAL has not been bound to it. Milestone 55 depends on this (`fruit:posix_rename` in the
  reference Samba config), so it is the next step rather than a permanent gap. Binding it is a change
  to `patches/std-cricker`, which rebuilds the std farm and moves the `hellostd` transcript, and it
  was kept out of this lane deliberately.
- ~~**`UNLINK` does not exist.**~~ Built by the commands lane, along with the unlink/revoke split the
  roadmap argues for: see [shell-navigation.md](shell-navigation.md). `REMOVE` now gates two verbs.
  What that lane found and did not fix: **there is no `RMDIR`**, so `MKDIR` can make a directory this
  contract cannot remove, and **no verb reports what rights a handle carries**, so a program handed a
  directory capability must be told out of band or discover by probing.
- **A directory cannot be moved between directories** (`EINVAL`). The argument is above; it is a real
  restriction against POSIX and it is declared rather than silently approximated.
- **`READDIR`'s cursor is an index, and the directory is re-read per call.** A name added or removed
  between two calls of one enumeration can be seen twice or missed. That is readdir's usual caveat.
  Fixing it means a snapshot per client, and this table is not per client.
- **A client of a dead warden blocks forever**, exactly as a client of a dead FS server does (§27).
  §26's fault endpoint is the mechanism that would turn that into a message a supervisor can act on,
  and wiring the FS service into a supervision tree belongs to milestone 23.
- **`dwarden` holds at most 16 handles per client** (`EMFILE` past that). It has no heap and runs on
  the single stack page `run` maps, so a growable table would need an allocator and a 4 KiB local
  would overflow the stack on the first request. Sixteen is well past what the attacker or any
  `cd`/`ls` sequence needs.
- **A single-name grant is still the directory the name is in**, which is wider than the name. The
  globbing lane closed that for a *pattern* operand (a set warden, `user/src/swarden.rs`, serves only
  the names that matched: see [glob-grant.md](glob-grant.md)) and it is still open for a literal one,
  because a set of exactly one has no wiring behind it today.
- **The rights are not printed by `caps` yet.** §42 says the rights *are* the discovery mechanism for
  what a mount offers, and they are introspectable in principle; nothing renders them at the shell
  today because the interactive boot wires no FS service (§27's amendment records that refusal).

## EXAMPLES

Grant a subtree to a confined program, read-only, and attack it:

```rust
// kernel/src/user.rs, dir_capability_tests
let report = fs_service::start_granted_dir(
    blk_server_image(),
    program("fs_server").unwrap(),
    program("dwarden").unwrap(),
    program("fsclient").unwrap(),
    fs_service::DirGrant {
        name: fs_proto::fixture::tree::SUB,
        rights: dir::DESCEND | dir::READ | dir::ENUMERATE,
        role: 5,  // ROLE_DIR_ATTACKER
        arg: 1,   // the run index
    },
)?;
let [tag, verdict, ..] = sched::ipc_recv(report);
```

Descend and then descend again, from a client, asking for less each time:

```rust
// the granted directory is always fs::ROOT on your own endpoint
put_page(b"deeper");
let child = call(FILE, fs::req(fs::OPENDIR, fs::ROOT, 6), dir::DESCEND | dir::READ).0 as i64;
// `child` can read and descend. It cannot create, remove or enumerate, and asking for
// dir::ALL here would have been EPERM rather than a quietly smaller capability.
```

Move a name inside your grant:

```rust
put_page(b"tmp");           // the source name
put_page_at(3, b"report");  // the destination, immediately after it
let r = call(
    FILE,
    fs::req(fs::RENAME, fs::ROOT, 3),
    fs::rename_dst(fs::ROOT, 6),
).0 as i64;
// 0, or -EROFS if this capability carries no REMOVE (source) or no CREATE (destination).
```

Run the whole thing:

```sh
script/test                         # both ISAs, plus the post-run host confinement check
cargo test -p fs_proto              # the contract and the ladder's arithmetic
cargo test --manifest-path fs-server/Cargo.toml   # the engine side, and the crash sweep
cargo kani -p fs_proto              # attenuation never widens, at any depth
```

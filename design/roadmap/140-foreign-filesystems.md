# 140. Mount a drive this system did not create

**Status: NOT-STARTED.** Minted 2026-08-18 by calef, correcting a question the tree had been asking
too narrowly. Milestone 138's work had turned into "is RedoxFS the right store", and that was the
wrong frame: *"The requirement set isn't just recovery on another computer. It is using a drive
constructed and previously attached to another OS."*

**Gate: NONE.** The first increment needs nothing that does not exist, and the sequencing argument
below says which increment that is.

**In brief.** An operating system mounts media it did not format. This one cannot. `fs_server` is
bound to RedoxFS at the type level, so there is no drive on any desk in this house that nife can
read.

## The requirement, in calef's ordering

His framing, and the reason this is one milestone rather than an argument about stores: **assume we
need all of these some day and we deliver them all eventually.**

| filesystem | why | when |
|---|---|---|
| **RedoxFS** | the backup volume. Vendored, working, and the only store here with crash-consistency evidence (186 injected lying-device cases: 112 recovered, 74 refused, **0 silently wrong**) | **kept**, and this milestone does not touch it |
| **FAT32** | **USB sticks.** The format removable media actually arrives in, so it is the one a person meets first | first |
| **ext2** | Linux-native, simple, and a real interop story for a data volume. **Not a backup volume**: ext2 is ext3 without the journal, and that is the property a backup cannot give up | second |
| **ext4** | what a Linux drive is actually formatted as today | some day |
| **ZFS** | what a serious storage box runs | some day |

**RedoxFS was chosen because it could be vendored**, which let the project skip building a
filesystem and get a working one immediately. That decision stands and this block does not reopen
it. What it says is that a vendored store solved one requirement and the others were never asked.

## One program per filesystem, not one server that speaks five

**Decided in discussion with calef, 2026-08-18**, and it replaces this block's first answer, which
assumed an in-process abstraction and was wrong about where the seam is.

**The client-facing seam already exists, and it is `fs_proto`.** A client holds an *endpoint*, not a
program name. A program with a directory capability cannot tell which process is behind it and has no
reason to care. So a separate program per filesystem costs the client nothing: no API change, no
dispatch, no trait.

**Confinement is the reason, and it is the product.** A FAT32 parser reading a stranger's USB stick
is the most attacker-adjacent code this system would run: a large parser over bytes from an unknown
machine. In one server it shares an address space with the backup volume, and a bug in it reaches the
family's backups. As its own program it holds one block-device capability, cannot name the backup
volume, and has nothing to escalate to. `caps fat32_server` prints that difference, which is the
demonstration rather than an assurance.

**It is also what this tree already does.** `net_stack`, `smb_server` and `sub_server_supervisor` are
separate confined programs, `fs_server` already ships three binaries, and one of them is
`second_mount`, a second FS-server process against the same block server. A single server speaking
five formats would be the exception here, not the pattern.

**And `Server<D: Disk>` is already generic on the wrong axis for this.** It abstracts the layer
*below* (the disk). Making it also generic over the layer above is the move to avoid, and §4's
refusal of speculative trait-ification is the rule that says so.

**So what falls out of building the second filesystem is a library, not a trait**: the serve loop,
the shared-page plumbing, the `fs_proto` decode, the parts that are not filesystem-specific. That is
§94's question (what must be per-binary, and what can be lifted) rather than an abstraction over two
stores that disagree about allocation, atomicity, transactions and whether a rename can be atomic.

### `fs_server` is misnamed, and the architecture is what makes it wrong

**calef, 2026-08-18**, on reading the section above: *"Which also means fs_server is misnamed. Its
something like redoxfs_server."*

He is right, and the name was accurate until this decision. A single server for all filesystems is
`fs_server`; **one program per filesystem makes that name a claim the program cannot meet**, and the
next reader would infer that a second filesystem goes inside it, which is exactly the architecture
just refused. §39's rule applies: a name is a claim, and the reader meets it first.

`fs_proto` is **not** misnamed and should not move. It is the protocol every filesystem server
speaks, so a generic name is the true one; that the client cannot tell which server is behind its
endpoint is the whole point.

The rename is mechanical and wide (a crate directory, a package name, three binaries, every
reference), so it wants its own lane rather than riding on prose. Doing it **before** the first FAT32
program is cheaper than after, because after there are two servers whose names disagree about what
kind of thing they are.

### Decided: one process per volume

**calef, 2026-08-18.** Each mounted volume gets its own server process holding exactly one
block-device capability.

Both shapes have one *program* per format, so the installation benefit is the same either way and is
worth stating because it is the strongest of them: **a machine that never mounts FAT32 never installs
`fat32_server`, and the parser is absent rather than confined.** What was decided is how many
*instances* run.

**Why per volume wins, and the deciding argument is not isolation strength.** Per-volume needs no
correctness argument at all. A per-type server holds N block-device capabilities and must get right,
on every request, which volume that request is for; that is the confused-deputy shape a capability
system exists to avoid, and it would have to be argued and tested. Per volume there is only ever one,
so the mistake is **unrepresentable rather than unlikely**, which is rung one of the ladder against a
test.

What it also buys: two USB sticks cannot reach each other, where per-type would put a hostile stick
and an innocent one in one address space. A panic takes down one mount. Unmounting is destroying one
region, which is `Untyped::DESTROY` doing what it already does. And `caps` names exactly one volume,
which is the tightest statement available of what a process may touch.

**The cost, stated plainly:** a process per mount, its memory and its supervision entry. Milestone
129's `--mem` work is already pricing that, and this workload barely feels it, because a home backup
server mounts one volume permanently and a stick occasionally. **The arithmetic changes on a machine
with twenty volumes**, and if that machine ever exists this decision should be re-taken rather than
inherited.

`second_mount` shows the shape already runs: a second FS-server process against the same block
server.

## What each one has to answer

Recorded here so a later lane does not rediscover them per filesystem:

- **Where does a mounted volume appear?** Milestone 47 gave this system absolute paths rooted in the
  caller's own namespace, and §50's `bind` is the shape a second volume would arrive through. A
  mount that becomes ambient is the one outcome this system exists to refuse.
- **What does a read-only mount cost in authority?** A FAT32 stick is attacker-supplied bytes from
  an unknown machine, which is `notes/untrusted-input-audit.md`'s surface with a much larger parser
  behind it.
- **What is the crash-consistency claim per filesystem?** The injector that produced RedoxFS's
  numbers is in the tree and any candidate can be run against it. **A filesystem this system writes
  to needs a measured answer, not an inherited reputation.** FAT32 has no journal and never claimed
  one; that is tolerable on a stick and disqualifying on a backup, and the difference should be
  written where a user meets it rather than assumed.

## BUGS

- **This block names five filesystems and prices none of them.** The ordering is calef's judgement
  about need, not an estimate of effort, and the effort is not known: a read-only FAT32 is small, a
  writable ext4 is not, and ZFS is a different category of undertaking that may never be right to
  own rather than vendor.
- **Nothing here says whether a foreign filesystem is vendored or written.** §46 governs it and the
  answer plausibly differs per filesystem, which is exactly the kind of thing this block should not
  decide in advance of the first one.
- **The seam argument is a prediction.** "Build two and the abstraction falls out" is the tree's
  stated preference and the usual outcome; it is not guaranteed, and a lane that finds FAT32 and
  RedoxFS share nothing worth abstracting should say so rather than manufacturing a trait.

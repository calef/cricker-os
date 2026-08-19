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

## Why the seam is not the first increment

`fs_server/src/lib.rs` reads `use redoxfs::{Disk, FileSystem, Node, Transaction, TreePtr}` and there
is no trait, no `dyn`, and no seam. The obvious move is to build one first. **That is the wrong
order here**, and `DECISIONS.md` §4 says why in general terms: *we are deliberately not speculatively
trait-ifying every subsystem, because that builds the wrong abstraction before the requirements are
known.*

A seam designed against one implementation is a description of that implementation. **Build the
second filesystem concretely, and let the seam fall out of having two**, which is the only way to
find out what they actually share. FAT32 and RedoxFS disagree about almost everything that matters:
allocation, atomicity, the existence of a transaction, what a directory entry is, whether a rename
can be atomic. A seam guessed before meeting that is a seam that will be rewritten.

So the first increment is **FAT32, read-only, mounted beside RedoxFS rather than through an
abstraction**, and the second is the seam that the two of them together make obvious.

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

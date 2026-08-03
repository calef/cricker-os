# Haiku: BFS attributes and package activation, as prior art

Two mechanisms from Haiku (the open-source BeOS reimplementation) that bear on questions this
project has open: **BFS's typed, indexed attributes** for milestone 57's extended-attribute layer,
and **`packagefs`'s activation model** for milestone 39's distribution question.

Recorded in `design/` rather than `notes/` because both inform decisions **not yet made**. Neither
system is capability-based, so **the mechanisms transfer and the authority reasoning does not**. Take
the shapes, not the security model.

## What Haiku is, briefly, and one caveat about it

An open-source reimplementation of BeOS, begun in 2001 as OpenBeOS, renamed 2004, in beta since 2018
and currently at R1/beta5. It is a **hybrid kernel** derived from NewOS, with filesystems, drivers
and the network stack in kernel space, a C++ API (the "Be Kits"), and pervasive multithreading. So it
is not architectural prior art for a capability microkernel; the two mechanisms below are.

The caveat worth carrying: **twenty-five years and no R1 final.** That is a data point about the cost
of reimplementing a complete, coherent system, and it is worth remembering whenever a milestone here
starts to sound like "and then we write the rest of an operating system".

## BFS: attributes as a first-class, typed, indexed thing

Designed by Dominic Giampaolo at Be. The lineage matters: **he later went to Apple**, where this idea
resurfaces as Spotlight, and he worked on HFS+ and APFS. So BFS's indexed attributes are not a dead
end; they are the ancestor of a shipping mainstream feature.

The design, as distinct from POSIX xattrs:

- 64-bit, metadata-journaled, B+trees for directories and indexes.
- **Attributes are typed** (string, int32, int64, float, double, raw) rather than opaque byte
  blobs.
- **Attributes are indexed**, and the filesystem supports **live queries** over those indexes: a
  query is a first-class object that updates as files change, not a directory walk.
- Small attributes live inline in the inode; larger ones overflow into an attribute directory.
- The consequence BeOS actually shipped: the mail and contacts applications stored each record **as a
  file with attributes**, and "show me all mail from X" was a filesystem query rather than an
  application database.

Reference: Giampaolo, *Practical File System Design with the Be File System* (1999), freely available.

### What this means for milestone 57

The fork it exposes: **opaque blobs or typed and indexed?** SMB needs POSIX-style opaque attributes
(Samba's `streams_xattr` stores Apple metadata as byte strings), so that is what must be built. BFS is
the evidence that the ambitious version is buildable, and knowing it exists should stop us designing
an attribute layer that **forecloses** indexing later, even though we are not building indexing now.

**The structural link worth recording**, because it is not obvious: a BFS query returns **a set of
files**. Milestone 47 already decided that a set of files is what globbing produces, and that the
way to grant one is an `fs_file_caretaker` attenuated to a **name set**. So a query result and a
glob result are the same object, and both are candidates for the same attenuation mechanism. If
attributes ever become queryable here, the granting story is already designed.

**What does not transfer.** BFS's indexes are in-kernel; ours would live in the FS server or a layer
above it, which is the microkernel version and fine. And indexing costs write amplification, which
matters more on flash and on a backup workload that writes far more than it queries.

## `packagefs`: installation as a composed view, not a mutation

Haiku's package management, from roughly 2013. The mechanism:

- A `.hpkg` is a compressed archive with a custom format.
- **Packages are activated, not installed.** Dropping the file into `/system/packages` (or
  `~/config/packages`) causes `packagefs`, a virtual filesystem, to present its contents merged into
  the directory hierarchy.
- **System directories are therefore read-only**, and their contents are the *union of activated
  packages* rather than an accumulation of whatever installers wrote.
- Activation and deactivation are **transactional**, with a boot-into-previous-state recovery path.
- `pkgman` is the CLI; dependency solving uses libsolv.

The idea in one line: **the filesystem view is computed from a set of packages, rather than mutated by
installers.**

### What this means for milestone 39

The PATH analysis in milestone 47 concluded that a program namespace *is* an endowment, and that
therefore **installing a program becomes granting it into a namespace**. Haiku arrived somewhere
structurally similar from an entirely different motive: atomic, rollback-able installs rather than
authority, which is the useful kind of convergence: it suggests the shape is right for reasons
beyond our thesis.

**Two things we would do differently, both in our favour.** `packagefs` is a kernel filesystem; ours
would be a userspace composer, which is what a microkernel should do and needs no new kernel surface.
And Haiku is **single-user by design**, so its packaging has no per-user authority story at all; here
a program namespace is per-session and handed out at login (milestone 49), so "activate this package
for this user" is expressible without a special case.

**The honest limit.** Haiku's model gives atomicity and rollback, not confinement: an activated
package's binaries run with the user's full authority exactly as an installed one would. The
composition is a *naming* mechanism there. Here it would be a naming mechanism too: milestone 47 is
explicit that extending a namespace requires already holding the capability, so it is not an authority
increase, and the confinement comes from what a spawned program is granted, not from how it was
installed. Do not let the resemblance suggest Haiku solved a security problem it did not address.

## See also

- Milestone 57 (partitioning, formatting, extended attributes) and milestone 39 (repository structure
  and the road to a distribution), in `design/roadmap/39-repository-structure.md`.
- DECISIONS §34 for why RedoxFS, and its 2026-07-30 amendment for the xattr mechanism.
- Milestone 47's `PATH` and globbing sections for the namespace and name-set arguments this leans on.

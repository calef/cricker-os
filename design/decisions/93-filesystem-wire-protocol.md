# 93. The filesystem wire protocol is ours, and 9P is an adapter at the edge

**Status: DECIDED.**

**Decided 2026-08-17 (calef), from a question rather than from a proposal.** Milestone 54 added
`fs_proto::fs::STATFS` as op 18, and he asked the question nobody in this tree had asked: *"are
these constants some standard? Is there a standard that we can or should adopt instead of creating
our own?"* His ruling on the answer below was `"op 18 seems right. I don't know what else we would
do."`

**The gap this closes is the reason it exists.** `crates/fs_proto` has had a bespoke opcode space
since milestone 32, and the choice to invent one was recorded **nowhere**. The crate's own header
explains the two protocols, the shared-page split and the error boundary, and never why the
numbers are ours. §46 says taking a dependency is a decision rather than a convenience; declining
to take one is the same decision with the same obligation, and this tree had been meeting only half
of it. A reader who wondered had no answer, and the next person to add an opcode would have been
extending a convention with no argument behind it.

## What `fs_proto` is, so the comparison is fair

It is the message protocol between a program and the FS server (§27), not the syscall surface. The
kernel routes these words the way it routes any IPC and never reads an opcode, so adding one is a
change to a crate and a note. `OPEN` is 1, `READ` 2, up through `REMOVEXATTR` at 17 and now
`STATFS` at 18. A handle carries a rights bitfield (`ENUMERATE`, `READ`, `WRITE`, `CREATE`,
`REMOVE`, `DESCEND`, §47) that narrows as it is derived.

## The two candidates

**9P2000** is the serious one, and the honest reading is that it is close to us. Thirteen message
types, Plan 9's protocol, with real exposure: Linux's v9fs, QEMU virtfs, WSL2, Docker Desktop. Its
`fid` is a handle, and `Twalk` **derives a new fid from an existing fid** rather than looking a path
up from an ambient root. That is structurally capability derivation, and milestone 54's own table
already calls 9P "closest to our model."

**FUSE** is the wrong shape, and it is worth saying why plainly rather than dismissing it. It is
keyed on `nodeid`, an integer namespace global within a mount, which is the opposite of a derived
handle. It carries roughly fifty opcodes with Linux semantics attached. Adopting it would import an
ambient-authority design into the one component this project most wants free of one.

## Why we do not adopt 9P, and it is not taste

**The framing is incompatible with the thing that makes this protocol cheap.** §10 is
control-by-message, bulk-by-shared-page: a request is two registers and the payload rides in a 4 KiB
frame already mapped into both address spaces. 9P is a size-prefixed byte stream that assumes
serialization. You can take 9P's framing and lose the shared page, or keep the shared page and no
longer be speaking 9P. There is no version that gets both, so the interop the standard is worth
evaporates at the moment of adoption.

**It is also a weaker authority model than the one we have.** 9P has no per-fid rights; `Topen`
takes a mode and that is the whole of it. Adopting it means extending it on day one, which is the
worst outcome available: the compatibility is gone and someone else's constraints have been
inherited anyway.

Two smaller costs, recorded so the decision is not prettier than it was. Base 9P2000 has no
extended attributes, which milestone 57 already needs, and its error model is a string where ours
is a negated errno.

**The part worth stealing, we already have.** `OPENDIR` plus `DESCEND` is `Twalk`'s derivation
shape. The tree reached it independently, which is evidence for the design rather than an argument
for the protocol.

## Where a standard does belong

**At the edge, as an adapter**, which milestone 54 anticipated when it wrote that 9P and NFSv3
"become optional later adapters rather than prerequisites." SMB is already exactly this: a protocol
a foreign client speaks, adapted onto `fs_proto` by a program holding one directory capability. A 9P
adapter would sit in the same place and buy something real, since QEMU could then serve a host
directory over virtio-9p straight into the guest.

So the rule this decision sets is a boundary rather than a preference: **ours on the inside, theirs
at the edge.** A standard earns its place where a stranger's client is on the other end, and loses
it where both ends are ours and the framing is the thing we designed.

## What this does not decide

**Op-by-op semantics are not recorded here.** They live in `crates/fs_proto` beside the constant
and in notes/fs-server.md, because `fs_proto` is not the syscall surface and a `DECISIONS` section
per opcode would put the contract in two places that can disagree. This section records the choice
of *whose* opcode space, once. `STATFS`'s own four wire choices (a record in the page rather than a
packed word, the record's length serving as its version, the allocation unit as its own field, and
no right required) are in pull request #255 and in the crate.

## BUGS

- **The interop we forgo is real and unmeasured.** Nothing in this tree can be mounted by a v9fs
  client today, and this decision means nothing will be until someone writes the adapter. That cost
  falls on a hypothetical user; it is recorded rather than dismissed because a future one may
  arrive with a real need.
- **The 9P adapter is unbuilt and nothing depends on it.** It is named here as the right shape, not
  as work in progress. Do not cite this section as evidence that it exists.
- **The comparison was made against 9P2000 base**, not 9P2000.L, which does carry extended
  attributes and errno-shaped errors. If the adapter is ever built, .L is the dialect to read
  first, and two of the smaller objections above weaken against it.
- **A foreign component (milestone 36) that expects to speak a filesystem protocol has no story
  here.** The seam question is §31's, and this decision does not answer it.

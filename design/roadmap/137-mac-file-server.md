# 137. The share as a Mac file server, which is not the same workload as the backup target

**Status: NOT-STARTED.** Minted 2026-08-18 by calef, ruling on §99: *option 1 plus a new milestone
for the file-server workload.*

**Gate: DECISION.** The first thing this milestone needs is the choice §99 deferred rather than
refused: where Apple metadata lands, given that this is the workload that actually wants it.
Options 2, 3 and 4 are priced in §99 with their costs measured; the lane that takes this block
should read them before proposing anything new, and the `:` fix below travels with whichever wins.

**In brief.** A Mac using this share the way a person uses a file server: Finder metadata, resource
forks, named streams, and the directory-enumeration extensions that make a Finder window feel like a
local disk. Distinct from milestone 55, which is a Time Machine target.

**Why it matters, and why it is a separate block.** Milestone 55 carried both workloads and nobody
had noticed they were two, which made the metadata work look like a backup prerequisite. §99 showed
it is not: **a Time Machine backup is a sparse bundle, directories and band files, and the metadata a
Mac cares about lives inside the disk image's own filesystem, which this server never sees.** Three
independent sources agree, all read rather than recalled, and the sharpest is `ksmbd`: a whole
in-kernel SMB server used as a Time Machine target whose entire Apple-create-context handling is four
lines and which claims no named streams at all.

So the two workloads have two feature lists, and holding them in one block meant the backup path
appeared blocked on work it never needed. Splitting them is what lets 55 finish.

## What this milestone owns

- **The `FILE_NAMED_STREAMS` bit**, which §99's evidence says is the *only* switch: macOS decides
  everything else from it.
- **Where a stream lands on disk**, from §99's options 2, 3 and 4, and **the on-disk attribute name**,
  which is a thing two programs agree on in the strict sense because `redoxfs_host` and any future
  recovery host read it.
- **The 3 KiB attribute ceiling**, which decides between the options rather than being a detail:
  `AFP_AfpInfo` is 60 bytes and fits; `AFP_Resource` is unbounded and does not.
- **`READ_DIR_ATTR` and the Finder-facing enumeration extensions**, unmeasured in either direction so
  far.

## What it must fix first

**`smb_proto::path` does not refuse `:`.** Harmless under option 1 and a correctness bug the moment
any other option is chosen, because `foo:AFP_Resource:$DATA` would create a literal file with a colon
in its name. §99 records it; whichever lane claims the bit fixes this in the same lane, not after.

## BUGS

- **Nothing here has met a real Mac as a file server.** §99 could not answer whether macOS in practice
  stamps any extended attribute on a share it merely browses, because that needs hardware. Every
  feature list in this block is derived from source and documentation rather than from observation,
  and the first bench session may reorder all of it.
- **The line estimates in §99 are estimates**, anchored on `ksmbd`'s ~200 lines of C. They are good
  enough to rank the options and not good enough to schedule against.
- **This block does not say what "good enough" is.** A Finder window that lists files is not the same
  bar as one that shows the right icons, and nobody has written down which one this is aiming at.

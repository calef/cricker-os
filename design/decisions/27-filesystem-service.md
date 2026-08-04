# 27. The filesystem service: a capability-shaped contract over a component we did not write (milestone 32 phase 2)

**Status: AMENDED.** (one amendment and three superseded accounts below, all kept for the record.)

RedoxFS runs confined as a userspace FS-server component, and its interface is **capability-shaped
from birth**. Three processes, wired by the kernel and named by nobody else: a **block server** (a
role of the virtio driver) that serves blocks over blk IPC with the DMA confinement unchanged; an
**FS server** (`fs_server/`, its own workspace because it links the vendored engine) that runs the
no_std RedoxFS core behind a `Disk` trait over blk IPC and allocates from its own untyped budget
through §22's `GlobalAlloc`; and a **client** that holds only a directory capability. The contract
and both wire protocols live in `crates/fs_proto`, host-tested, the way the terminal contract lives
in `line_editor::proto`. Full design in notes/fs-server.md.

**The contract's rules, which milestone 31 will grant against.** The endpoint a client holds IS the
directory capability: it is bound, in the server, to one directory node, and every name in an `OPEN`
is resolved under that directory. There is no absolute path, no `..`, no global namespace; a client
without the endpoint can open nothing, and the refusal is "no such capability", not a permission
check. A handle is a server-minted token, validated against the session's table in one place, so
forging one is meaningless. Open-by-path exists only inside the server. None of this adds a syscall:
the kernel routes these words the way it routes any IPC (§10, §12) and never reads an opcode, so
adding a method is a change to `fs_proto` and the note, not to the surface (the §16 discipline).

**The error boundary is mapped exactly once.** RedoxFS's error type (`syscall::error::Error`) rides
unmapped through the sans-IO core and the `Disk` impl; the serve loop is the single site that turns
it into the wire's negated errno (`fs_proto::reply_err`). There is no ABI type below the boundary to
leak, which is what makes the rule enforceable rather than aspirational.

**The block server moves a whole block per request, and waits on the interrupt.** RedoxFS scans a
256-entry header ring at mount, so an open is hundreds of reads. The block server moves a whole
4096-byte block per virtio request (its DMA region's second page IS the FS server's block page, so
the device DMAs straight in, no copy), which is what keeps the mount's request count in the low
hundreds and affordable. It then **WAITs on the device's completion interrupt**, the milestone-9
driver discipline, and lets `used.idx` decide when a wakeup is really its own.

*This paragraph is a correction* (fix/irq-delivery, 2026-07-29). It used to say the server polled the
used ring deliberately, because "a reschedule per read overran the watchdog". It does not: with the
WAIT path the fs_server test passes on both ISAs at the 4-core SMP boot, all of the mount's
interrupt-driven completions landing well inside the 60 s watchdog. QEMU still completes virtio-blk
synchronously inside `NOTIFY` (notes/dma.md), so the interrupt is already pending when the server
WAITs, and the pending-signal count (§9a) returns that WAIT at once instead of blocking on an event
already over. The machine overruled the note.

The runners also order the two mmio disks with care, because QEMU assigns virtio-mmio slots in
reverse command-line order and the kernel enumerates by ascending slot.

**Creation stays host-side, always.** The std-gated core APIs are exactly creation (uuid, getrandom);
the server only ever opens an image, so entropy never becomes a userspace dependency. Test images are
made by `tools/redoxfs_host` with the same pinned engine (roadmap §32 item 4).

**Proven.** The read path is proven end to end on both ISAs (the §19 gate): a host-made image,
mounted by the confined FS server over blk IPC, its `motd` opened through a granted directory
capability and read back byte for byte, plus a host-tool consistency check after the run. The sans-IO
core is host-tested for read AND write (`fs_server` lib), so the filesystem logic is proven both ways
independently of any device.

**Amendment (2026-07-29, corrected four times in one day; THIS paragraph is the settled account, and
it is the one that explains the other three). There was never a filesystem bug. The write always
succeeded.** Everything below this paragraph is superseded and kept only because how this fact
wobbled is worth more than the fact.

The cause is **the missing `TRUNCATE` verb meeting a whole-file comparison.** A write shorter than
the file does not truncate it. One boot's FS client left a **64-byte** payload in `scratch`; the next
boot's `std::fs` test wrote its **61-byte** pattern, asserted the whole file equalled it, got 64
bytes back (61 new plus the old three-byte tail), and panicked *inside its write block*. That panic,
read as "the server refused the write," is the entire bug. No allocator loop, no heap exhaustion, no
accumulated mount state, no device-only defect, and no error reply, which is why nobody ever found
the errno: **there was none to find.**

That also explains why three investigations produced three incompatible answers while every one of
them reported honestly. The symptom depended on what the *previous boot's* client happened to leave
behind, and that changed as the client changed, so each round measured a genuinely different thing.
Two lessons worth carrying off, because neither is about filesystems:

- **An order-coupled gate manufactures facts.** `mkredoxfs` ran once for both ISA legs and the
  aarch64 leg mutates the image, so whichever leg ran second failed and neither was reproducible
  alone. Each leg now regenerates its own fixture, and `CRICKER_KEEP_REDOXFS=1` makes the cross-boot
  case *deliberate* rather than an accident of ordering.
- **A test that asserts on whole-file equality asserts on history it did not write.** The fix is at
  that layer, not in the engine: the client restores the fixture as its last write, all its payloads
  are one length, and the post-run host check compares content **and length**, so a future client
  leaving a longer file fails the gate instead of corrupting a later boot's assertion. Pinned by a
  millisecond host test carrying the real 64/61/3 byte counts, so if it ever fails, the contract grew
  a verb and that was a decision.

Two hypotheses died by measurement, and both are recorded as dead rather than left looking plausible.
Heap exhaustion and accumulated mount state: the real engine under the FS server's own allocator,
capped identically, image in a `static` so it stays off the heap exactly as a real disk does, runs 30
mount-and-write cycles with the high-water **flat at 352 KiB**, four percent of the 8 MiB budget, the
cap never once refusing a growth. Raising `FS_BUDGET_PAGES` would have fixed nothing, and a number
chosen to make a test pass would have been a coincidence rather than an argument.

The errno plumbing built to chase this stays, because the reason it was unreadable was real: the
client routed every failed reply through a panicking `check`, so a trapped client told the waiting
test only that something went wrong while the server's reason died with the process. A negative reply
is now sent, carrying the **raw reply word alongside** the decoded errno rather than instead of it,
because the wire's negated errnos overlap the kernel's own `invoke` errors at −1..−8 (the
notes/std.md wart) and a small value is otherwise ambiguous between "the server returned this errno"
and "the IPC itself failed."

*Superseded (2026-07-29), kept for the record.* The previous settled account said there was no
allocator loop but that a second mount of a used image failed its write for an unrelated reason. The
first half was right. The second half named a real symptom and mislocated it: the mount was fine and
the *assertion* was wrong.

Measured on a clean build: the FS client writes the same block **three times in one run** and passes
on both ISAs, and the image afterwards carries the third payload, so the repeat write reached the
disk. A `VERIFY_WRITES` switch that reads every written block back through `IpcDisk` and compares
never fired, so the blk IPC transport is faithful (nothing lost, nothing misdirected, no stale read).
And the observed failure was never a spin: the std program's own `expect` panics, which is what
truncated the transcript that got read as a hang. The "400% CPU looping in
`Transaction::sync_allocator`" reading does not survive a correct build.

What was actually broken: `mkredoxfs` ran **once for both ISA legs**, and the aarch64 leg writes the
image, so the riscv leg mounted an image a previous *boot* had mutated. Whichever leg ran second
failed, and neither leg was reproducible on its own. That is why three separate investigations,
each measuring a differently-broken setup, produced three incompatible answers, and it is worth
naming as a failure mode: **an order-coupled gate manufactures facts.** Each ISA leg now regenerates
the same known-good fixture.

That is determinism, not a fix. A second mount of a *used* image still fails its write, and the
recipe is recorded in notes/fs-server.md (generate once, run one leg, then the other without
regenerating) along with the leading hypothesis: accumulated **mount** state rather than bad data. A
used image carries a higher header generation, a longer allocator log and more live tree blocks, so
the second mount allocates more heap (capped at 8 MiB in `fs_server.rs`, bounded by
`FS_BUDGET_PAGES`) and may reach an allocator squash path a pristine mount never does. The next step
is reading the errno the server returns, which nothing currently surfaces. Note the cost of the fix:
the gate no longer exercises the cross-boot case at all, so this bug is now known-and-untested,
which is the same shape of invisibility that hid it in the first place.

The missing test layer, which should have existed from the start, now does: the EL0 binary's chunking
was extracted into a host-testable `BlockDisk`/`BlockIo`, because chunking that lives only in the EL0
binary is chunking no host test can reach. Ten host tests run in milliseconds (repeat writes,
record-sized writes across the multi-block and compressed-tail paths, and write then drop the mount
with no unmount then reopen and write again) and all pass. That is the decisive comparison: the host
does not loop, so there is no upstream RedoxFS bug and no vendored patch to offer.

*Superseded, kept for the record.* An earlier amendment claimed a first write works and a repeat
write to the same block still loops, reasoning that `mkredoxfs` rewriting the target to a placeholder
made every gated write a first write. The premise about `mkredoxfs` was right and the conclusion was
wrong: the harness was indeed hiding something, but not a loop. This
section used to carry an open item, that an end-to-end write "loops inside RedoxFS's allocator commit
on bare metal even on a pristine image" (the `prev`-chain walk in `Transaction::sync_allocator`). It
does not. Driven through `std::fs` (§22's phase-two amendment), the write completes on both ISAs and
reads back byte for byte when the **host tool reopens the image afterwards** with the pinned engine,
which is the half a cache cannot fake; that reopen is now part of the gate rather than a comment. The
likely cause of the old symptom is the interrupt-delivery fix of the same day (the block server WAITs
on the completion IRQ instead of polling the used ring, the same correction the read path needed);
stated as likely, not proven, because what was measured is that the write completes, not why the poll
path did not. The milestone-32 client stays read-only by choice now rather than by blocker.

**The remaining gap is in the contract: there is no `CREATE` and no `TRUNCATE` verb**, so
`std::fs::write` and `File::create` are honestly `Unsupported` and a write means opening a file the
image already carries. Both verbs are addable (`Transaction::create_node` is not std-gated; "creation
stays host-side" above is about creating a *filesystem*, which needs uuid and getrandom, not a file),
and adding them is a change to `fs_proto`, the FS server, and this section, so it is a decision to
take deliberately rather than a hole to plug. Reported up, with the reply-space overlap noted in
notes/std.md (the wire's negated errnos collide with the kernel's invoke errors, -1..-8). See
notes/fs-server.md.

## Amendment (milestone 31 phase 2, 2026-07-30): `CREATE` and `TRUNCATE` exist, and a per-file grant is a caretaker process

**The two verbs are built.** `CREATE` (opcode 6) resolves a name under the bound directory and makes
it, answering `EEXIST` if it is already there and modifying nothing: create is create, not
create-or-open, because a caller that wants either has to say which it got, and the alternative is
what makes a partly-working write read as a working one. `TRUNCATE` (opcode 7) sets a file's size in
**both** directions, growing with zeroes and shrinking by discarding, with the new size in the second
word rather than the length field (the length field is clamped to one page, which would silently cap
a truncate at 4096 bytes). Both are host-tested in the sans-IO core and bound in `std::fs`, so
`File::create` and `std::fs::write` work rather than returning `Unsupported` (§22's amendment).

**One gap closed while adding them, and it was ours rather than RedoxFS's.** RedoxFS's `check_name`
rejects `:`, over-long names, and duplicates; `/`, `.` and `..` pass straight through. Nothing walked
paths, so nothing escaped, which made the "one component, no `..`" rule true by the absence of a
walker rather than by a check. `CREATE` turned that from a latent oddity into something a client
could *write*: `create_file("../escape")` made an entry literally named that. `check_component` now
enforces the rule at our boundary, deliberately there and not patched into the vendored engine,
because it is a rule of this contract and not a bug in a component whose callers may name entries
whatever they like.

**A per-file grant is a separate process, and that is the decision.** Milestone 31's `run wc
report.txt` must hand over one file; the unit of authority here is a directory. The narrowing is
`user/src/fs_file_caretaker.rs`, a **caretaker** (Mark Miller's term): it holds the directory
capability, opens the granted name once at startup, and serves the same `fs_proto::fs` contract on
its own endpoint with a namespace of exactly one name. Three rules, each phrased as a fact about
what the holder has rather than as a permission refusal, because there is no policy here to consult:

- `OPEN` of any other name is `ENOENT`. In this scope there is no such name. The holder cannot
  enumerate and cannot learn what else the directory holds.
- `CREATE` is `ENOTDIR`. A file capability is not a directory, so "make a name in it" is not a
  request that means anything.
- `WRITE` and `TRUNCATE` are `EROFS` without the write direction. `EACCES` was rejected on purpose:
  it implies a policy that could have said yes.

**Why a process and not a check inside the FS server.** The server receives on one endpoint. Serving
a second, narrower one would need a receive over a *set* of endpoints, which this kernel does not
offer; adding it means giving endpoint capabilities a **badge** (seL4's answer), which is a design
fork and is recorded here as the alternative rather than taken. The caretaker needs nothing new: it
is an ordinary FS client above and an ordinary FS server below. And it is the stronger form of the
claim. The confined program holds an endpoint to the caretaker and nothing that names the FS server,
so "it cannot reach a second file" is a property of its cspace rather than of a branch it is trusted
to take. The boundary is an address space, which is the same reason §31's checker lives outside the
component it checks.

The grant costs no memory: the name and direction ride in the caretaker's three `START` argument
words (`fs_proto::grant`, 16 bytes of name), and the one frame is shared by all three processes,
which is sound because every request on both hops is a blocking `CALL`, so the client is parked
inside its own call for the whole time the caretaker is using the page.

**Proven on both ISAs by an attacker, twice, and the second run is what makes the first mean
anything.** The attacker reports a bitmap of what got through rather than a pass. Read-only: every
bit clear, against a neighbouring file that really exists and that the caretaker really could open.
Read/write, same shape: the two write bits **set** and everything else clear. A caretaker that
refused every request passes the first test and fails the second. Each accepted write is read
straight back, because "the server accepted my write" and "my write landed" are different claims.

**The interactive shell still refuses a named file, and that refusal is true rather than pending.**
(It was spelled `file:NAME` when this was written; milestone 47 removed the designator, and a bare
token in a file position designates the file now. The mechanism is unchanged.) The
boot that starts the shell wires no FS service, so the shell holds no directory to narrow, and `caps`
says so in those words. `grant_plan` carries the whole vocabulary (a `FileSpec` in the manifest, a
`FileGrant` in the endowment, refusals both ways) and the decision is a function of what the shell
*holds*, not of the calendar; phase 1 hardcoded that refusal, which was true when written and would
have quietly become a lie. Wiring an FS service into the interactive boot is the remaining step, and
nothing in the suite gates that boot, which is why it is recorded here instead of built.

**Also settled, by measurement: the FS server's stack.** RedoxFS recurses in 8 KiB frames, and the 33
pages it had were **528 bytes short** once `CREATE` and `TRUNCATE` added a level of tree recursion.
The server died mid-request and its client blocked forever on a `CALL` nobody would answer. The size
is now measured rather than chosen: the kernel poisons every FS-server stack page and
`fs_service::fs_stack_used` reports the deepest word that is no longer poison (135,696 bytes on
aarch64, 135,824 on riscv64, of a 397,312-byte grant), with a test on both ISAs that prints it every
run and fails under a quarter left. notes/fs-server.md carries the incident, including the two
instruments it blinded and why a ceiling failure reports the ceiling and not the cost. *Milestone 37
measures 127,408 and 127,536 for the same grant, 8 KiB lower, and does not attribute the drop; both
numbers and the reasoning are in the note. It also widened the instrument to a maximum over every FS
server a boot starts, so the mount that recovers a crashed disk is measured too, which is the case
most likely to recurse further than a clean one.*

**The remaining honest gap: a client of a dead server blocks forever.** §26's fault endpoint is the
mechanism that would turn that into a message a supervisor can act on, and wiring the FS service into
a supervision tree belongs to milestone 23.

**The missing `TRUNCATE` is no longer only a missing feature; it is a sharp edge that cost a day.**
The four-times-corrected amendment above traces to exactly this gap: a short write leaves the old
tail, so a caller that reasonably expects `write` to replace a file's contents gets a longer file than
it wrote. `std::fs::write` reporting `Unsupported` is honest, but the *partial* capability underneath
it is the trap, because a write that half-works reads as a write that failed. Adding `TRUNCATE` would
remove the edge rather than merely add a verb, which is the strongest argument yet for taking the
decision, and it belongs with `CREATE` in milestone 31 phase 2.

# Frame capabilities: shared memory a process owns

DECISIONS §10 has a one-line rule for the data path: **IPC carries control, shared memory carries
data.** The endpoint moves the small stuff (a length, a request code) and the bulk bytes live in a
page both parties can see, so the kernel never copies them. For a long time nife honored that
rule only by accident of setup: the kernel allocated the shared page and mapped it into both the
console client and server at spawn, and both sides just found it at a fixed virtual address they had
agreed on in advance. The sharing was real but frozen. Two processes could share memory only if the
kernel decided, at the moment it created them, that they should.

A `Frame` capability makes shared memory a thing processes *do* instead of a thing the kernel
*pre-arranges*. This note is that object.

## What a Frame is

A capability whose object is a single physical page. Its address is its identity: `Object::Frame(pa)`
names the page at `pa`, and a process can never forge one, because the only ways to hold a `Frame`
are to retype it out of your own untyped or be handed it by someone who has it, and both keep the
object intact. Its rights say what you may do with the page: `READ` to map it read-only, `WRITE` to
map it read/write, `GRANT` to pass it on.

## Retype, then map: two operations, not one

seL4 splits "get a page" from "put it in your address space," and so do we, because the split is what
makes a page a first-class, delegatable object rather than something that only exists mapped.

- `Untyped::RETYPE` carves one page out of the caller's untyped and mints a `Frame` capability for
  it, full rights, into the caller's cspace. Nothing is mapped. The caller now *holds a page* and
  can map it, or delegate it, or delegate it and never map it.
- `Frame::MAP(va, writable, untyped_slot)` maps the page at `va`. A read/write mapping needs `WRITE`
  on the frame; a read-only one needs `READ`. The page tables to reach `va` are drawn from the
  untyped named by `untyped_slot`, so like everything a process spends, mapping a frame comes out of
  its own budget and the **kernel allocates nothing**.

Contrast `Untyped::MAP`, which does both at once (retype a page and map it writable). That is the
convenient path for a process's private memory. `RETYPE` + `MAP` is the path when the page is going
to be shared, because between the two steps is where the delegation happens.

## Sharing is delegation applied to memory

Because a `Frame` is an ordinary capability, it travels over an endpoint with `SEND_CAP`, and the
rights narrow on the way exactly as they do for any delegation (see [delegation.md](delegation.md)).
So the whole sharing protocol is:

1. Producer `RETYPE`s a frame, `MAP`s it read/write, writes into it.
2. Producer `SEND_CAP`s the frame to the consumer, narrowed to `READ` (dropping `WRITE` and `GRANT`).
3. Consumer `RECV_CAP`s it, `MAP`s the *same physical page* read-only, and reads what the producer
   wrote.

The kernel copied nothing and was never told these two processes would share memory. They built the
sharing themselves out of a capability, and the read-only narrowing means the consumer can look and
not touch. A peer handed `READ` alone gets `NotPermitted` if it asks to map the page writable, which
the test checks by trying.

## The lifetime question, and why there is no double-free

A page shared into two address spaces cannot be owned by either, or the first one to die frees memory
the other is still using. nife sidesteps this cleanly because of how teardown already works: an
`AddressSpace` frees only the frames it recorded at spawn (`self.frames`) plus its page tables and
root. A page mapped at *runtime*, by `Untyped::MAP` or `Frame::MAP`, is never in that list, so
teardown does not free it. A frame's page (and the page tables that map it) belong to the untyped
region they came from, and are reclaimed only when that region is destroyed, wholesale, the way
untyped memory always is. So when the producer exits, its mapping of the shared page simply goes away
with its address space; the physical page persists, and the consumer's mapping is still good. No
refcount, no double-free, because address spaces borrow frames and never own them.

The honest limit: individual frames are not reclaimed on their own, only with their whole untyped
region. That is the same bounded, deliberate gap untyped memory already has, and closing it is the
same parked problem: capability revocation.

## The synchronization edge is the IPC rendezvous

On ARM's weak memory model, the producer's write is not automatically visible to the consumer just
because it happened first in time. What makes it visible is that the delegation is a *rendezvous*:
the producer's `SEND_CAP` releases the scheduler lock and the consumer's `RECV_CAP` acquires it, and
that release/acquire pair is the happens-before edge. The write lands before the send, the send
synchronizes with the receive, the read comes after. So the same IPC that carries the capability also
orders the memory, which is a tidy demonstration of why "control travels by IPC" and "data travels by
shared memory" fit together rather than being two unrelated rules.

## What the test proves

`a_frame_capability_shares_a_page_and_a_read_only_view_cannot_write_it` runs the protocol above with
two user processes and checks two things: the consumer reads the producer's sentinel through its own
mapping (the page is genuinely shared), and a writable mapping of the read-only view is refused (the
rights confine it). The sharing half is self-verifying in a nice way: `RETYPE` hands back a *zeroed*
page, so if the consumer had somehow mapped a different page instead of the shared one, it would read
zero, not the sentinel. Reading the sentinel can only mean it mapped the producer's page. And verified
it can fail: stub the `WRITE` check in `Frame::MAP` and the read-only view becomes writable, so the
confinement assertion trips.

## Three ways a page gets into an address space, and one of them is invisible

This is the finding milestone 108 turned up, and it is the reason that milestone existed. There
were, and are, three routes:

| Route | Who calls it | In the mapping database? |
|---|---|---|
| `Frame::MAP` | a process, for a frame it holds | **yes** |
| `Aspace::MAP_INTO` | a userspace loader, into a space it is building | **yes** (`user_aspace_map`) |
| `Spawn::maps` | the kernel, before the process's first instruction | **no** |

The first two go through `revoke::record_mapping`, and an unrecordable mapping is refused rather
than made, at the mapper's own expense. The third is `AddressSpace::map_physical`, which maps and
returns; there is nothing to record it against, because the process does not exist yet.

So a page delivered by `Spawn::maps` **cannot be revoked**. `Frame::REVOKE` deletes every capability
naming the page (there is none) and unmaps it from every space that recorded it (this one did not),
and the holder's mapping survives untouched. That is not a bug in `revoke`; it is the honest
consequence of a mapping that no capability ever stood behind. **A spawn-time mapping is permanent by
construction.**

It also cannot be narrowed by anyone downstream, because the kernel picked the permissions at spawn
and there is no object to attenuate, and it cannot be handed on, because there is nothing to hand.

## The migration (milestone 108)

The disk and display paths now hold their pages as `Frame` capabilities and map them themselves.
Each migrated program gained two things in its cspace: the frames, and an **untyped** to draw the
page tables from, because `Frame::MAP` retypes intermediate tables out of a region the caller names
and the kernel allocates nothing.

Migrated: `disk_surveyor` (the block-shared page and the roster), the roster probe, `disk_partitioner`,
`mkfs`, the virtio-gpu driver (its whole DMA region), `painter` (the surface), and `display_terminal`
in its whole-screen mode (the surface and the application's output page).

**The stack is the floor.** It is still a `Spawn::maps` entry and has to be: a program cannot map its
own stack, because it needs a stack before it can make the syscall that would map one. Everything
else a process touches can be a capability; that one page cannot.

### EXAMPLES

The wiring side, from `kernel/src/user/disk_service.rs`. The roster goes in with `READ` and nothing
else, and the program is handed a budget to reach it with:

```rust
crate::sched::grant_at(SURVEY_SLOT_BUDGET, untyped_cap(budget))?;
crate::sched::grant_at(SURVEY_SLOT_ROSTER, frame_cap(roster_phys, Rights::READ))?;
run(surveyor_image, Spawn { arg0: ROLE_SURVEY, grants: &[], maps: &stack, .. })
```

The program side, from `user/src/disk_surveyor.rs`. It picks its own address, because it owns the
page now and the kernel has no opinion:

```rust
if !user_rt::map_frame(ROSTER_FRAME, ROSTER_VA, /* writable */ false, BUDGET) {
    user_rt::exit()
}
```

And the negative control, which gained a rung it could not have had before. Under the old wiring the
only boundary was the page permission, so the only thing the probe could do wrong was write. Now it
cannot even obtain a writable window:

```rust
// Rung one: refused by the rights on the capability, before a page-table entry is written.
let rw_refused = !user_rt::map_frame(ROSTER_FRAME, ROSTER_VA, true, BUDGET);
// Rung two: the read-only mapping we are entitled to, and a write through it. This faults.
user_rt::map_frame(ROSTER_FRAME, ROSTER_VA, false, BUDGET);
unsafe { core::ptr::write_volatile(ROSTER_VA as *mut u64, 0) };
```

### What the migration proves that the object alone did not

`disk_tests::the_roster_can_be_revoked_out_from_under_its_holder`. A program maps the roster frame,
reads its first word and reports it, and parks. The kernel checks that word against its own read of
the same physical page through the direct map (so the mapping was real, and was of *that* page),
revokes the frame, and lets the program go. The second read faults, at the address it faults at.

**Verified it can fail**, which is the point of writing it: put the roster back as a `Spawn::maps`
entry and the test trips its own assertion, "a program read a frame that had been revoked out from
under it, at 0x50010000, and was NOT stopped: the mapping outlived the capability". That was the
state of the world for every driver in the tree the day before.

## The frame budget: what a test boot spends, and what it gets back

This section is the receipt for the failure that has misled three milestones, and it is written as a
budget rather than as a story because the number is the point.

### The symptom, and why it always accused the wrong test

The aarch64 test boot failed as `Unmappable(OutOfFrames)` about **one run in three** (measured
2026-08-16). It failed in whatever test happened to spawn last, which was never the test that spent
the memory. It also failed in disguise: milestone 107 met it as `time_tests` reporting *"no swish
program in the initrd archive, or no memory to wire one"*, which reads like a packaging bug and is
not one. `notes/net.md` had recorded it eight separate times as "`virtio::MAX_DEVICES` has asked for
reclamation again".

**Two numbers, not one, and the second is the one that fails a boot.** Free frames and the longest
*run* of free frames are different questions, and `alloc_contiguous` asks the second. Milestone 107
measured **137 frames free and no run of 128** at the failing allocation and read it as exhaustion.
Both readings now exist: `memory::free_frames()` and `memory::largest_free_run()`
(`FrameAllocator::largest_free_run`, host-tested in `crates/frames/tests/allocator.rs`).

### The instrument: the frame ledger

`kernel/src/testing.rs` reads free frames **once per test, at the top**, so the readings *partition*
the run: what a test is charged is the drop between its own reading and the next one. That shape is
not fussiness. Reading before and after the test body instead charges only what the test spent while
it was running, and a test that spawns a service and returns the moment it has its report leaves that
service still mapping its heap: on the first measured boot, before-and-after attribution left
**17362 of 29091 frames** landing nowhere at all.

What a run prints:

```text
test kernel::user::dir_capability_tests::a_full_directory_capability_... ... ok
    [that test kept 2284 frames]
...
frames: 29306 free before the first test, 15307 after the last (13999 never returned); longest free run 14080
  the biggest single spender was kernel::user::dir_capability_tests::a_full_directory_... at 2284 frames
```

A charge under `FRAME_REPORT_MIN` (16 frames) is silent, so the transcript names the services and not
the arithmetic.

**Two ceilings fail the run, and the second is the one that names the bug.** `SUITE_FRAME_BUDGET`
catches the total residue growing past what is accounted for below, which is how a
new service-shaped test that forgets to hand its memory back is caught in the act. `SUITE_MIN_FREE_RUN`
requires the boot to end with a free run of at least 1024 frames, and that is the real gate: loading a
program calls `alloc_contiguous`, so what a boot runs out of is contiguity rather than memory. A suite
can pass a residue ceiling and still be fragmented into uselessness, which is exactly the state this
one was in.

### What the boot spent, measured

| | free before the first test | free after the last | never returned | longest free run |
|---|---|---|---|---|
| before | 29307 | **216** | 29091 | **117** |
| after reclaiming init | 29306 | 12504 | 16802 | 12394 |
| after reclaiming init and the net services | 29306 | **15307** | 13999 | **14080** |

216 free frames and no run longer than 117 is the whole failure. There was no headroom left for
anything, so which test died was decided by scheduling.

The largest single causes, before:

| frames | who | what it was |
|---|---|---|
| 12289 | the six `spawn_init` tests | 2048 frames of building budget each, reserved and never returned |
| 2759 | the ten net tests | a `net_stack`'s 128-frame budget, its stack, and its client's, ten times |
| 2284 | the FS service | one block server and one FS server with an 8 MiB heap budget |
| 2146 | the two `authority_tests` | `root_supervisor`'s 1024-frame budget, twice |
| 1656 | the credential store | a 6 MiB budget sized by Argon2id's scratch |
| 1362 | the crash-recovery FS test | two more FS servers, deliberately killed and never reclaimed |

### The fix, which is wiring and not mechanism

Everything used here is DECISIONS §16 object revocation, already built and already proved. What was
missing was a handle and an ordering.

- **`user::holding::Holding`** (name provisional) remembers what the kernel handed a service: its
  threads, the regions to reclaim while those threads still exist, and the regions that may only be
  reclaimed once they are provably gone. `release` spends `sched::kill_thread` and
  `sched::reclaim_region` on them in that order, retrying because the first `reclaim_region` is what
  *arms* §16's kill on a resident. `release_or_fail` is the form tests use, because an instrument that
  reports success either way is not one.

- **The region's endpoints are now swept before the refusal, not after** (`sched::reap_region_objects`).
  This is the load-bearing half. A blocked thread never reaches `schedule()`, so it never spends the
  armed kill, so a region holding a server parked in `RECV` was refused **forever**:
  `userspace_init_brings_up_the_console_server` builds exactly such a server out of init's budget, and
  its 2048 frames were unreclaimable by construction. Sweeping first fixes it because the wake was
  already there: removing an endpoint drains its wait queues, aborts each waiter's IPC and wakes it,
  which is precisely the transition the doomed resident needs. A refused reclaim was already
  destructive (it arms kills; see `reclaim_region`'s BUGS), so this is the same commitment one object
  over.

- **`spawn_init` carves init's building budget outside the spawned thread** and hands the caller a
  holding over it. The region is unchanged; who can name it is not, and that is the whole difference
  between 8 MiB spent and 8 MiB lent.

- **A service's endpoints come out of a region of its own** (`net_stack`'s do), because that is the
  handle the sweep above needs. From the kernel's shared endpoint chunks there is no such handle and
  no way to end the process short of rebooting.

- **Stack pages are retyped from a region instead of allocated**, and that region is reclaimed only
  after every thread is gone. A `Spawn::maps` page is not a recorded mapping (see "Three ways a page
  gets into an address space" above), so §13's revocation cannot pull it, and freeing a running
  thread's stack is a use-after-free rather than a fault.

### What is still held at the end of a boot, and why

13999 frames, and the difference is accounted rather than shrugged at:

- **The FS service, ~2284.** Wired once (`fs_service::ensure`) and used by every later filesystem
  test. It is a boot service, not a leak.
- **The credential store, 1656.** Same shape: wired once behind a `DONE` flag and shared.
- **`root_supervisor`'s two trees, 2146, and this one is a real limit rather than a choice.**
  `root_supervisor` **`SPLIT`s** the spawner's budget out of its own, and `reclaim_region` refuses a
  region with live children (freeing its whole run would double-free the child's pages). The child is
  destroyed only by *its* owner, which is a process that has been torn down, so the parent can never
  become childless. Reclaiming a split parent whose children's owners are gone is what a capability
  derivation tree buys and this kernel deliberately does not have (notes/object-revocation.md). It
  wants its own lane.
- **The crash-recovery FS servers, 1362**, and the disk, sink, `c_seam` and shell services below them.
  All are the same shape as the two above: reclaimable in principle by giving their spawn helper a
  holding, and each is a small separate change rather than part of this one.
- **One DMA page and one virtio shadow frame per net service, ~20 frames.** Deliberately *not*
  reclaimed. The NIC keeps whatever receive buffers the dead driver posted, and returning those pages
  to the allocator would let a live device write into memory handed to somebody else. Ending that
  safely means resetting the device at teardown, which is a change to the transport seam and its own
  piece of work. Twenty frames is not worth the hazard.
- **A page per kernel endpoint**, carved into chunks by `sched::create_endpoint` and never freed by
  design.

### BUGS in the ledger itself

- **A charge is attributed to the test that was running, not to the code that spent it.** A boot
  service wired lazily by whichever test asks first is charged entirely to that test, which is why
  `a_full_directory_capability_does_everything_inside_and_nothing_outside` appears as the biggest
  spender in the tree while spending almost nothing of its own. Read a large charge as "the service
  this test was first to need", not as an accusation.
- **The two ceilings are set from one measurement each**, so they are as good as that boot was
  representative. A run where the host resolver does not answer the non-gating DNS check spends a
  little less; that variance is inside the headroom both numbers carry, but neither is a tight bound
  and neither should be read as one.
- **The ledger cannot see memory that never reaches the frame allocator.** A region reserved and
  unspent costs exactly as much as one filled to its watermark, which is correct for this failure
  (an untyped is a reservation) and means the ledger says nothing about waste *inside* a budget.

## BUGS

- **A `Frame` names one page, and a DMA region is a run of them.** The virtio-gpu driver's region is
  nine contiguous pages, so it holds **nine capabilities** and issues nine `MAP` calls for memory
  that is adjacent in physics, adjacent in its address space, and covered as a single range by the
  IOMMU domain the kernel programmed for it. That is slots 5 through 13 of a sixteen-slot cspace
  (`cap::CSPACE_SLOTS`), one of which is reserved for the fault endpoint: it fits with **one slot
  spare**, and a wider scanout would not fit at all. `display_service::DRIVER_SLOT_DMA` carries a
  `const` assertion so that someone who widens the surface fails the build rather than the boot.

  The milestone's scope note called this out in advance ("if the migration finds the object short of
  something a real driver needs, that is a finding worth recording, and it is a design fork rather
  than a quiet addition"), so it is recorded and not fixed. **The fork is whether a `Frame` should be
  able to name a run of pages** (seL4 has no answer to copy here: it retypes N frames and you hold N
  capabilities, and its cspaces are radix trees rather than sixteen slots, so the pressure lands
  somewhere else). Growing `CSPACE_SLOTS` is a one-number change paid in TCB size, and is the other
  half of the same question.

- **Not everything migrated.** The console is deliberately last (a bootstrap that needs a capability
  service to print cannot report its own failure, so it is its own decision with its own argument),
  and the compositor path is not in this milestone at all: `display_terminal` therefore maps its own
  frames in `MODE_DISPLAY` and still receives spawn-time mappings in `MODE_WINDOW`. `date` keeps its
  `Spawn::maps` clock page in the kernel's test wiring, and it is the one place in the tree where
  both mechanisms appear in a single spawn literal (a `frame_cap` whose only job is to be probed for
  presence, beside a `Mapping` that does the actual work). Migrating it means touching the shell's
  spawn path and §67's grant manifest, which is a bigger change than the wart. Note that the *shell*
  spawns `date` through `Aspace::MAP_INTO`, which is recorded, so the revocability gap there is in
  the test wiring rather than in the real path.

- **Each migrated program costs one more untyped region.** `untyped::create` takes a contiguous run
  from the frame allocator and holds it for the process's life, and the region table has a finite
  number of slots. Eight pages apiece is negligible next to what these programs already reserve
  (`mkfs` takes 384), and a small contiguous request is easy where milestone 107's 128-page one was
  not, but it is a reservation added to a machine whose frame pool that milestone found at the edge.

- **The mapping records cost the process, not the kernel.** Every `Frame::MAP` writes a record into a
  log page retyped from the *address space's* own backing region (not the untyped named in the call,
  which pays only for page tables). That is 255 records per page and `AS_OVERHEAD` is sixteen pages
  of slack, so nothing here comes close; a program that maps thousands of frames would notice.

- **The suite overflowed a kernel thread stack intermittently on this branch, and the cause was not
  this milestone.** Fixed on `main` before this merged; kept here because the next person to meet a
  fault on this path should meet the whole story, and because "the milestone that surfaced it was not
  the milestone that caused it" is the part that would otherwise be lost.

  One run in five faulted (2026-08-13; four green runs on this branch, one red). **The kernel binary
  was byte-identical between a run that faulted and a run that passed**: the two commits differ only
  in `.github/dependabot.yml`, `.github/workflows/toolchain-bump.yml` and `script/ci-qemu`, with
  nothing under `kernel/`, `crates/`, `user/` or `fs_server/`. So it is depth-dependent rather than
  deterministic, and re-running until green would hide it.

  ```
  ESR_EL1  0x96000047   EC 0x25, data abort taken without a change in EL (so: kernel mode)
                        WnR 1, a write
                        DFSC 0x07, translation fault at level 3
  FAR_EL1  0xffff0010001b3000
  ELR_EL1  0xffff00004012fa34
  x8       0xffff0010001b7a90
  ```

  `FAR` is **exactly the guard page of kernel thread stack slot 87**. `thread::STACK_AREA` is
  `KERNEL_VA_BASE | 0x10_0000_0000` and the per-thread stride is five pages (`STACK_PAGES` = 4 plus
  one guard), so `FAR - STACK_AREA` is `0x1b3000` = 87 × `0x5000` with a remainder of **zero**. `x8`
  is `0x4a90` into the same slot, which is that thread's own stack, 1392 bytes below its top. So the
  guard page did its job: a 16 KiB kernel stack ran out and the write below it was caught rather than
  quietly landing on the neighbour.

  It faulted while the supervision and reap tests were running (the console interleaves, so which
  test owns it is not established, and this note should not pretend otherwise).

  **That last sentence about the stack running out is wrong, and the register above it says so**
  (2026-08-16). A stack 1392 bytes below its top has used 8% of itself, not all of it, and the two
  claims sit in the same paragraph. This exact `FAR` came back twice more, byte for byte, after
  #157 shrank `reap_region_objects` and after milestone 124 rebuilt the spawn path, which no
  depth-driven overflow could do; and `script/stack-depth-check` now says the deepest chain a thread
  stack can carry is 13792 bytes against the 20480 this address would need. See notes/stack.md, "The
  guard-page faults of 2026-08-16, which were not overflows". **The rest of this entry stands**: the
  fault is real, it is not this milestone's, and the binary really was byte-identical between a red
  run and a green one.

  **A correction worth keeping, because the wrong reading was reasonable and cost an hour.** The
  first pass at this decoded `FAR` through `phys_to_virt` (which is `pa | KERNEL_VA_BASE`), read the
  result as physical `0x1b3000` with a stray bit 36, and concluded the pointer was corrupted. Bit 36
  is not corruption: it is `STACK_AREA`, placed 64 GiB up **precisely so that a stack address can
  never collide with the virtual name of a physical one**, which `thread.rs` says in the comment
  above the constant. The lesson is that a high-half address is not automatically a physmap address,
  and masking off `KERNEL_VA_BASE` is not a decode unless you have first established which region
  you are in.

  **What it turned out to be, and why this milestone was the wrong suspect.** The milestone was held
  rather than merged on four green runs out of five, and the investigation went looking for what made
  *this branch's* kernel path deeper. It was not this branch. Measuring per-function frames with
  `-Z emit-stack-sizes` and comparing this milestone's test binary against `main`'s says the largest
  single frame growth in the whole milestone is **128 bytes**; its biggest new frame is one more
  `spawn_on` instantiation, the same size as the eight already there.

  The cause was `sched::reap_region_objects` on `main`, whose frame was 6816 bytes, of which 4096 was
  one `[u64; MAX_ENDPOINTS]` scratch array, against a measured thread-stack headroom of 4712 bytes.
  **That one frame wanted 2104 bytes more than all the headroom there was**, so any chain reaching the
  measured peak and then entering a reap could not fit. This milestone added one more spawned program
  to a margin that was already short, which is why it faulted here first. Fixed on `main`
  (notes/stack-high-water.md), and `script/stack-frames` now fails the build on a frame that size.

  The general lesson is worth more than the bug: **the milestone a fault appears in is not
  necessarily the milestone that caused it**, and on a shared resource as invisible as stack depth,
  the last change to arrive gets blamed for a margin that many changes spent.

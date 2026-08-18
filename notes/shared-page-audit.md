# Auditing the shared pages: time of check to time of use

Done 2026-08-04, as the tree's **second** security audit. The first
([arch-audit.md](arch-audit.md)) read the hand-written architecture assembly and found three bugs in
the class "state staged in single-copy hardware registers across more than one instruction." This
one deliberately does not re-read a line of that, because the value of a second audit is the lens
the first one lacked.

## The lens, and why this one

**Every service contract in this system moves bulk data through a page shared with the client**
(DECISIONS §10: control by message, bulk by shared page). That shape did not exist when the first
audit was written; the compositor's surfaces, the C seam, `std::fs`, `std::net`, the FS service and
the sink contract all arrived afterwards, and the attack surface roughly doubled.

The bug class this audit hunts, stated generally:

> A value that a server **checks** and a value that a server **uses** are two different reads of
> memory that a party other than the server can write in between.

That is the double fetch, and it is invisible to every gate we run, because both the check and the
use are individually correct. `script/verify` proves the pure-logic crates and cannot see across an
address-space boundary. `script/test` runs one thing at a time. Nothing in the tree today can fail
because of a window that only a second runnable writer can enter.

The question asked of every request handler was therefore the arch audit's four, transposed:

- **(a) The window.** Where is the check, where is the use, and what is between them?
- **(b) What can land in it.** Which processes hold a writable mapping of that frame, and is any of
  them runnable at that moment?
- **(c) The corrupted state.** What does the server do with the value it did not check?
- **(d) Reachable?** Is it closed by a mapping, by the blocking-`CALL` rendezvous, or only by the
  fact that nobody has built the wiring that would open it?

(d) is again the interesting one, and again the honest answer for most of the tree is "closed by
something other than the check."

## What was audited

Every place a frame is mapped into more than one address space, and every loop that serves requests
against one.

| Contract | Server | Client(s) | Files |
|---|---|---|---|
| blk IPC | block server | FS server | `crates/fs_proto` (`blk`), `user/src/blk.rs`, `fs_server/src/bin/fs_server.rs` |
| file IPC | FS server | every FS client | `crates/fs_proto` (`fs`, `xattr`), `fs_server/src/bin/fs_server.rs` |
| file IPC, narrowed | the three caretakers | one confined program each | `user/src/fs_file_caretaker.rs`, `fs_subtree_caretaker.rs`, `fs_nameset_caretaker.rs` |
| the sink | `user/src/sink.rs` | a redirected program | `crates/sink_proto` |
| the serial terminal | `user/src/line_editor.rs` | the shell | `crates/line_editor` |
| the console | `user/src/console.rs` | its client | `kernel/src/user/console_service.rs` |
| the display | `user/src/display.rs` | painter, terminal, compositor | `crates/gfx_proto` |
| the compositor | `user/src/compositor.rs` | window clients, the input source | `crates/compositor` |
| the display terminal | `user/src/display_terminal.rs` | an application | `crates/video_terminal` |
| credentials | `user/src/credentialer.rs` | provisioner, verifier | `crates/cred_proto` |
| the wall clock | `kernel/src/user/clock_service.rs` | init, the shell, `date` | `crates/clock_proto` |
| the C seam | `user/src/c_shim.rs` (C) | `user/src/c_confiner.rs` | `crates/c_seam`, `user/c/c_seam.c` |
| the input ring | the compositor | the keyboard driver | `crates/compositor` (`proto::ring`) |
| sockets | `user/src/net_stack.rs` | a client, `std::net`, `ntp` | `crates/socket_proto` |
| the virtio DMA regions | four userspace drivers | the **device** | `user/src/net_transport.rs`, `kbd.rs`, `entropy.rs`, `display.rs` |

The last row is not a process pair and is in the table on purpose: a DMA region is a page one party
writes and another reads, the other party is a device rather than a program, and the question this
audit asks does not care which.

## What was deliberately not examined

Stated because a scope nobody wrote down is a scope nobody can check.

- **The arch and assembly layer.** That is [arch-audit.md](arch-audit.md)'s, and re-reading it would
  be the failure this milestone exists to avoid.
- **Capability lifetime races** between revocation and an in-flight use (generational names,
  `Untyped::DESTROY`, `Endpoint::REAP`). Named in the milestone as a candidate lens and left for a
  later one; it is a different question and mixing it in would have diluted both.
- **The census of `unsafe`.** Also a candidate lens, also a whole audit of its own.
- **The supply chain**, the boot trust root, and the DMA/IOMMU descriptor validator. The last has
  its own machine-checked proofs (`crates/dma_validator`, DECISIONS §30) and reading it by hand
  would add nothing a prover has not already said for every input.
- **The Kani bounds.** Whether a proof's chosen bound is the right bound is
  [verification.md](verification.md)'s question, not this one's.
- **Anything that requires already being init.** SECURITY.md puts it out of scope and this audit
  honours that: init is unverified and privileged by design.

### And three things that moved under this audit

An audit reads a commit, not a project. This one read `main` at `313a055`, and three areas changed
in flight; each is named so the clearance above is not read as covering work it never saw.

- **The inbound socket half** (`LISTEN`/`ACCEPT` with a spawn-time port grant) is **not on `main`**.
  `crates/socket_proto` there stops at `OP_CLOSE` and `net_stack.rs` has no listener. What is
  audited here is the outbound contract only. The `net_transport.rs` finding below applies to both,
  the file being identical across them.
- **`crates/cred_proto` and `user/src/credentialer.rs`** are being substantially rewritten with an
  NTLM path. The clearance recorded below is of the version on `main` and does not transfer.
- **The clock page's seqlock** has a live finding of its own from another lane (see finding 7's last
  paragraph). This audit did not re-derive it and does not claim `clock_proto` is clear; it uses that
  page only as the in-tree precedent for the acquire side.

## The structural fact that saves most of the tree

Worth stating before the findings, because it is the reason there are so few.

**Lengths, offsets, counts, handles, opcodes and rectangles all travel in the IPC register words,
never in the page.** The kernel copies a message's words into its own state at `SEND` time and hands
them to the receiver in registers, so by the time a server sees them they are in memory only that
server can write. `fs_proto::fs::req` packs opcode, handle and a 40-bit length into one word;
`gfx_proto` packs a whole rectangle into four 14-bit fields of one word; `cred_proto` packs two
lengths into one word. There is **no contract in this tree whose length field lives in the shared
page**, which removes the entire classic form of the bug (read a length from the page, bound-check
it, read it again to size the copy) by construction rather than by care.

Three consequences fell out of the sweep and bound the search:

- **Every payload length is clamped to the page at the top of its serve loop** and the clamp is a
  local, not a re-read. `fs_server.rs:246` `let len = fs::req_len(w0).min(BLOCK);` is the pattern,
  and the three caretakers, `line_editor`, `display_terminal` and `sink` all repeat it.
- **The one contract whose decode lives in a host-testable crate is the one with a proof.**
  `cred_proto::read` takes the page and the register word, checks both lengths against their
  maxima, and returns two subslices; `crates/cred_proto/src/lib.rs` carries a harness asserting the
  returned slices' lengths match the word and stay inside the page. Every other contract's decode is
  inlined into a `no_std` serve loop, where neither a host test nor Kani can reach it. That is rule
  7's argument arriving from a new direction.
- **The check-free caretaker is the one with no window.** `fs_subtree_caretaker` performs no name
  check at all (its attenuation lives entirely in the handle the FS server minted for it), and it is
  therefore the only caretaker that cannot have this bug. Its own doc comment argues for that design
  on simplicity grounds; this audit is the security argument for the same choice.

## Findings

### 1. The FS service's one shared frame is mapped read-write into every client the boot ever wired

**(a) The window.** `kernel/src/user/fs_service.rs`'s `ensure()` allocates **one** frame,
`FILE_SHARED`, on the first call and hands that same physical address to every later caller:
`spawn_fs_client`, `start_granted`, `start_granted_dir`, `start_granted_set`, `narrow_dir`,
`start_file_sink`, `start_sink_verify` and `start_std`. Each maps it `Flags::user_data()`
(read-write) at `FILE_VA_CLIENT` (`0x60_0000`), or at `FS_PAGE_STD` for a std program. The FS server
maps the same frame at `FILE_PAGE`.

So the number of processes holding a writable mapping of the file page grows with every FS client a
boot wires, and never shrinks: a caretaker `serve`s forever and never exits.

**(b) What can land in it.** The property that makes one frame sound is stated in
`fs_file_caretaker.rs`'s module comment:

> One frame, three parties, and that is sound because every request on both sides is a blocking
> `CALL`: the client is parked inside its call for the whole time the caretaker is using the page.

That argument is correct **for a chain** (one client, one caretaker, one server) and says nothing
about a **fan-out**. A second FS client is not inside anybody's call; it is runnable, and it holds
the same page read-write.

This is not a hypothetical the audit invented. `fs_service.rs`'s own `wait_for_caretaker` exists
because exactly this went wrong once, at startup, and its comment is the precedent:

> a confined program that already exists writes its own first name over that page, and the FS
> server resolves whatever it finds there.

The fix taken then was **ordering** (drain the handshakes before the client exists), which closes
the startup case and not the steady-state one.

**(c) The corrupted state.** A client `A` sends `OPEN` with the name staged at offset 0 and blocks.
Client `B`, runnable, writes a different name over offset 0. The FS server reads the page after the
message arrives and opens `B`'s name, returning the handle to `A`. `A` now holds a handle to a file
it never named and, if `A` is behind a caretaker, one outside the namespace that caretaker exists to
enforce. The same substitution works on `CREATE`, `UNLINK`, `RMDIR`, `MKDIR`, `OPENDIR` and both
halves of `RENAME`, and on the *data* of a `WRITE`.

**(d) Reachable? Not today, and the reason is the wiring rather than the check.** In the interactive
boot three processes now map the file page, and the audit's first named event has happened:
`crates/system_initializer` grants the shell `(SH_FS_VA, g.fs_page, MAP_RW)`, **keeps its own copy
for the life of the boot** (milestone 31 phase 3, 2026-08-17), and maps it into both the
`fs_subtree_caretaker` and the program behind a directory grant. What still closes the hole is that
those three are never runnable at once on the same page: the shell is parked in `recv` on the
spawned program's stream for the whole time that program exists, the program is inside a blocking
`CALL` whenever the caretaker is forwarding, and the caretaker touches the page exactly once at
startup and then only relays handles. **Init itself never writes it at all**, which is worth stating
because it now holds the capability: it maps the frame into children and does not speak `fs_proto`.
In the kernel test suite several caretaker chains do coexist on the one frame, but each is blocked on
`recv_cap` between tests, and the confined clients `exit()` after reporting.

**The remaining opening is a runnable third party, and one of the two originally named is now gone.**
This note used to say that the day init could build a caretaker per grant, "a runnable shell holding
the page coexists with a caretaker chain using it". Init can, since 2026-08-17, and the coexistence
is real while the *runnability* is not: `swish::spawn` blocks on the child's answer, so the shell has
no instruction to execute between sending the request and reading the result. That is a property of
one function rather than of the model, and it is the thing to check when the shell learns to run a
job in the background. The second event is unchanged: a confined program granted an untyped budget
could retype a second `Tcb` into its own address space and scribble the page from a helper thread
while its main thread is parked in `CALL`; today's confined programs are granted two endpoints and no
budget, so they cannot.

**Disposition: proposed as a milestone.** See "What wants a lane" below. The fix is a frame per
client channel rather than a frame per service, which is a wiring change across `fs_service.rs` and
the four programs that name `FILE_VA_CLIENT`, plus a witness test with two live clients. It is too
large for an audit lane and too specific to leave as prose.

### 2. `fs_nameset_caretaker` checks a name and forwards it without re-staging it

**(a) The window.** `user/src/fs_nameset_caretaker.rs`'s serve loop:

```rust
if filtered && v.takes_name() && v.operand == verb::Operand::Name {
    let mut buf = [0u8; grant::MAX_NAME];
    let n = name_at(0, len, &mut buf);          // read the name OUT of the shared page
    if !nameset::contains(set, &buf[..n]) {     // check it against the granted set
        reply(reply_slot, reply_err(ENOENT));
        continue;
    }
}
...
let r = forward(fs::req(code, server_handle, n), second);  // the FS server reads the page AGAIN
```

The caretaker copies the name into a stack buffer, decides on the copy, and then forwards a request
carrying only the *length*. The FS server does its own read of the same page
(`fs_server.rs:254`, `unsafe { file_page(len) }`) and resolves whatever is there then. Two reads,
one check, and the checked bytes are never written back.

`RENAME` has the same shape twice, and the destination half is the load-bearing one: the caretaker's
own comment says renaming a matched name onto an unmatched one "would destroy a name this capability
was never granted, which is an escape even though nothing was opened."

**(b) What can land in it.** Only another writer of the frame, which is finding 1. The confined
client is parked in its `CALL` and the caretaker is parked in `forward`.

**(c) The corrupted state.** The FS server acts on a name the set filter never saw, and the handle
comes back to the caretaker, which installs it in the client's table. The namespace the capability
designates is no longer the set.

**(d) Reachable? No, for finding 1's reason and no other.** The two are one bug seen from two sides,
and they are recorded separately because they have different fixes and the second is cheap.

**Disposition: fixed in this lane.** The caretaker now writes the checked bytes back into the page
before forwarding, so the bytes the FS server reads are the bytes the filter approved. On the honest
path this is a byte-identical rewrite and the existing glob-grant and directory-capability tests
prove it did not change behaviour.

**And the honest limit, stated where the fix is:** re-staging narrows the window from "the whole
check-to-forward span" to "the caretaker's store until the FS server's load". It does not close it,
because a third writer of the frame can still land in the smaller window. Only finding 1's fix
closes it. This is a hardening with a named residue, not a repair.

`fs_file_caretaker` does **not** need this and cannot have the bug: it answers `OPEN` locally
(comparing the asked name against the granted one and returning `grant::HANDLE`) and never forwards
a name at all. `fs_subtree_caretaker` cannot have it either, for the reason in the section above.

### 3. The console server takes an unbounded byte count from its client

**(a) The window.** `user/src/console.rs`:

```rust
let (len, _, _) = recv(REQUEST);
let shared = SHARED_VA as *const u8;
for i in 0..len {
    let byte = unsafe { core::ptr::read_volatile(shared.add(i as usize)) };
    uart_put(byte);
}
```

`len` is the first register word with no clamp. The shared mapping is exactly one frame
(`kernel/src/user/console_service.rs`, one `alloc()`, mapped `Flags::user_rodata()`).

**(b) What can land in it.** Nothing needs to: this is not a race. Any holder of `WRITE` on the
request endpoint sends `len = u64::MAX`.

**(c) The corrupted state.** The server reads off the end of its own mapping and is killed by the
fault. The console is a shared service, so its death takes out every other client's output too.

**(d) Reachable? Yes, trivially, by any client of this contract.** What bounds the damage is that
this wiring is milestone 19f's test console; the interactive system's terminal is `line_editor`,
which does clamp (`.min(PAGE)` at every one of its four length sites).

The finding worth keeping is not the missing clamp but **the reason recorded next to it**. The
`SAFETY` comment says:

> A malicious length is a read out of our OWN mapping, which faults us, not the kernel: a driver bug
> is a crashed process.

The length is not the driver's; it is the **client's**. The comment classifies a client-triggered
kill of a shared server as a driver bug, and a reader who trusted it would carry that reasoning to a
contract where it is not merely a test program. This is the same failure the arch audit's finding 1
was really about: the code was defensible and the record was wrong.

**Disposition: fixed in this lane.** The count is clamped to the page, matching `line_editor`, and
the comment now says who supplies the length and what the clamp is for.

### 4. Two unchecked arithmetic sites in the compositor's window client

**(a) The windows.** `user/src/window.rs` reads its geometry out of the control page the compositor
publishes:

```rust
let w = rd32(CTL_VA + ctl::WIDTH);
let h = rd32(CTL_VA + ctl::HEIGHT);
if w == 0 || h == 0 || w * h * 4 > compositor::MAX_SURFACE_BYTES { die(E_GEOMETRY); }
```

`w` and `h` are `u32`. `w * h * 4` wraps in a release build, so `w = h = 0x10000` computes `0` and
passes the check the moment before the paint loop writes `2^32` pixels past a two-frame surface. The
check is written to catch exactly this ("a control page claiming more than that describes memory we
were not granted") and the arithmetic is what lets it through.

The second is in the same file's input loop: `let n = line_editor::proto::len(w0);` and then
`(bytes >> (8 * k))` for `k` in `0..n`, with no clamp. `proto::len` is a full 32-bit field, so
`8 * k` passes 63 at `k == 8`. Every other consumer of `OP_BYTES` clamps to 8
(`display_terminal.rs`, `line_editor.rs`); this is the one that does not.

**(b)/(c)/(d) Reachable? No.** Both values are written by the **compositor**, which is the trusted
party in this direction, and it publishes correct geometry and always sends a count of 1. They are
latent, in a test witness program, and they are recorded because a bounds check that its own
arithmetic can defeat is worth naming wherever it appears.

`display_terminal.rs` validates the same geometry by **division** (`w / GLYPH_W <= MAX_COLS`), which
cannot overflow. That is the shape to copy.

**Disposition: fixed in this lane**, both, because each is one line and neither can change behaviour
on any path the tests exercise.

### 5. The compositor composites surfaces whose owners are not blocked

**(a) The window.** `user/src/compositor.rs`'s `serve_frame` iterates every committed window, and
`source(i)` builds a `&'static [u32]` over client `i`'s surface. The invariant claimed next to it is:

> The caller is blocked in `CALL` throughout, which is what makes reading a client's pixels safe
> without a lock: the client that rang cannot be writing while we read.

**(b) What can land in it.** That covers the caller and not the other clients. `paint` reads
**every** window's surface, and the keyboard driver's `COMMIT` rings the doorbell while no window
client is blocked at all. Clients hold `Flags::user_data()` on their own surfaces and control pages.

The damage rectangle has the same shape one level down: four independent `rd32`s of `DAMAGE_X/Y/W/H`,
which a non-calling client can be mid-way through writing, so the compositor can assemble a rectangle
that never existed. The client's `SEQ` fence orders the *client's* stores and cannot stop the
compositor sampling between them on somebody else's `COMMIT`.

**(c)/(d) Reachable, and bounded to tearing.** The slice length is `SCENE[i].pixels()`, a
compile-time constant, and every clip uses constant geometry, so no client-supplied value ever
indexes anything. The observable effect is a half-drawn window or a wrong damage rectangle. It is
the same limit already recorded for a capture client reading a mid-composite screen.

**Disposition: recorded-accepted**, in `notes/compositor.md`'s BUGS section, where a reader meets
the frame protocol. Making it a real guarantee means either compositing only the caller's surface
(which breaks the contract, since a `COMMIT` from the input source must repaint everything) or
double-buffering per client, which is a design decision and not an audit's to take.

**One hardening this audit does recommend and did not take**: the compositor holds
`Flags::user_data()` on every client surface and **never writes one**. Read-only there would make
"the compositor cannot deface a client's window" a mapping rather than a discipline, exactly as
`ROLE_CAPTURE`'s read-only screen already does. It is a one-word change in
`kernel/src/user/compositor_service.rs` with a real behavioural risk if any path does write, so it
belongs with a test that proves the fault, not in an audit's diff.

### 6. Two userspace virtio drivers trust the index the device writes into the used ring

Not a double fetch. It is what the enumeration the lens required turned up: to ask "is this value
checked twice" you must first list every value read from a page a hostile party writes, and two of
those values are not checked at all.

**(a) The window.** `user/src/net_transport.rs`'s `rx_take` and `user/src/kbd.rs`'s drain loop both
take a used-ring element and use its 32-bit `id` as a buffer index:

```rust
let id = r32(RX_USED + 4 + slot * 8) as usize;    // descriptor head = buffer index
let total = r32(RX_USED + 4 + slot * 8 + 4) as u64;
let base = rx_buf(id) + NET_HDR_LEN;              // rx_buf(i) = 0x400 + i * 0x2C0
```

Neither `id` nor `total` is bounded by anything.

**(b) What can land in it.** The **device**, and it needs no race: it simply writes a number. The
used ring lives inside the driver's own single-page DMA region, which the device is entitled to
write; that is what a used ring is for.

**This is not covered by the DMA confinement**, and the reason is worth stating because it is easy
to assume otherwise. `crates/dma_validator` validates the **driver to device** direction: at
`NOTIFY` it checks that every descriptor the device could follow lies inside the granted region, and
copies the validated descriptors into a kernel-private shadow the driver cannot touch afterwards.
Its own module comment says the shadow is what makes the check hold under time-of-check to
time-of-use. All of that is about where the device may **touch**. Nothing anywhere says what the
device may **say** on the way back, and the driver believes it.

**(c) The corrupted state.** `id = 4` is already past the one-page region. `id` near 1.5 million puts
`base` at this process's heap (`user_rt::heap`'s `DEFAULT_BASE`), which is an ordinary `u32`, so the
network driver copies its own heap into a frame and hands it to smoltcp, which may put it on the
wire. `total` unbounded reads past the buffer and asks a 96-page heap for up to 4 GiB. In `kbd.rs`
the same index leaves the region at `id = 462` and returns process memory as a keystroke.

**(d) Reachable? By a device, yes, and by nothing else.** The threat model is the question, and this
project's answer is already on the record: DECISIONS §20, §23 and §30 exist because the device is
**not** trusted, which is why there is an IOMMU and a validator at all. A driver that trusts the
device's own accounting contradicts the thesis those milestones establish. Under QEMU with slirp
nothing lies; on the VisionFive 2, or behind any device the host does not fully own, the assumption
is doing real work and is written down nowhere.

The other two drivers show this is an omission rather than a policy: `entropy.rs` clamps its length
(`.min(POOL_LEN)`), and `display.rs` and `crates/virtio` read only `used.idx` and never the element,
because they use one fixed buffer.

**Disposition: fixed in this lane**, both drivers, failing closed: a completion naming a buffer that
was never posted is consumed and dropped, and a length larger than a buffer is truncated to it. What
that costs is one receive buffer per lie, which is the right trade against a device that has stopped
being a network card.

**And what is not proven, said plainly:** nothing in the suite exercises a lying device, so the
existing tests show the honest path is unchanged and nothing shows the hostile path is now safe. A
harness that can make a virtio device misbehave is a piece of work in its own right and is proposed
below.

### 7. The compositor is the only reader in its subsystem with no acquire side

**(a) The window.** Three writers in this subsystem publish data and then publish an index that
advertises it, each with a fence between and a comment explaining the fence:

- `user/src/window.rs`: writes the damage rectangle, `fence(SeqCst)`, writes `SEQ`. "The sequence
  bump must be visible after the pixels and the rectangle it describes, or the compositor could
  composite a frame we have not finished writing."
- `user/src/display_terminal.rs`: the same, with the same comment.
- `user/src/kbd.rs`'s `ring_publish`: `fence(SeqCst)`, then writes `TAIL`. "The bytes must be visible
  before the tail that advertises them."

`user/src/compositor.rs` is the reader of all three, and it read with `rd32`, a plain
`read_volatile`, with no fence anywhere: `serve_frame` loads `SEQ` and then loads the four damage
fields and (via `paint`) the pixels; `drain_input` loads `TAIL` and then loads the bytes.

**(b) What can land in it.** Nothing needs to. `read_volatile` guarantees the access happens and
guarantees **no ordering at all**, and this kernel runs on aarch64, which is the weakly ordered one
(DECISIONS rule 4). The dependent loads may be satisfied before the load that gates them.

**(c) The corrupted state.** A fresh sequence beside the previous frame's rectangle or pixels; a
fresh tail beside bytes that are not yet there. Exactly the outcome each producer's comment says its
fence prevents.

**(d) Reachable? On real weakly ordered hardware, yes**, and it has never been observed, which is
what a memory-ordering bug looks like right up until it is not. QEMU's TCG does not reorder, and
this system has not yet run on a physical board.

The evidence that this is an oversight rather than a judgement is that **the kernel's own stand-in
for the input ring gets it right.** `kernel/src/user/keyboard_service.rs`'s `take_typed` reads the
tail, then `fence(SeqCst)`, then reads the bytes, with the comment "The tail is published after the
bytes it advertises; read it before them." Two readers of one contract, one fenced.

**Disposition: fixed in this lane**, two `fence(Acquire)`, matching `clock_proto`'s reader, which is
the in-tree precedent for the acquire side of exactly this pattern.

**Relationship to milestone 80.** That lane found the same failure shape in the clock page's
seqlock, on the **writer's** side (the claim is not ordered ahead of the data, and neither `AcqRel`
nor `SeqCst` on the claim fixes it; it needs a release fence). This finding is a different page, a
different pair, and the **reader's** side, so it is fixed here rather than folded into that lane. The
two together say something neither says alone: **this tree writes release-side fences by instinct
and forgets the acquire side**, and a comment explaining a one-sided fence reads exactly like a
comment explaining a correct one. That is a lint's worth of pattern, not a bug's.

## Candidates cleared, and why each is safe

Recorded because "we looked and it is fine" is the other half of an audit, and because each is a
place a future change could break something.

**The FS server's whole request decode.** `len`, `offset`, the handle, the opcode, `RENAME`'s
destination handle and length, and `SETXATTR`'s value length and type code all come from `w0`/`w1`.
`len` is clamped to `BLOCK`; `RENAME` and `SETXATTR` refuse rather than clamp when the two payloads
would overrun the page, which is right (clamping a name renames something else). The bit-packed
fields cannot overflow the additions that check them: `req_len` is masked to 40 bits, `dst_len` to
40, `spec_value_len` to 32, and `len` is already at most 4096, so every sum is far inside `u64`. On a
32-bit target `len + dst_len` would wrap; this kernel has no 32-bit target and, if it ever does,
that sum is the line to revisit.

**`GETXATTR` copies the name to the stack before writing the reply over it**, and says why. That is
this audit's lens applied correctly, in the tree, before the audit existed.

**`cred_proto::read`.** Both lengths from the register word, checked against `MAX_IDENTITY` and
`MAX_SECRET`, fixed offsets, one bound against the page. The returned slices alias the page and their
*contents* can change under the store, but their lengths cannot, and the party that would change them
is the one that wrote the secret in the first place. The credentialer also wipes the request area on
every exit path, which is the right discipline for a page a secret passes through.

**The display driver's `FLUSH`.** The rectangle is four 14-bit fields of `w0`; the second data word
is explicitly discarded. Nothing in that serve loop reads the shared page at all, which makes it
structurally immune. Its doc comment records that reading the rectangle out of the second word was
the first bug caught there.

**The keyboard ring, read by the kernel.** `take_typed` reads `tail` once into a local, fences, and
indexes with `head % ring::CAPACITY`, so a hostile `tail` cannot move the index out of the frame.
`head` is the reader's own field, kept in the reader's memory.

**The compositor's damage handling, considered as a double fetch.** The four fields are read once
into a `Rect` local, and both the intersect and the blit use that local. This is the exact place a
double fetch would live and it is not there. (Its problem is finding 5's, which is a different
question: whether the single read is of a consistent value.)

**`line_editor`.** Four length sites, four clamps, and `copy_in` is called with
`offset + dst.len() <= PAGE` at every one.

**The C seam.** `c_seam_transform` does `strlen` and then `memcpy` of `n + 1` bytes, which is
literally a double fetch, and it is harmless: the copy's size comes from the first read and the
buffer is allocated to it, so a changed page changes the bytes and not the bounds. The one thing that
would bite (an input area with no NUL, making `strlen` run off the mapping) cannot happen: the
confiner writes the input and the frame is zeroed at wiring. The Rust side reads only fixed-size
fields and compares against a constant.

The seam also turned out to be **better than its reputation**. `crates/c_seam` now parses the
`#define`s out of `user/c/c_seam.c` and asserts they equal the Rust constants, so the "written twice
with nothing checking that the two agree" warning in `CLAUDE.md` is stale. Correcting that file is
the maintainer's; it is reported rather than edited here.

**The clock page.** Mapped `user_rodata` into init, the shell and any child whose manifest declares
one. A holder can read the wall clock and cannot set it, which is the point, and read-only is what
makes it a fact about the mapping instead of about the code.

**The nameset grant frame.** Mapped `user_rodata` into the caretaker and copied into a local at
`_start` "so nothing that happens to that page afterwards can widen the namespace." That is this
audit's lens, stated in the tree, and it is the model the file page should follow.

**The socket contract's decode.** Opcode and socket id from the request word, the id refused above
`MAX_SOCKETS` before it indexes anything; every payload length refused above `DATA_MAX`; receives
staged through a `[0u8; DATA_MAX]` stack buffer and then copied into the page. No slice is ever
formed over the shared mapping, in the server, in `std::net`'s PAL, or in `ntp`, so a concurrent
writer can change the bytes that go out and can corrupt nothing. The PAL's half is **generated** from
`crates/socket_proto` by `xtask`, so the offsets cannot drift.

One thing there is worth naming before somebody tidies it: **`OFF_LEN` is a length field in the page
that nothing reads.** The server writes it on receive and no client consults it; every length that
matters travels in a register. It is documented as "in for `SEND*`, out for `RECV`", which is an
invitation. The first change that makes the server honour the header length converts a contract with
no double fetch into one with a double fetch, and the diff will look like a tidy-up.

## The honest summary

| | What | Disposition |
|---|---|---|
| 1 | One FS shared frame for every client a boot wires, read-write in all of them; the property that keeps them apart is a scheduling argument, not a mapping | **Proposed as a milestone** |
| 2 | `fs_nameset_caretaker` checks a name off the page and forwards without re-staging it, so the FS server reads it a second time | **Fixed**, and the residue named |
| 3 | The console server's byte count is unclamped, and its `SAFETY` comment blames the wrong party | **Fixed**, comment corrected |
| 4 | `window.rs`: a `u32` overflow defeats its own geometry check, and an unclamped shift count | **Fixed**, both |
| 5 | The compositor reads surfaces and damage rectangles of clients that are not blocked | **Recorded-accepted**, in notes/compositor.md's BUGS |
| 6 | `net_transport` and `kbd` index a DMA buffer with a `u32` the device wrote, unchecked; the DMA validator covers the other direction | **Fixed**, fail-closed, with the hostile-device harness proposed |
| 7 | Three release-side fences in the compositor's subsystem whose acquire side did not exist | **Fixed**, two acquire fences |

**Nothing found is a live privilege escalation reachable from a confined program on `main` today**,
and that sentence has to be read with all three of its qualifiers, because two findings fall outside
them: finding 6 needs only a device that lies, and finding 7 needs only hardware that reorders. Both
are the parts of the threat model this system has declared and not yet met.

The limit is the same one the first audit recorded and is sharper here:

**This lens finds windows, and a window nobody can enter looks exactly like no window.** Findings 1,
2 and 5 are closed today by something other than the check that was supposed to close them: by a
wiring that happens to have one client, by a `CALL` that happens to be blocking, by a constant that
happens to bound a slice. Every one of those is true. Not one is written down as a requirement
anywhere the next change would have to read. That is the shape of arch-audit.md's finding 1 (safe by
an unstated invariant, not by construction), and finding 1 here is its direct descendant.

Two shapes recurred often enough to be worth naming as patterns rather than as bugs:

- **A guarantee assumed from the wrong side of a boundary.** The DMA validator confines where a
  device may touch and the driver read that as "the device cannot lie" (6). The blocking `CALL`
  parks the caller and the compositor read that as "no client is writing" (5). The producer's fence
  orders the producer's stores and the reader read that as "these loads are ordered" (7). In all
  three the guarantee is real, and it is a guarantee about somebody else.
- **One of a pair, written by instinct.** Three release fences with no acquire (7); a length clamped
  at four sites in one program and none in its sibling (3); a filter that copies what it checks and
  does not write back what it copied (2). Each is half a discipline, and half a discipline reads
  exactly like the whole one.

The single most useful thing this audit can hand forward is therefore not a bug but a rule, and it
is already followed in two places in the tree:

> **Copy what you check, and check what you use.** A server that validates a value out of a shared
> page and then acts on that page has validated a different thing. `fs_nameset_caretaker` copies its
> grant set out of the read-only page at `_start`; the FS server copies an attribute name to the
> stack before it writes the reply over it. Both do it for local reasons and both are right for this
> one.

## What wants a lane

Written as cases rather than as numbers: minting a milestone is the integrator's, and these are
proposals.

**A. One shared frame per client channel, not per FS service.** The case is finding 1. `ensure()`
memoises a single frame and hands it to every client the boot wires, so the FS service's page is
read-write in up to ten processes at once and what keeps them apart is who happens to be blocked.
The fix is a frame per channel: allocate in `spawn_fs_client` / `start_granted*` / `narrow_dir`
rather than in `ensure`, which also removes the ordering hazard `wait_for_caretaker` was written to
patch, because a caretaker's staging page would no longer be reachable by anything but its own
chain. **The witness is the deliverable**, not the wiring: two live confined programs on one FS
service, one of them substituting the other's name mid-request, failing before the change and
passing after. Severity is what makes it worth a lane rather than a note: it is a confinement escape
in the exact terms SECURITY.md puts in scope, and it moves from latent to live the day the shell can
ask init to build a caretaker, which `swish.rs` already names as the next step.

**B. A harness that can make a virtio device misbehave.** The case is finding 6 and the honesty gap
under it. This tree tests DMA confinement by making the *driver* attack (`crates/virtio`'s
`run_attack` and its indirect-descriptor cousin, both proving the kernel refuses), and it has no way
to make the *device* attack. So the direction the IOMMU and the validator were built for is the one
direction with no negative control, and the two fixes committed in this lane are unproven for the
same reason. Something that can write an arbitrary used ring under the driver, whether a fake
transport behind `crates/virtio`'s trait or a QEMU device model, would give every driver a hostile
counterparty and would be reusable by every later one. It is also what a second board makes urgent.

**C. A lint for one-sided fences.** The case is finding 7 plus milestone 80's. Both are the same
mistake and neither was visible to any gate, because a fence with a comment explaining it looks
correct whichever side is missing. A cheap version is mechanical: for each shared-page contract,
require that a documented publish/observe pair name its counterpart, and fail when a `fence` in one
program has no partner in the program on the other end. Even a list in a note that says "these are
the pairs, here is where each half lives" would have caught both, and this audit produced most of
that list on the way past.

---

*See also [arch-audit.md](arch-audit.md) for the first audit and its lens, [security.md](security.md)
for the review after milestone 11, [fs-server.md](fs-server.md) and [glob-grant.md](glob-grant.md)
for the contracts finding 1 and 2 live in, [compositor.md](compositor.md) for finding 5, and
`SECURITY.md` for what this project claims and what it does not.*

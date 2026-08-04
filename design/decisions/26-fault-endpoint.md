# 26. The fault endpoint: thread death becomes a message a supervisor holds

**Decided 2026-07-28 (Chris), the five sub-decisions settled one at a time; not yet built.** The
kernel is the only witness to a thread's fault, so it is the one that must pass the news along.
When a thread faults or exits, the kernel delivers a message to the supervision endpoint its
spawner designated. This is the one kernel mechanism milestone 22's supervision tree needs;
restart policy stays in userspace, and the kernel never relaunches anything.

The five parts, each decided explicitly:

1. **Build it.** The alternative (userspace heartbeat polling) is a poor death detector: timeouts
   are guesswork where the kernel has the exact instant and cause. Polling remains the right tool
   for a different problem, liveness ("alive but wedged"), and any supervisor can layer it on with
   ordinary IPC and no kernel help.
2. **Supervision is granted at spawn, only.** The fault endpoint is one more capability in the
   spawn endowment, so the supervision relationship is visible in the spawn literal and cannot
   change afterward. Runtime reattach (`Tcb::SET_FAULT_EP`) is deferred until milestone 23's
   hot-swap work demands supervision handoff, and it is a new decision when it does.
3. **Both faults and exits flow**, distinguished by an event code. Restart policy needs to tell
   "crashed" from "finished".
4. **Dead-until-reaped.** After the message, the thread never runs again, but its corpse (TCB,
   address space, memory) persists for postmortem until the supervisor reaps it with §16 object
   revocation. Suspend-for-inspection (resumable faults) is deferred to the SUSPEND tracker in
   Open design ideas, which now carries the userspace pager as a third trigger; the message format
   reserves a word so a fault-reply/resume protocol can arrive additively.
5. **One shared supervision endpoint per supervisor**, kernel-stamped identity per message:
   `(event code, tid, fault pc, fault address, reserved)`. Synchronous rendezvous means `RECV`
   blocks on one endpoint, so per-child endpoints would force a supervisor thread per child or a
   new wait-any primitive; the shared endpoint needs neither. The id word is trustworthy because
   the kernel is the only sender on this path (seL4 solves the general untrusted-sender case with
   badged capabilities; that mechanism returns as its own decision if shared endpoints ever need
   trustworthy identity from userspace senders).

Surface cost: no new syscall and no new method. Spawn already carries grants; delivery is a
kernel-internal send. The additions are a message-format convention and a spawn-slot convention,
recorded in notes/abi.md when built.

## Implementation (milestone 22, phase A), the decisions the build settled

The five sub-decisions above are the design; building it settled the details they left open. These
are amendments to §26, not a new section, per its own "no new section" intent.

1. **The spawn-slot convention is the last cspace slot, consumed at `START`.** The designated
   endowment (§26.2) is a real capability in a reserved slot, `abi::fault::FAULT_EP_SLOT` (=
   `CSPACE_SLOTS - 1 = 15`). A supervisor places its endpoint there with `Tcb::CAP_INSERT`, which
   grew an explicit target-slot argument (`0` keeps first-free, `n` targets slot `n - 1`) so the
   fault endpoint lands in the reserved slot instead of wherever first-free fell. That is the one
   surface change, and it is an *argument* to an existing method, not a new method. At `START` the
   kernel reads the slot: an `Endpoint` there makes the thread supervised, and the kernel records the
   endpoint (`Thread::fault_ep`) **and clears the slot**, so the child cannot forge messages on its
   own supervision endpoint. The *last* slot is deliberate: ordinary children fill low slots from
   zero, so none accidentally lands a working endpoint there and gets read as supervised.

2. **Delivery reuses the synchronous-send rendezvous; the corpse is the parked sender.** The
   non-blocking requirement (do not lose the event if the supervisor is not in `RECV`) is met by the
   *existing* sender-queue mechanism, not a new one. If a supervisor waits, rendezvous; if not, the
   dead thread parks on its supervision endpoint's sender queue with the message in its mailbox, and
   `RECV` collects it later. A death carries data (tid, pc, addr), so the data-less IRQ signal count
   does not fit; the sender queue does, and it is already proven. The corpse is never woken:
   `ipc_recv` leaves a `Dead` sender dead after taking its message, the same way it leaves a `CALL`
   caller blocked. So no new kernel mechanism was needed, which is what §26 predicted.

3. **Dead-until-reaped is a distinct thread state.** `State::Dead` is a corpse the reaper must *not*
   collect (unlike `Finished`); only the supervisor's §16 `DESTROY` frees it. Reusing `Finished`
   would race the reaper against the supervisor, so the distinction is a property of the type. The
   corpse's TCB retains the fault-time registers (its mailbox holds the five words), which is what
   the reserved fifth word needs to exist for.

4. **The IPC mailbox widened from three words to five.** The message is five words and `RECV` must
   deliver all five, so the kernel mailbox and the `RECV` result grew to five registers. Ordinary
   three-word IPC pads the top two with zero, so `user_rt::recv` and every existing program are
   unchanged; only a supervisor reads `w3`/`w4`. This is the message-format convention made real.

Proven on both ISAs (`kernel/src/user/supervision_tests.rs`): a child crashes and its supervisor
receives `(FAULT, tid, pc, addr)`, the corpse survives with its state until revocation reaps it, a
respawned child runs, and a clean exit reports `EXIT`. See notes/supervision.md and notes/abi.md §5.

## Milestone 22 phase B.1: measured boot, and the signature variant we did not build

**Built 2026-07-29.** Recorded here rather than as a new numbered section, because this is milestone
22's record and §26 is where milestone 22's decisions already live; the section numbering is
contended and grabbing a number would collide. Concept note: notes/trusted-init.md.

The gap: §14 promises "a verified core that confines unverified workloads," and at runtime the kernel
confines init as well as anything (MMU isolation proved, W^X, capabilities unforgeable, a compromised
init cannot break the kernel or escape). But init's **bytes** were loaded unchecked, and it is the
program that builds every other process. Anything that could substitute bytes at
`/chosen/linux,initrd-start` got to be init. Milestone 16b (§20, the IOMMU) had already closed the DMA
window a device could have used to rewrite the initrd *behind* the check, which is why the check is
now airtight rather than theatre; that ordering was deliberate.

Five decisions, each with its alternative on the record:

1. **Measured, not signed.** The kernel carries a digest of the boot program compiled into its own
   image (`trust::TRUST_ROOT`, generated by `kernel/build.rs`) and refuses to enter a program that
   does not match. The meaning is exactly "this kernel image runs exactly this init," which needs no
   keys, no certificate chain, and no signature-verification code inside the trusted computing base.
   It is the minimal honest thing that closes the gap.

2. **SHA-256, hand-written, one implementation for both sides.** The threat is byte substitution, so
   the hash must be collision- and preimage-resistant; a non-crypto hash (the FNV xtask uses for
   stale-input detection) would let someone craft a colliding init. Among collision-resistant options
   SHA-256 costs the TCB least: ~100 lines of shifts and adds in `crates/measured_boot`, no dependency, no
   allocation, no `unsafe`, and independently checkable with `shasum -a 256` anywhere. BLAKE3 (faster)
   and SHA-3 (a second permutation to audit) both buy speed we do not need for one 1.2 MB hash per
   boot. Hand-written rather than vendored, because a vendored crate is a supply-chain edge inside the
   TCB to save arithmetic whose reference text and test vectors are published. The build and the
   kernel hash through the *same* crate, so the measurement has one definition; the risk that trades
   for (an implementation agreeing only with itself) is answered by testing against the published
   FIPS 180-4 vectors and by the cross-check against the host's `shasum`.

3. **Fail closed in both directions, and an unmeasured program is a refusal.** Wrong bytes halt with a
   diagnostic naming the expected and measured digests. A *missing* measurement halts too: a kernel
   built without the manifest gets an empty trust root, and an empty trust root vouches for nothing.
   That second half is the one that matters, because the natural bug in a measured boot is for the
   check to evaporate silently when the build step does not run.

4. **The build composes one way: userspace -> archive -> manifest -> kernel image.** The kernel image
   holds the hash of a separately built initrd, so the initrd must exist first. No chicken-and-egg:
   the hash never feeds back into the initrd, and every xtask path already built `user()` before the
   kernel (it boots with the archive as `-initrd`), so nothing was resequenced. Cost accepted: a
   userspace change now relinks the kernel, which is what "runs exactly this init" means. A bare
   `cargo build`/`clippy` with no manifest yields an empty trust root rather than a build error, so
   the lint gate still works and the failure lands at boot where it belongs; a *malformed* manifest is
   a hard build error, because measuring nothing silently is worse than stopping.

5. **The kernel measures only the program the kernel loads.** `init` on aarch64, `init` and `system_initializer`
   on riscv64. Every other program in the archive is loaded by init in userspace and is not measured
   today, so the chain of trust stops at init's entry. The capability-correct extension is **init
   measuring what init loads** (its own table, in userspace, trustworthy because init's own bytes are
   now measured), which keeps policy out of the kernel the same way supervision does. Recorded as the
   follow-up, not built. Hashing the whole 14 MB archive in the kernel would cover everything with one
   value but puts both the cost and the policy in the wrong place.

**The signature variant, recorded as a follow-up rather than built.** A signature over init against a
public key compiled into the kernel buys one thing a hash cannot: **updating init without rebuilding
the kernel.** Its costs are real and both land in places this project protects. First, signature
verification enters the TCB: Ed25519 means field arithmetic, point decompression, and SHA-512, which
is an order of magnitude more code inside the boundary than SHA-256, and it is code where a subtle bug
is an accepted forgery rather than a crash. Second, key custody becomes a question a hash never asks:
where the private key lives, who can sign, how it rotates, and what revokes a compromised one (a
kernel with a baked-in public key and no revocation list is one leaked key away from accepting
anything forever). The peer project Atom ships Ed25519-signed executables, so this is real and
reachable, just a bigger TCB. It becomes worth paying for when init is delivered independently of the
kernel; today they are built by one command in one tree in one sequence, so the hash is strictly
better. The natural sequence if it is ever wanted: signature verification *in addition to* the
measured root (so the hash stays the floor if key handling fails), and the verification code proved
under §18's toolchain before it is trusted.

Proven on both ISAs (`kernel/src/user/measured_boot_tests.rs`): the boot program in RAM measures to
the digest in the running kernel's own `.rodata` (the end-to-end build-composition proof, nothing
hard-coded), and one flipped bit or an unmeasured name is refused. The refusal *path* is not booted in
a test because a real refusal halts the machine; the decision function is tested instead, and the boot
path's only response to `Err` is `arch::halt()`. Recorded plainly. Host tests in `crates/measured_boot`
carry the FIPS vectors. No bench movement: the bench boot enters no boot program.

## Milestone 22 phase B.2: init gives its authority away, and the supervision tree keeps running

**Built 2026-07-29.** Phase A gave the kernel the mechanism (a death becomes a message); B.1 settled
what bytes init is. This settles **what a compromised init can still reach**, which was the second half
of the §14 soft spot. Recorded here with the rest of milestone 22 for the same reason B.1 is.

1. **Init's authority becomes short-lived, not merely careful.** The pre-B.2 init holds a large untyped
   budget for its whole life because it stays the system's process builder, so every process is one bug
   in init away from being built wrong. The new root (`user/src/root_supervisor.rs`) holds full construction
   authority only long enough to build two servers, then **deletes** it (the wiring capabilities, the
   spawner's budget copy, and the root untyped). After that it cannot make a page, an address space, a
   thread, or an endpoint. The alternative (keep the budget, be careful with it) was rejected on the
   §14 thesis: a confinement you can only honour by being correct is not confinement.

2. **Process construction moves to a sub-server that holds one program image, not the archive.** The
   spawner gets `flaky`'s bytes copied into read-only pages of its own address space, never the 14 MB
   initrd, so "build program X" is unanswerable for any other X. Its budget is `WRITE` without `GRANT`:
   it may spend memory, never lend it. Each instance is built in its own region split off that budget,
   which makes a reap one `Untyped::DESTROY` (§16) and, LIFO, returns the pages to the budget.

3. **The supervisor holds no memory at all.** `sub_server_supervisor` has a request channel, a fault endpoint, and a
   report endpoint. It cannot build, allocate, or reap; it can only *ask*. So the split is: the
   supervisor decides **whether** to reap and rebuild, the spawner is what **can**. Policy and
   authority separated by an IPC boundary, which is the same shape as every other decision here.

4. **Restart policy is userspace code and stays there.** Bounded retries, a clean exit read as
   "finished" rather than "crashed" (which is why §26 delivers both events), a give-up. The kernel's
   whole contribution is one five-word message, unchanged from phase A. No new syscall, no new method.

5. **Proven by authority, not by timing.** Two cross-ISA tests (`authority_tests`): after the handoff
   both construction primitives fail from inside init with `NoSuchSlot` (nothing there) rather than
   `NotPermitted` (something there, restricted); and a faulting sub-server is reaped and restarted by
   its supervisor, with the clean exit of the replacement *not* triggering another restart. "init was
   not involved" is proven by the empty capability slot, not by scheduling order: a process that cannot
   retype a page cannot have built the replacement.

**Two design forks found, reported rather than built through.**

- **Reaping needs the same right as building.** `DESTROY` and `RETYPE` both need `WRITE` on the region,
  so a root supervisor that can restart a dead tier-one server is a root supervisor that can build
  processes, which is the authority the milestone exists to give away. root_supervisor therefore chooses to be
  unable to build, and its policy for a tier-one death is "report and stop", the fail-closed floor.
  Splitting a **reap-only right** out of `WRITE` (a rights bit, or a distinct `Untyped::REAP` method)
  would let a root recover without regaining construction authority. That changes the rights model and
  the syscall surface, so it is a decision, not an implementation detail.
- **A supervisor cannot turn a tid into a handle.** The fault message names the dead thread by tid
  (§26.5), but nothing maps a tid to something a builder holds, so `sub_server_supervisor` names instances by a handle
  the spawner issues. That is sufficient for one child at a time and insufficient in general. Options:
  a `Tcb::NAME` method (small, and discloses nothing the fault message does not already), per-child
  fault endpoints (which §26.5 rejected for needing a thread per child or a wait-any primitive), or the
  builder reporting the tid it created.

**What is deliberately still open.** The tree proves the pattern with real programs on both ISAs, but
it is **not yet the interactive boot's init**: `system_initializer` and `hello`'s init role still hold their
budgets for life, because they remain the shell's spawn service. That migration is the next increment
and was not done blind in the same pass, because that boot path is hand-validated (the harness cannot
inject keystrokes) and moving the spawn service wants an interactive confirmation, not a green unit
test. See notes/trusted-init.md for the shape it takes.

**A pre-existing bug this work found, on both architectures.** The supervision tree enters more
processes per run than anything before it, and that surfaced a race in the **exception-return path**:
staging `SPSR_EL1`/`ELR_EL1` (aarch64) or `sepc`/`sstatus` (riscv) for the return is not atomic with
respect to a nested exception, so an interrupt in a two-instruction window could return a brand-new
process to its entry point **at EL1** (aarch64, observed, about one suite run in four) or to a kernel
address in U-mode (riscv, found by inspection). Only the first-entry path was exposed, because a normal
trap return already has interrupts masked. Fixed by masking at the top of the restore, one instruction,
free at the far end because the return restores the mask from the saved state anyway. Written up in
notes/exceptions.md. The icount baselines moved by well under 1% (one extra instruction per exception
return) and were re-saved in the same commit.

# Open design ideas

Not decisions yet. Proposals with real open questions, parked deliberately.

The [post-v1 milestone roadmap](roadmap/README.md) sequences the buildable ones below into
proposed numbered milestones (12+) and names the two decisions they force (the verification
endgame, and POSIX posture). The entries here remain the detailed source for each.

- **SMP thread placement** (§11's deferred step 3c). **SUPERSEDED by §28 (built 2026-07-28/29).** The
  standing gap this described (every spawn and wake on the current core, so a workload fanning out
  from one core stayed there; the milestone 32 FS mount starved beside three idle cores) is closed.
  §28 shipped the power-of-two-choices spawn placement and message-shaped work stealing this entry
  weighed, and its implementation amendment chose the third option here, **wake-time balancing**, for
  device interrupts specifically (least-loaded, ties to the current core) while keeping IPC rendezvous
  wakes local. See §28 and notes/scheduler.md. Kept for the record of the reasoning that led there.

- [Microarchitecture-variant binaries](fat-binaries.md): our targets straddle the
  ARMv8.0 / ARMv8.2 line (no LSE atomics on Cortex-A72, LSE on everything newer), and with
  no libc we can't lean on LLVM's `outline-atomics` to paper over it. Milestone 6 forces
  the kernel-atomics question; milestone 7 is where a fat userspace format would be
  decided. Feature detection via the `ID_AA64ISAR*_EL1` registers is worth building at
  milestone 2 regardless.

- [Driver domains, and the DMA-confinement design space](driver-domains.md): the
  principled version of the DMA hole we closed in software (notes/dma.md): run each driver in its
  own VM with cricker-os as the hypervisor at EL2, and confine its DMA with the SMMU's stage-2. The
  strongest driver isolation there is, and the opposite of a shortcut: it needs EL2, an SMMU
  driver, and is impossible under HVF. Parked as the most interesting unbuilt direction.

- **Call/Reply IPC: a kernel-minted, one-shot reply capability** (notes/ipc-naming.md). IPC names
  an endpoint and the sender is anonymous, so a server cannot reply to a *specific* caller. Today
  we wire an explicit reply endpoint per client at spawn. seL4 mints a one-shot `Reply` cap on
  `Call` so a server can answer whoever called, with a kernel-tracked call chain that also enables
  priority donation. We can emulate reply-to-caller with `SEND_CAP` (the client passes a
  reply-endpoint cap in the request), but *not* the one-shot safety or the call chain: those need a
  `Reply` object and a `Call` method, which widen the §4 syscall surface and so should not be added
  speculatively.

  **Two triggers to build.** *Functional:* the first server that must serve clients it was not
  individually wired to (a general RPC service). *Safety:* the first reply whose correctness depends
  on going to **this** caller (caller-identity) or on being consumed **exactly once**. The
  distinction matters because a pre-wired reply endpoint is reusable and nameable, so nothing
  *structural* stops a reply reaching the wrong caller, a double reply, or a stale reply landing on
  a client that moved on. A one-shot kernel-minted reply cap makes "exactly one reply, to exactly
  this caller, consumed on use" a kernel guarantee instead of a server discipline.

  **Where we stand today (checked, 2026-07-22):** safe, but by *convention*, not guarantee. The
  console server shares one `reply` endpoint across clients yet is correct because it is
  **single-threaded** and IPC is synchronous rendezvous: it handles one request-reply cycle at a
  time, so the only client in `RECV(reply)` when it replies is the one it just served. Workers and
  drivers use a **per-request** result endpoint (no sharing). The safety trigger fires the moment
  either of those stops holding: a server **thread pool** on a shared reply path, or pipelined /
  asynchronous requests.

  **Built at milestone 12 (§12).** The shape sketched here is exactly what landed: a `CALL` method and
  a one-shot `Object::Reply(Tid)`, kernel-minted at the rendezvous, delivered through `RECV_CAP`, and
  consumed on use. The call chain and priority donation are deferred (moot without priorities); the
  detail above stays as the design record.

- **Capability revocation, and untyped reclamation** (notes/capability-lifecycle.md).
  **Built at milestone 13 (§13), scoped to frame revocation.** A `Frame::REVOKE` method and
  `untyped::destroy` now unmap a page from every holder and delete every capability to it, which is
  what met the precondition below and let reclamation land. The full capability-derivation tree (for
  subtree-granularity revoke) is deferred, not on the path to an inevitable rewrite; see §13 and
  design/roadmap/13-capability-revocation.md. The rest of this entry is the pre-§13 design record.

  A granted
  capability cannot be retracted: no capability-derivation tree, no refcount, no `revoke`
  (untyped.rs). This is **not a memory-safety hole**: frames come from spend-only untyped and
  teardown never frees a shared leaf, so a surviving peer maps valid, non-reused memory, but it
  means you cannot *un-share* a frame from a live peer (only destroy the peer) and never *reclaim*
  the page. seL4's mechanism is a capability-derivation tree plus a recursive `revoke` that unmaps
  the object from every holder; expensive and kernel-tracked, which is why it is a first-class
  object there and "the harder story parked for later" here. **Trigger to build:** needing to
  retract authority from a live, untrusted peer, or to reclaim untyped on process death.

  **BLOCKING PRECONDITION on any reclamation work.** The "not a memory-safety hole" conclusion
  rests entirely on one invariant: **retyped frames are spend-only and never returned to a reusable
  pool.** So *any* future reclamation: wiring up `untyped::destroy`, a frame free-list, an
  allocator that recycles, or the reclaim-on-process-death above: is **blocked on revocation
  landing first.** The instant a shared frame can be reused while a peer still maps it, every
  dangling mapping this entry calls "harmless" becomes a use-after-free. This is the classic seam:
  two individually-correct changes, months apart, whose *interaction* is the hole. `untyped::destroy`
  already exists, unused, as exactly that trap; it carries the same warning at the code, so the
  person who eventually wires it (thinking about untyped accounting, not shared-frame lifetimes)
  meets the precondition there too.

- **`Tcb::SUSPEND`/`RESUME`: pause a thread without killing it** (deferred from the `^C` decision,
  §24, 2026-07-28). The two-tier interrupt design covers notify and kill; suspend is the third
  verb that would make "interrupt" mean pause-and-inspect. Deferred because it widens the §4
  syscall surface with no consumer yet, and because it should be designed next to milestone 22's
  fault endpoints (both are "the kernel turns a thread's state into a message a supervisor
  holds"). **Triggers to build:** (1) a userspace pager (demand paging is fault-message,
  fix, resume: the fault endpoint of §26 is its front half); (2) real job control (`fg`/`bg`, a
  stopped-process state) in the shell; (3) a debugger. (Milestone 22's supervision tree chose
  dead-until-reaped over suspend-on-fault, §26, so it is no longer a trigger.) Whichever fires first, design SUSPEND and the fault endpoint as one
  surface, and give the method its own DECISIONS entry.

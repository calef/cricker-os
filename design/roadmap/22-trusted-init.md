# 22. Trusted init: verify it, and shrink what a broken one can do

**Status: BUILT.**

**In brief.** Measured/secure boot that checks init before running it; reduce init's authority so a compromise is bounded

**Why it matters.** **closes the thesis's own soft spot:** init is the privileged *unverified* component the whole system is built by

**The soft spot this closes.** §14 promises "a verified core that confines unverified workloads."
init is unverified, but it is not a *typical* workload: it holds the process-construction authority
and builds every other process. At runtime the kernel confines it as well as anything (MMU
isolation is proved, its code is W^X, capabilities are unforgeable), and a compromised init
**cannot break the kernel or escape confinement**. But its *bytes* are currently loaded unsigned and
unchecked, and its *authority* is broad, so within that authority a corrupted init can do real harm
(endow malicious children, deny the system it was meant to start).

**Deliverable, three halves.**

1. **Verify init before it runs. (Phase B.1, BUILT 2026-07-29.)** A measured boot step: the kernel
   checks init's hash before dropping to EL0/U-mode at its entry. seL4's high-assurance deployments do
   exactly this for the root task; it was the single biggest gap between "verified kernel" and
   "trustworthy system." Built as the **measured** variant: the build hashes the archive entry it
   packed and `kernel/build.rs` compiles the digest into the kernel image, so the check means "this
   kernel image runs exactly this init" with no keys and no signature code in the TCB. SHA-256,
   hand-written in `crates/measured_boot`, one implementation shared by the build and the kernel. Fails
   closed both ways: wrong bytes halt, and an *unmeasured* program halts too (an empty trust root
   vouches for nothing). Both ISAs. The **signature** variant (update init without rebuilding the
   kernel, at the cost of Ed25519 in the TCB and a key-custody question) is recorded in DECISIONS §26's
   phase B block as a follow-up, not built. See notes/trusted-init.md.
2. **Shrink the blast radius. (Phase B.2, BUILT 2026-07-29; the interactive boot's migration is the
   remaining increment.)** Reduce what a compromised init can do: hand most process-construction to
   smaller, less-privileged sub-servers, so init's own authority is minimal and short-lived (build the
   first servers, then drop the untyped). The less init holds, the less a broken init costs. Built as a
   four-program tree (`root_supervisor`, `spawner`, `sub_server_supervisor`, `flaky`): the spawner holds one program image and
   a `WRITE`-only budget (not the archive, so it can build exactly one program), the supervisor holds
   no memory at all and can only *ask*, and the root deletes its untyped once both are running. Proven
   on both ISAs by authority rather than timing: after the handoff, retyping a page or a kernel object
   from init fails with `NoSuchSlot`, and a faulting sub-server is reaped and restarted by its own
   supervisor. **That migration then reached the interactive boot itself (BUILT 2026-08-04, both ISAs).**
   `system_initializer` and `hello`'s init role remain the shell's spawn service, but they no longer
   hold the construction budget for life: after the boot servers are up, each carves a bounded
   scratch budget and a bounded job pool, deletes the root, and gives away the UART device
   capability, its interrupt, and every capability reaching a live job's memory. A new `job_undertaker`
   (one endpoint, `READ`, no untyped, no restart policy) collects finished jobs and returns their
   regions to the pool. Proven the same way the rest of this milestone is: init prints, from inside
   itself, that `RETYPE` now answers `NoSuchSlot` rather than `NotPermitted`, and `script/shell-check`
   runs eleven jobs through a six-job pool, so a boot that collected nothing fails partway down.
   The predicted sub-server for the spawn service was **refused with a reason** (the spawn service is
   the ELF loader and the loader is the archive, so a sub-server would hold every program in the
   system while init held a pipe); see notes/trusted-init.md. Two design
   forks found and reported rather than built through (a reap-only right, and turning a tid into a
   handle). See DECISIONS §26's phase B.2 block and notes/trusted-init.md.

   **Both of those forks are now closed (DECISIONS §32, BUILT 2026-07-29, both ISAs).** Reaping
   moved off `Untyped::DESTROY`, which needs `WRITE` on the region and therefore the same right that
   *builds* a process from it, onto `Endpoint::REAP` on the supervision endpoint. Authorization
   needed no new bookkeeping: §26 already records `Thread::fault_ep` and the kernel already stamps
   the tid, so the check is that the named thread's recorded endpoint *is* the one being invoked.
   The tid-to-handle fork is closed for this case by the same move, because the tid is authorized
   relative to the endpoint it arrived on rather than being a global handle. The measured payoff:
   **`sub_server_supervisor` now holds nothing but endpoints**, since the phase B.2 proxy that had to ask `spawner`
   to reap is no longer needed. The measured limit: milestone 36's `c_confiner` still holds a
   construction budget because it is *also* the builder, which shows the bundling was two things and
   only one of them was the reap. `REAP` refuses a live thread on purpose, so a **hung** child still
   cannot be restarted; that is the watchdog case and it belongs to 23. Two Kani harnesses in
   `crates/capability` cover the authorization invariant. See notes/supervision.md.
3. **Supervise, don't relaunch-in-kernel.** What happens when init (or any server) *fails*, as
   distinct from being corrupted. The failure of init degrades to a **halt, never a breach**
   (the kernel's guarantees hold regardless), so the only open question is availability: halt, or
   recover? The answer is neither a bare halt nor a kernel that relaunches init.

   - **Not kernel-relaunch.** Relaunching init from the kernel re-imports the loader we just
     evicted (milestone 19) plus *restart policy* (retries, backoff, escalation) into the trusted
     core, and it crash-loops on a deterministic fault (init panics on a bad ELF; relaunch hits
     the same bug). Restart is policy, and policy does not belong in the kernel.
   - **The mechanism/policy split, as everywhere else.** Add one small *mechanism* to the kernel:
     a **fault/death notification**, when a thread faults or exits, the kernel delivers a message
     to an endpoint held by whoever holds the capability to supervise it. Capability-gated (you
     can supervise a thread only if you were granted its fault endpoint), mechanism-only. This is
     seL4's fault endpoint.
   - **Policy lives in a userspace supervision tree.** init builds the system, wires supervisors,
     and either becomes a *minimal* root supervisor (so small it essentially cannot fail) or steps
     back. A sub-server that dies is restarted by *its* supervisor with whatever policy it wants
     (bounded retries, fall-back, give-up), in userspace. Failures below the root are contained
     and restartable; only the death of the irreducible root supervisor halts, which is the
     fail-closed floor, pushed as high and as small as possible.
   - **This also dissolves the SPOF.** init-during-boot stays a single point of failure (if it
     cannot build the system, halt is correct: nothing to recover to). init-*after*-boot stops
     being one: it is either a trivial root or gone, and failures below it are supervised.

   The one kernel primitive this adds (the fault endpoint) is worth its own numbered decision when
   19d.2/22 make it concrete; recorded here so the design (halt is the floor, supervision is the
   answer, the kernel never runs restart policy) is on the record rather than in a conversation.

**The reach tail.** Beyond verifying init's *bytes*, verifying init's *behaviour* is the natural
next layer inward for the §14 thesis: init is small and privileged enough to be worth proving, once
the kernel's proofs are done. Recorded as the direction, not committed. (Distinct from supervision
above: proof buys *safety*, supervision buys *availability*; init's failure mode is availability, so
supervision is the load-bearing answer and proof is the optional reach.)

**Prior art.** seL4 + a verified boot chain (measured boot, or CapDL-driven system initialisers
whose output is checkable); the general secure/measured-boot literature (TPM/PCR measurement,
signed boot images). For the supervision half: seL4 fault endpoints (the kernel turns a fault into
a message a supervisor holds); MINIX 3's reincarnation server (a userspace process that restarts
dead drivers, not the kernel); Erlang/OTP supervision trees and "let it crash" (decades of evidence
that restart policy wants to be a rich userspace thing, not a kernel reflex).

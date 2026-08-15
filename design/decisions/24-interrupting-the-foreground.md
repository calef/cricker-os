# 24. Interrupting the foreground process: two-tier, shell-held, no new kernel surface

**Status: AMENDED.** (an implementation amendment below records the two primitives the build forced.)

**Decided 2026-07-28 (calef), from the proposal in design/interrupt-routing.md.** `^C` routes in
two tiers. The first `^C` is cooperative: the shell sends an interrupt message on an endpoint the
foreground child was spawned holding, and a program that listens can cancel cleanly. The second
`^C` (or a shell-side timeout) escalates to the forcible tier: the shell tears the child down with
object revocation (§16), which handles the runaway that never checks its endpoint. The interrupt
capability is held by the **shell**, because job control is the shell's knowledge; a process that
was not granted a child's interrupt endpoint cannot interrupt it, and there is no ambient deliverer
of signals. Unix signals (ambient authority, delivered by PID) are exactly what this design
refuses to reintroduce.

**No new kernel primitive.** The cooperative tier is the existing endpoint machinery; the forcible
tier is §16's revocation. The escalation policy (how many `^C`s, what timeout) lives in the shell,
userspace, where policy belongs.

**Deferred, deliberately: `Tcb::SUSPEND`/`RESUME`.** A suspend method would make "interrupt" mean
pause-and-inspect (real job control, an eventual debugger) instead of notify-or-kill. It is
deferred, not rejected: it widens the syscall surface for a consumer that does not exist yet, and
milestone 22's supervision work (fault endpoints) is the adjacent primitive it should be designed
beside. Tracked in Open design ideas above; the trigger to revisit is written there.

## Implementation amendment (built): two primitives forced the shape

Building it (both ISAs) hit two facts about the primitives that refine, without changing, the
two-tier decision. Both were confirmed with the architect before building; recorded here because
the reasoning is the deliverable.

**The cooperative signal is a shared-memory flag, not an endpoint delivery.** The design imagined an
async notification on an endpoint the foreground job watches. But the job the user most wants to
interrupt is *running a computation*, and a running program cannot watch an endpoint: there is no
non-blocking receive, and a blocking one would stall the very work being interrupted. So the shell
mints a per-job shared frame (`grant_plan::jobframe`), maps it into the child, and writes an interrupt
word the child reads with a plain load *between work units*. This is "control by shared memory"
where the model usually says "control by message", and it is honest about why: the message form
needs a notification primitive that does not exist yet. It is granted like any capability, through
the manifest's `interruptible` endowment, so the authority story is unchanged: a program the shell
did not endow a job frame cannot be signaled, and cannot signal back.

**The forcible tier is a plain `Untyped::DESTROY` on the child's region, and that required the §16
amendment.** The shell builds a supervised child *entirely from an untyped it split from its own
budget and delegated to init*, so the whole child, aspace and TCB and code and stack, lives in a
region the shell holds. Tearing it down is `DESTROY` on that region. The first instinct, faulting
the child by revoking a frame it touches, was rejected: a genuine runaway (a bare `loop {}`) touches
nothing revocable, so frame-revocation cannot reach it. Instead `DESTROY` learned to force-kill a
live resident thread (the §16 amendment): a refused reclaim arms the kill, each core converts its
own killed thread to a corpse at the next preemption, and the owner retries `DESTROY` until it
succeeds. The shell's watch loop retries exactly so. A pure `loop {}` spinner is now torn down on
the second `^C` on both ISAs.

**The shell learns of `^C` by polling, deliberately (wait A).** The shell must watch the job and the
`^C` at once, and with only blocking primitives it cannot block on both. It busy-polls `line_editor`'s new
`OP_INTRCOUNT` (an immediate reply with the running `^C` count) with `yield` between, driving the
escalation from the count's advance. The escalation policy (first `^C` cooperative, a second `^C` or
a grace-window timeout forcible) is host-tested in `grant_plan::Escalation`. Holding `^C` routing in the
shell, not `line_editor`, is the §24 premise: job control is the shell's knowledge, and `line_editor` stays a
terminal. The clean blocking form waits for the notification primitive milestone 23's latency
ladder forecasts; the shared flag and the poll are the honest interim, not the destination. See
notes/grant-expression.md (the interrupt grant) and notes/terminal-contract.md (the flow).

# Interrupt routing: what Ctrl-C means without signals

**Status: DECIDED 2026-07-28 (Chris), then BUILT: the two-tier design (option C), interrupt
capability held by the shell, `Tcb::SUSPEND` still deferred. See DECISIONS §24 and its
implementation amendment for what shipped (the cooperative tier is a shared-memory flag, not the
endpoint notification this proposal imagined, because a running compute job cannot poll an endpoint;
the forcible tier is `Untyped::DESTROY` on a region the shell holds). This document stays as the
original design record.** Milestone 28 built the
terminal that *detects* `^C` and wrote down the contract's hook for it (`FLAG_INTERRUPTED`), and
deliberately stopped there. How the interrupt actually reaches the foreground process is a
capability-routing question with real forks, and this project's answer will not be Unix signals.
This note frames the problem, lays out candidate mechanisms, reads the prior art, and recommends
a direction. It builds no kernel mechanism.

## The problem

A user runs a command and it takes too long, or hangs, or is a mistake. They press `^C` and
expect the foreground computation to stop and the prompt to come back. In Unix the tty line
discipline turns `^C` into `SIGINT` and sends it to the **foreground process group**, named by
PID. That is ambient authority in two layers: the tty may signal any process in its group, and a
process is reachable by a global identifier it did not hand anyone. cricker-os rejects both
(DECISIONS §10, [ipc-naming.md](../notes/ipc-naming.md)): there are no PIDs to signal and no
global namespace to reach a process through. So the question is sharp: **when the user presses
`^C`, whose right is it to interrupt which process, and what capability carries that right?**

Two cases, and the split is the whole difficulty:

1. **The foreground process is blocked reading.** This one is already handled. The terminal fails
   the parked `OP_READLINE` with `FLAG_INTERRUPTED`; the shell sees it, discards the line, and
   reprompts. No process was doing work, so there is nothing to stop. `termd` does this today.
2. **The foreground process is running.** A runaway loop, a long compute, a wedged driver call.
   There is no outstanding read to fail. Interrupting it means reaching a process that is *not
   asking to be reached*, which is exactly the authority Unix grants ambiently and we do not.

Everything below is about case 2.

## What must be true of any answer

- **The authority is scoped to the current foreground job, and nothing else.** The terminal (or
  the shell behind it) may interrupt *the process it just launched into the foreground*, because
  the user designated it by running it there. It may not interrupt an arbitrary process. This is
  the capability restatement of "the foreground process group": the group is not a PID set, it is
  the single job you hold an interrupt capability for.
- **It is a capability the holder was handed, not a name it knows.** The interrupt right is minted
  when a process becomes the foreground job and is surrendered when it stops being one.
- **No new "signal" concept.** Whatever we build reuses the primitives we have: endpoints and
  notifications for the cooperative path, and the object lifecycle (revocation, or a TCB method)
  for the forcible path. A general signal number space is the thing §10 argues against.

## Candidate mechanisms

### A. A cooperative interrupt notification (async, endpoint-delivered)

When the shell puts a process in the foreground, it gives the terminal an **interrupt endpoint**
that the foreground process is watching (or that the kernel delivers an async notification to,
the same way an IRQ becomes a message: [interrupts.md](../notes/interrupts.md)). On `^C` the
terminal notifies it. The process handles the notification at a safe point and unwinds: it prints,
frees what it holds, and returns to the shell.

- *Strength:* clean, fully within the existing model (an interrupt is already "a notification on
  an endpoint"). Graceful: the process decides how to stop.
- *Weakness:* it is **cooperative**. A process that never checks (a tight loop, a wedged state)
  never sees it. That is precisely the runaway case the user most wants `^C` for. Preemption gets
  the CPU back, but nothing makes the process *notice*. On its own, A does not solve case 2.

### B. A forcible abort capability (the holder can tear the job down)

The foreground job is represented by a capability the terminal holds that can **destroy or
suspend** it: either the job's `Tcb` capability with a suspend/kill method, or a revocable "job"
capability whose revocation reclaims the job's objects (DECISIONS §13, §16, which already reclaim
a process's frames and objects). On `^C`, the terminal invokes it; the job stops regardless of
what it was doing.

- *Strength:* works for the runaway case, because it does not need the target's cooperation. Reuses
  object revocation, which exists.
- *Weakness:* it is **kill, not interrupt**. The process gets no chance to clean up or to
  distinguish "you were interrupted" from "you died." For a shell built to relaunch a fresh job
  per command (which ours is, milestone 10's worker model), kill-and-reprompt is often acceptable,
  but it forecloses graceful interruption and in-process handlers.

### C. Two-tier: cooperative first, forcible on escalation

Combine them. `^C` delivers the cooperative notification (A). If the job does not yield within a
window (a second `^C`, or a timeout), the terminal escalates to the forcible capability (B). This
is close to how a real shell feels: the first `^C` asks nicely, a second one insists.

- *Strength:* graceful when the process cooperates, decisive when it does not. Both capabilities
  are job-scoped and surrendered when the job ends. No ambient authority at any tier.
- *Weakness:* two mechanisms and an escalation policy to specify (who times out, how long, where
  the second-`^C` policy lives). More surface than either alone.

### Who holds the capability: terminal or shell?

Orthogonal to A/B/C. The interrupt right can live with the **terminal** (it detects `^C`, so it
acts directly, lowest latency) or with the **shell** (the terminal reports the interrupt as an
event, and the shell, which owns job control, decides). The shell is the more principled home,
because "what is the foreground job" is the shell's knowledge, not the terminal's, and milestone
31 makes the shell the authority on grants. The cost is one more hop (`^C` becomes an event the
shell must be waiting for) and a shell that must multiplex reading its next line against watching
for an interrupt. A pragmatic split: the terminal reports `FLAG_INTERRUPTED` (case 1) itself, and
routes case 2 to the shell, which holds the job's interrupt capability.

## Prior art

- **seL4** has no signals. Control over a running thread is control over its `TCB` capability: a
  supervisor holding the TCB can `Suspend`/`Resume` it, and faults are delivered to a
  registered fault endpoint. Interruption is "whoever holds the TCB decides," which is candidate
  B in its purest form, plus fault-handler endpoints that resemble A. The lesson: the right to
  stop a thread is just the right to name its TCB.
- **Fuchsia** also has no Unix signals. Tasks live in a **job hierarchy**; a job or process is
  named by a handle, and `zx_task_kill` on that handle tears it down. Cancellation of pending
  async waits is explicit (`zx_port` / cancel), which is candidate A's shape. A shell's `^C`
  routes through the handle it holds for the job it launched, not through a PID. This is the
  closest cousin to what we want: capability (handle) scoped to the launched job, kill via the
  handle, cancel via ports.
- **Plan 9** uses **notes**, a deliberately thin signal-like mechanism: a note is a short string
  written to a process's control file (`/proc/n/note`), and `^C` posts an "interrupt" note to the
  process group. It is still a namespace (the /proc file), but reaching a process is gated by
  file permissions on that control file, not by knowing a PID, so it is a step toward
  designation-as-authorization. The counter-lesson: even Plan 9 keeps a coarse, string-tagged
  interrupt rather than a full signal table, which supports keeping ours minimal.

## Recommendation

Adopt **C (two-tier), with the interrupt capability held by the shell**, and reuse existing
primitives rather than inventing a signal:

1. The **cooperative tier** is an async notification on an endpoint the foreground job watches,
   built from the same "interrupt becomes a message" machinery as IRQ delivery. A job that wants
   graceful interruption receives it; a job that does not care simply never registers, and the
   escalation covers it.
2. The **forcible tier** is object revocation / a `Tcb` suspend-or-destroy method on the job,
   scoped to exactly the foreground job. This is where the model may need one genuinely new
   method (a TCB `SUSPEND`, if we want suspend-and-report rather than destroy), which would be its
   own DECISIONS entry because it widens the §4 boundary. Destroy-and-reprompt needs nothing new.
3. The **shell holds both capabilities** for the current job and surrenders them when the job
   ends. The terminal handles case 1 (`FLAG_INTERRUPTED` on a parked read) itself and routes case
   2 to the shell.

The open decisions for the architect, which is why this is not built:

- Whether case 2 is worth solving now at all, or whether "the shell relaunches a fresh job per
  command and a runaway job is reaped by preemption plus a forcible destroy" is enough for the
  demonstrator, deferring the cooperative tier until a long-running foreground job exists that
  deserves graceful interruption.
- Whether to add a `Tcb::SUSPEND`/`RESUME` method (suspend-and-report) or to make `^C`'s forcible
  tier a plain destroy via the revocation already built.
- Where exactly the escalation policy (second-`^C`, or timeout) lives.

None of these should be settled by building through them. They are the substance the milestone-28
roadmap entry called "kernel-adjacent," and they want a decision before code.

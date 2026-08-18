# 98. `OPENDIR` cannot be asked to attenuate, so a held directory probes for its own rights

**Status: PROPOSED.** Raised 2026-08-18 by milestone 122's lane, which shipped the workaround and
said so rather than presenting it as a design. What is proposed is the replacement, and it is a
**wire change**, which is why it is calef's and not a lane's.

**What is blocked: nothing.** Milestone 122 is built and correct. The cost of leaving this is that
the tree carries a workaround in the place a reader meets the type.

## The rule this collides with

Every verb in the std PAL asks for the **minimum right it needs**, because over-asking is `EPERM`
rather than attenuation, and a client cannot read its own capability to find out what it holds. That
rule is right and it is why `readdir` does not ask for `dir::ALL`; the same file records that
`readdir` nearly shipped asking for exactly that, which would have passed every test in the suite and
failed through every narrowed grant.

**The rule has no answer for a held object.** A `Dir` is not a verb. What will be asked of it later
is not knowable when it is minted, so there is no minimum to ask for.

## What shipped

A `Dir` asks for `dir::ALL`, and when a narrowed grant refuses, it discovers what is actually there by
asking for one right at a time. Six extra messages, at most once per `Dir::open`, and one message in
the common case. Correct, bounded, honest, and a workaround.

## What was considered, and why each lost

- **Ask for a fixed useful mask.** Every fixed mask has a grant it breaks under. See `readdir` above:
  this tree nearly shipped that mistake once and the suite would not have caught it.
- **Mint the handle per operation.** Then `Dir` is not holding a capability, which is the point of
  the type, and it reintroduces the time-of-check-to-time-of-use the type exists to avoid.
- **Hold a path and re-walk when a new right is first needed.** The same TOCTOU, and it is the
  generic fallback wearing a capability's clothes.

## The proposal, priced

**A sentinel in `OPENDIR`'s rights word meaning "the parent's, whatever they are."** It cannot widen
anything, because the result is `parent & parent`. About thirty lines across `fs_proto`, `fs_server`
and the PAL, and it deletes both the probe and the same trap in `MKDIR`.

## Why this is calef's

It is **a thing two programs agree on**, which the *move fast on what can be undone* tenet puts in the
irreversible column alongside names, dependencies and the syscall surface. The code is a morning; the
un-shipping is not.

**If the answer is no**, the probe stands. It costs six messages per `Dir::open` under a narrowed
grant, which nothing measurable cares about today, and the standing cost is a workaround where a
reader meets the type rather than a wrong behaviour.

## BUGS

- **The probe's cost is asserted, not measured.** "Six messages, nothing measurable cares" is
  reasoning from the shape of the code. No benchmark covers `Dir::open` under a narrowed grant, and
  milestone 121's per-component IPC measurement, which would price the neighbouring case, has not
  started.

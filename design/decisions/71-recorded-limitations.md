# 71. A limitation is promoted when it stops being a fact and becomes a plan

**Status: DECIDED.** calef, 2026-08-15. Raised 2026-08-04 by milestone 94's sweep, which blessed
nine recorded limitations and correctly declined to invent this convention as a drive-by.

## The two artifacts assert different things

A `BUGS` entry asserts a present-tense fact: this is how the system is, knowingly. It addresses
a user at the moment of use, it is complete in itself, and it carries no commitment to change.
A roadmap row, whatever its status, asserts intent: the project plans, or deliberately defers,
some work. `RECORDED` specifically means analysis captured with the decision deliberately not
taken (milestones 39 and 52). Promoting every substantial limitation to a row would convert
documentation into commitment and erode both artifacts: the roadmap fills with rows nobody
intends to fund, and `BUGS` starts reading as a promise queue instead of honest disclosure.
FreeBSD man pages carry `BUGS` entries for decades, healthily, and that posture is the one this
tree copied on purpose.

## The three triggers, one of them mechanical

A limitation is promoted to a roadmap row exactly when one of these fires:

1. **Another milestone is blocked on it.** Forced by the machinery: gates cite only milestones,
   so a limitation something waits on must become a row or the dependency is invisible to
   `script/roadmap`'s classifier. The ladder argument: the gate is the mechanism, and a `BUGS`
   entry sits one rung below it.
2. **Fixing it requires a design fork** calef must rule on before any lane could start. This is
   the only case that lands as `RECORDED`; the row exists to hold the analysis while the
   decision waits (§26's signature variant is the shape).
3. **Someone proposes to spend on it and the spend needs coordination**: it spans lanes or
   components and cannot be a drive-by. Lands as `NOT-STARTED`.

Anything untriggered stays a `BUGS` entry indefinitely, and "wants a lane" is the strongest
phrasing it may carry. A lane may still take an untriggered limitation directly, small ones
skip the roadmap entirely; the row is for work that needs the roadmap's coordination, not a
toll both.

## The act of promotion

The integrator's, with a two-way citation: the minted block names the `BUGS` entry as its
origin, and the entry's "wants a lane" becomes "is milestone N", so neither record orphans the
other.

## The evidence it was already the practice

The day this was decided ran the convention three times unknowingly: the UART-IRQ limitation
went straight to a lane (trigger 3, small enough to skip a row), the canary flake was promoted
the moment a second failure signature appeared, and `net_stack`'s accept-backlog entries stayed
"eventually" because no trigger had fired. Writing down what the practice already is, is this
project's favorite kind of decision.

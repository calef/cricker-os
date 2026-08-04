# 40. A supervisor's death is its subtree's death; there is no reaper of last resort

**Status: DECIDED.**

**Decided 2026-07-30 (Chris), from the `disown` question in milestone 48.** When a supervisor dies,
its children die with it and the parent restarts the subtree. There is **no privileged process that
can collect any corpse in the system**, and there will not be one.

## Why Unix needs one and we do not

Unix reparents orphans to PID 1, which reaps them. That works, and it costs an **authority
concentration**: a singleton whose power is "collect any corpse anywhere". It exists to paper over a
structural gap, namely that an orphan has no owner.

Here there is no gap. **A child's resources come from its supervisor's region**, which is already the
shape of the supervision tree built in milestone 22 phase B.2: `root_supervisor` builds `spawner` and holds
its region; `spawner` builds children out of its own `WRITE`-only budget. Destroying `spawner`'s region
reclaims `spawner` *and everything built from it*, in one act, through §16's object revocation. There
is nothing left to reap individually, so the answer is **ownership rather than a reaper**.

Erlang/OTP reached the same design from the other direction: a dying supervisor takes its children with
it and the parent restarts the subtree, rather than orphans being adopted. Convergence from a different
tradition is worth something.

## The hole in §32 that makes this close to the only coherent answer

§32 authorizes `Endpoint::REAP` by checking that **the named thread's recorded `fault_ep` *is* the
endpoint being invoked**. If a supervisor dies and its endpoint dies with it, nobody can ever satisfy
that check for its children: they become permanently unreapable through the supervision path. The
memory is still reclaimed when the region is destroyed, so this is not a leak, but the supervision
route is closed.

Reparenting would therefore require the endpoint to outlive its holder *and* something to re-stamp
`fault_ep` on the orphans. That is kernel policy of exactly the kind §4 exists to refuse. The cascade
needs neither.

## Two caveats, recorded because they are load-bearing

- **The chain holds only if children are built from the supervisor's own region.** A supervisor that
  builds a child out of a region delegated from elsewhere breaks it: destroying the supervisor does not
  reclaim the child, and orphans are back. Whether that is *forbidden* or merely discouraged is not yet
  decided, and it should be before someone writes a supervisor that does it.
- **`disown` cannot mean what Unix means.** "Stop supervising but keep running" is incoherent when
  dropping the supervisor kills the subtree. It has to mean **transfer supervision upward**, to the
  shell's own supervisor, which is an explicit act with an obvious place to put it. That is a better
  answer than Unix's silent reparenting, because the transfer is visible and someone specific accepts
  responsibility.

## Rejected

- **A reaper of last resort (an init-like collector).** It is the authority concentration milestone 22
  spent its whole effort removing from init, reintroduced for a case that ownership already covers.
- **Corpse expiry (a timeout that collects the unreaped).** Time is not a principal. A corpse would be
  collected because a clock ran out rather than because anyone decided, and the postmortem §26's
  dead-until-reaped exists to permit would be racing a timer.

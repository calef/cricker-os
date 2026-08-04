# 66. Vaultwarden: somebody else's real application, running here

**Status: NOT-STARTED**, and this is the **largest single item on this roadmap**. It is recorded as a
target rather than a plan, and its value today is that it converts "runs real workloads" from a claim
into a checklist.

**Gate: DECISION, MILESTONE 64, MILESTONE 107.** 64 is named as the prerequisite and this is its
extreme case; 107 owns the listen and accept that head the gap table. The decision is the block's
own: which subset counts as running Vaultwarden has to be settled before the work starts, or the
goalposts move to wherever the effort lands.

## Why this application

Vaultwarden is a Bitwarden-compatible server written in Rust: self-hosted, widely deployed, and the
kind of thing Chris actually runs. It is **not a benchmark or a demo**. Getting it working would mean
this system runs software written by people who have never heard of it, which is the difference §14
draws between a demonstrator and a curiosity.

It also lands on the same board as milestones 53 to 55. A VisionFive 2 serving the family's Time
Machine backups **and** their passwords is a home server, not an exhibit.

## What is actually missing, measured

| Gap | State today |
|---|---|
| **TCP listen and accept** | **absent from the contract.** `socket_proto` has `OP_CONNECT`, `OP_SEND`, `OP_RECV`, `OP_CLOSE`. There is no way to be a server. |
| `std::thread` | 4 of 6 PAL functions answer `Unsupported` |
| `std::fs` | 32 of 54 answer `Unsupported` (milestone 64) |
| async runtime | none. Vaultwarden uses Rocket, which uses tokio: timers, wakers, and a reactor |
| TLS | none. `rustls` needs entropy (have it) and a large crypto surface |
| SQLite | a **C library**, so the §31 seam plus real filesystem locking |

**The listen/accept gap is the interesting one**, because it is a design question rather than missing
code. A listening socket is a *capability to accept connections on a port*, and `accept` mints a new
capability per connection. That is a genuinely new shape in this contract, and it is where the
capability model meets the server model for the first time.

## Its relationship to the rest

- **Milestone 64** is the prerequisite and this is its extreme case. 64 measures with small probe
  crates; this is what the measurements are eventually for.
- **Milestone 65** is a different thing wearing a similar word, and conflating them would be a
  mistake worth naming: 65 is a secrets service **for the system** (keys the OS computes with);
  Vaultwarden is a secrets service **for a human** (passwords a person retrieves). Different layers,
  different threat models, no shared machinery.
- **Milestones 53 to 55** share the board and the thesis.

## BUGS

- **This is a target, not a plan.** Every row in the table above is milestone-sized on its own, and
  several are unsequenced. Treating it as scheduled work would be dishonest about the distance.
- **"Runs Vaultwarden" is not one bit.** It could run with SQLite on a real filesystem and no TLS, or
  behind a TLS terminator, or single-threaded. **Which subset counts should be decided before the
  work starts**, or the goalposts will move to wherever the effort lands.
- **A capability system may not want to run it unmodified.** Vaultwarden expects ambient filesystem
  and network access. Running it here may mean granting it a directory and a listening socket and
  finding out what it does when it asks for more, which is a more interesting result than success.

**Effort: not estimated, and deliberately not.** The first honest deliverable is the sequence, not a
date.

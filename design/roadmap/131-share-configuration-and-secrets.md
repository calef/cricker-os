# 131. A share is configured, not compiled, and its secret arrives from somewhere

**Status: NOT-STARTED.** Minted 2026-08-17 by calef, from the question "how do we configure the
shares today?" asked while ruling on milestone 54's identity item. The answer was that we do not:
every fact about a share is a compile-time constant, and the deliverable he named in the same
breath was "a configuration file in the future along with a secret stored".

**Gate: DECISION.** The configuration half needs nothing and has a precedent to copy. The secret
half is a fork calef must rule on, stated in full below: **where a secret physically comes from on a
real boot.** The two are one milestone because they are one act, and because splitting them would
ship a document with a hole where the only value that matters goes.

## What "configured" means today, measured rather than characterised

Nothing is configurable. Changing anything about a share means editing Rust and rebuilding.

| Fact about the share | Where it lives today |
|---|---|
| Which behaviour (fixture, read-only, read-write, authenticated) | a `--features` flag choosing one of four numbered constants in `user/src/smb_server.rs` |
| What is served | the whole RedoxFS image; there is no "this directory is a share" notion at all |
| The identity | four constants in `cred_proto::fixture`: resource `backups-chris`, user `User`, domain `Domain`, password `Password` |
| Which resource the adapter authenticates against | the same constant, compiled in and **named in every request** |
| The port | `127.0.0.1:10445`, fixed, which is why two serve boots on one machine collide |

`--features smb_serve` wires `SHARE_FS_READ_WRITE` rather than `SHARE_FS_AUTHENTICATED`, and
`smb_server.rs`'s own header says why: **there is no way to tell that boot a password.** The
identity constants are [MS-NLMP] §4.2.1's published account on purpose, so the gate asserts against
numbers Microsoft printed rather than arithmetic this tree performed, and `cred_proto::fixture`'s
doc is explicit that they are a fixture and not a deployment: *"a real share's account and password
are somebody's, arrive through a provisioning path that does not exist yet, and must never be
these."*

**So this milestone is not "add a config file". It is the first time anything in this system is told
a fact at runtime that it currently learns at compile time**, and the secret is the hardest instance
of that.

## The precedent to copy rather than invent

**Milestone 55's mDNS responder already did the configuration half**, and it is the shape to follow:
`user/mdns_responder.conf` describes what the machine advertises, `crates/mdns_config` parses it
host-tested, and `notes/mdns.md` records the property that matters, that what it advertises "is
`user/mdns_responder.conf`, not compiled-in". A share document is the same move one service over.

## The four deliverables, in order

1. **A share configuration document.** Which directory, read-only or read-write, which resource
   authenticates it, and which port. Parsed by a host-tested crate in `mdns_config`'s shape, read at
   boot by whoever wires the adapter.

2. **A provisioning path**: how a secret reaches a running system's credential store. This is the
   fork, and it is below.

3. **Per-resource credential endpoints**, which fall out of (1) rather than needing their own
   argument. Today the adapter names its resource **in the request**, which is one authority more
   than it needs: it is choosing which record in the store to ask about, and nothing structural stops
   it asking about another. `caps` on that adapter cannot show which credential it reaches, because
   the answer is "whichever one it types". If a document says share `backups` uses resource
   `backups-chris`, then **init mints an endpoint that means exactly that credential**, the request
   carries only a challenge and a proof, and the name is not refused but unsayable, because there is
   no field to put it in. That is §27's "the endpoint IS the capability" carried through the one
   place `cred_proto` does not yet carry it, and its own header already argues the general case:
   *the endpoint gives a number its meaning, not the number itself.*

   **It retires a recorded limit for free**, which is usually the sign a shape is right. The
   one-verify-frame limit exists because one endpoint serves everyone; per-resource endpoints want a
   per-client frame anyway.

4. **`smb-serve` stops admitting guests.** Trivial once 1 and 2 exist, and **it is the deliverable
   calef asked to be tracked** (2026-08-17): *"Leave it guest-writable for now. We want to come back
   when we can to locking this down."* Until then the demo boot is guest-writable and says so in its
   banner, and identity is proven in the gate rather than in the demo.

## The fork: where does a secret come from

Milestone 56 built the store and the entropy; it is `BUILT` and it is not this. What is missing is
the path **into** it on a boot that is not a test. Four answers, with what each costs:

- **Typed at a console.** Honest, needs no new authority, and matches how a person expects to set a
  password. It requires an interactive boot to reach the credentialer, and it cannot serve a machine
  that boots unattended, which a backup target eventually is.
- **Read from a separate device** (a USB key, a second partition). Works unattended. Moves the
  question rather than answering it: the secret is now at rest somewhere this system does not
  control, and "who may read that device" is a capability question of its own.
- **Provisioned at image build.** Simplest, and it is what the test path does today. It means the
  secret is in the image, so the image becomes the secret, and every copy of it is a copy of the
  password.
- **Bound by measured boot**, sealed to the measurement milestone 22 already takes. Strongest, and
  the largest: it needs a sealing story this tree does not have, and it fails closed in ways that are
  hard to recover from at 2am.

**No recommendation is recorded here on purpose.** The choice is calef's, it is a fact that leaves
the machine (a secret's resting place is not un-decidable later), and this block exists so the
options are written down rather than settled by whoever implements first.

## Why it matters

**It is the head of the customer path.** Milestone 54 is `BUILT`: a real Mac mounts the share, reads
and writes byte-correct, sees real free space, walks subdirectories, and proves an identity against a
server that never holds the key. What stands between that and a machine calef's family backs up to is
not more SMB. It is that the share he would actually mount admits guests, because nothing can tell a
running system a real password.

## BUGS

- **This block names four deliverables and only the first is cheap.** Do not read the ordering as an
  estimate; (2) is a design fork and (3) is a wire change to `cred_proto`, which is the irreversible
  category.
- **A configuration document is a new parser reading attacker-adjacent bytes** if it is ever read
  from anywhere but the image. `crates/mdns_config` is host-tested and fuzzed; a share document
  should be held to the same standard on day one rather than after.
- **Nothing here fixes the fixed port.** `smb-serve` binds `127.0.0.1:10445`, so two serve boots on
  one machine still collide; the document is the natural place for it and this block does not require
  it.

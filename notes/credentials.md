# Credentials

An identity, a secret, and a way to check the second against the first without ever being able to
read it. Milestone 56's second half; the first half is [entropy](entropy.md), and this depends on
it for every salt it uses.

The contract is `crates/cred_proto`, the logic is `crates/cred`, the service is
`user/src/credentialer.rs`, and its clients are `user/src/credentialer_test_client.rs`.

## The problem this exists to solve

Milestone 55 wants a Mac to authenticate against an SMB share. SMB requires an identity and a
secret; this system had neither, and had no cryptography at all. Adding "a password file" is the
obvious move and it is the one that gives away the property this kernel is for.

Here is the tension, stated plainly, because it is the interesting part of the milestone:

> **A secret is a bearer token. A capability is an unforgeable reference. Once a component can
> *read* a password hash, it holds it forever and can copy it anywhere; knowledge cannot be
> revoked. Every other authority in this system can be.**

So the answer is not to protect the file better. It is to **hand out the operation, not the
secret**. A client gets an endpoint that means *"you may ask whether a secret is right"*. It never
gets the secret, the salt, the tag, or the list of who exists.

This is the fifth appearance of one idea, and the roadmap gives it its name: **attenuation by
operation, not by object**.

| milestone | the object | the attenuated power |
|---|---|---|
| 51 | the wall clock | an NTP client may **propose** a time, not set it |
| 51 (§43) | the clock page | read is a read-only mapping, set is a writable one |
| 56 | the virtio-rng | **obtain** bytes, without reaching the device |
| 56 | the credential store | **use** a credential, without reading it |
| 56 | the credential store | **write** it, only during a phase that ends |

## The shape

```text
   the provisioner ──the provision endpoint──►┌────────────┐
   (once, at boot)      (PUT, then SEAL)      │ credential │──►the entropy service
                                              │  service   │   (salts; an endpoint
   a client ────────the verify endpoint──────►└────────────┘    that names no device)
                    (VERIFY, forever)            the store lives here and nowhere else
```

Two endpoints. Two phases. The second phase never ends and the first one never comes back.

1. **Provision.** The service blocks on the provision endpoint. Each `PUT` derives a record with a
   salt drawn from the entropy service. `SEAL` ends the phase.
2. **Delete.** The service `cap_delete`s its receive end. The provisioner drops its send end.
   Between them, nothing in the system can name the object any more.
3. **Verify.** The service blocks on the verify endpoint, forever. One opcode. Yes or no.

### Why two phases and not two operations

Because **this kernel has one wait point.** There is no wait-any primitive and no threads inside
one address space, so a process can block on exactly one endpoint. `user/src/clock.rs` records the
same constraint and answers it differently: the clock's wide authority (set) is a page write rather
than a message, so the service only ever serves the narrow one.

A credential store cannot copy that, because "write the store" is not a memory operation you can
hand out as a mapping. But it does not need to: **writing the store is not an operation at all, it
is a phase.** Provisioning happens once, before any client exists, and then it is over. Making that
a phase rather than a privileged opcode is what turns "the service refuses to let you write" into
"there is no object through which the request could travel".

That distinction is the whole point and it is worth being precise about. A client is **not**
refused permission. By the time a client holds anything, `PUT` is a word arriving at a loop that
implements one opcode, and the endpoint that gave the word meaning has been destroyed.

Compare Samba: `smbd` opens the password database directly, so compromising it leaks every hash,
crackable offline and reusable wherever the password was reused. Here a compromised client can
guess passwords while it runs and cannot exfiltrate one, and revoking its endpoint ends the access.

## Argon2id, and why we depend rather than write

DECISIONS §46 draws the line at **exposure**: write the thing whose correctness you can argue from
the spec, depend on the thing whose correctness is won by many people checking it against published
answers over many years. A password KDF is squarely the second kind. So this takes a dependency,
and the amendment to §46 says depend rather than *vendor*: a vendored copy is invisible to
`cargo-deny` and `cargo-audit`, and crypto is the code that most needs to be visible to them.

**The crate is RustCrypto's `argon2` 0.5.3**, `default-features = false`, plus `subtle` for
constant-time comparison and `zeroize` so the library scrubs its own memory. The whole graph is
nine crates, all RustCrypto core, and it passes `deny.toml` unchanged: advisories, licences, bans,
sources. `argon2` was already in this tree, in the fs_server workspace, underneath RedoxFS's
encryption path, so the licence question had an answer before the question was asked.

**Argon2id specifically**, not Argon2i or Argon2d: it is RFC 9106 §4's recommendation, it is what
OWASP puts first, and it is the variant that resists both a side-channel adversary (the Argon2i
half) and a time-memory tradeoff (the Argon2d half). The alternatives were scrypt, which is the
older answer to the same question with no advantage here, and PBKDF2, which is not memory-hard at
all and would make a GPU attack cheap.

### The vectors are the point

A dependency whose answers you never check is a dependency you have merely hoped about. So
`crates/cred`'s tests run:

- **RFC 9106 §5.3**'s Argon2id vector (m=32 KiB, t=3, p=4, with a secret key and associated data);
- the **reference implementation's** vectors (phc-winner-argon2 `src/test.c`) at its two smallest
  memory settings, through `cred`'s own `kdf` function, so they pin our wiring (algorithm, version,
  tag length, and the no-allocation entry point) and not only the library's arithmetic.

If a version bump changes an answer, these fail before anything else does.

### A bug we found in the dependency

The exhaustive record-corruption test (every byte position crossed with four bit patterns) panicked
inside `argon2`. `Params::new` evaluates `m_cost < p_cost * 8` **before** it range-checks `p_cost`,
so a `p_cost` above `u32::MAX / 8` overflows the multiply.

- In **release** it wraps, the later bound check still fires, and the answer is correct.
- In **debug**, which is what `cargo xtask test` builds the userspace programs as, overflow checks
  are on and it **panics**.

A credential service that a cost value can kill is a login outage anybody can cause. `Cost::new`
therefore enforces Argon2's documented ranges itself, in an order that cannot overflow, before the
value crosses the boundary. This is DECISIONS §31's confine-what-you-did-not-write rule arriving at
a much smaller scale than a C component, and it is also the argument for sweeping every byte rather
than picking a few interesting ones: nobody would have hand-written that case.

### The cost parameters, and where they fall short

**Wired: m = 4 MiB, t = 3, p = 1.** That is **below** OWASP's recommendation of 19 MiB / t=2, and
the entry says so where the constant is defined rather than leaving it as an absence.

The reason is the machine. The whole system under test is 128 MiB of QEMU RAM, the filesystem
server alone reserves 8 MiB of it, and `kernel/src/user.rs` already records that three 8 MiB
untypeds do not fit. Three passes over 4 MiB is 12 MiB-passes of work against OWASP's 38, so this
is roughly a third of the recommended cost, not a token gesture, and it is genuinely memory-hard: an
attacker's GPU still has to find 4 MiB per guess, which is where Argon2's advantage over PBKDF2
lives.

**The parameters travel in the record, not in the code.** Raising them on real hardware is a
provisioning change plus a budget change, and both fail loudly rather than silently.

Measured on the development machine (Apple Silicon, native, not under QEMU):

| build | m=1 MiB t=2 | m=4 MiB t=3 | m=8 MiB t=3 |
|---|---|---|---|
| optimised | 2.0 ms | **5.3 ms** | 7.3 ms |
| unoptimised | 25.8 ms | 66.7 ms | 135.0 ms |

Which is why `Cargo.toml` compiles `cred`, `argon2`, `blake2` and `digest` at `opt-level = 2` even
in a debug build. That is `measured_boot`'s precedent (the measured-boot SHA-256) applied to a function
that costs an order of magnitude more, before TCG multiplies it again. Argon2 is memory-hard on
purpose; an unoptimised build does not make an attacker's job harder, only ours.

## Three things a naive verifier gets wrong

1. **The tag comparison is constant-time** (`subtle::ConstantTimeEq`). A verifier that returned on
   the first differing byte lets an attacker recover the tag one byte at a time.
2. **The identity lookup is constant-time too, and does not stop at the match.** `Store::select`
   scans every slot, conditionally assigns with `ConditionallySelectable`, and has no early exit. A
   scan that short-circuited would leak *which* identities exist, which is the same oracle by a
   slower route.
3. **A miss costs one full derivation.** An identity nobody provisioned lands on a **decoy** record
   whose salt and tag come from the entropy service at start-up, and the KDF runs against it. Without
   that, "no such user" returns in microseconds and "wrong password" returns in milliseconds, and the
   store's membership is readable with a stopwatch.

A miss and a wrong password are also the **same reply code** (`MISMATCH`). Distinguishing them
would turn the verify endpoint into an identity oracle: ask about a thousand names, learn which
three exist.

### How the constant-time claim is checked

Mostly structurally, and deliberately so. `Store::select` is branch-free, and a unit test pins its
behaviour at every slot position and on a miss: the decoy's salt, the decoy's tag, the store's cost,
`found = 0`. That is a deterministic test of the thing that matters.

There is also **one** timing test, and it is written to be robust rather than precise: 25 runs of
each kind, compare the **medians**, and fail outside a 0.4x–2.5x band. A machine under load inflates
both medians together, so the ratio is stable even when the numbers are not. The bug it exists to
catch is not subtle: an early exit on "no such identity" skips the whole KDF, which is a ratio near
zero. If it ever fails at 0.6, suspect the machine before the code, and re-run it quiet.

## The reply carries no data

Every reply is **one word, and the second word is always zero**. Not as a convention a future
opcode might relax: there is nothing about a credential store a caller is entitled to, so the reply
channel has no room for it. A service that answered a verify with the stored tag would be a
decryption oracle wearing a verifier's clothes, and the shape of the contract makes that a change
to the contract rather than a bug in a serve loop.

The reply codes are all small positives (1..=6), which is the trick `entropy_proto` established: every
failure the kernel can return from a `CALL` is one of its small negatives, which read as enormous
`u64`s. So `cred_proto::authenticated` can collapse "there is no credential service", "the request
was malformed", "the service died" and "wrong password" into one `false`, and no caller has to
remember which of six codes were the good ones. **A caller that mistook a missing capability for a
successful authentication would be the single worst bug this contract could permit**, so it is the
one made impossible by arithmetic rather than by care.

## The shared page, and what is left in it

Bulk rides in a page (DECISIONS §10) because an identity and a passphrase do not fit in two
registers. Two frames, never one: the provisioner writes **plaintext secrets** into its page, and a
client sharing that frame would read them.

**The service zeroes the request area after reading every request, on every path**, including the
malformed ones. So after an answer, the frame the client and the service share holds neither the
presented secret nor anything else. The wipe uses `write_volatile` and a compiler fence, because a
compiler that can prove nobody reads those bytes again is entitled to delete a plain store, and
"the optimiser removed the wipe" is the classic way a zeroing loop turns into a comment.

## EXAMPLES

### Provision three identities and seal the store

From a provisioner holding the provision endpoint in slot 0 and its page mapped:

```rust
use cred_proto as proto;

let page = /* the mapped provision page, 4096 bytes */;
for (identity, secret) in [
    (&b"chris"[..], &b"correct horse battery staple"[..]),
    (b"corinne", b"a different secret entirely"),
    (b"graeme", b"and a third one"),
] {
    let w0 = proto::place(page, identity, secret, proto::provision::PUT).unwrap();
    let (r0, _) = call(SERVICE, w0, 0);
    assert_eq!(r0, proto::OK);
}

// After this the provision endpoint is dead at both ends.
let w0 = proto::place(page, b"seal", b"seal", proto::provision::SEAL).unwrap();
let (r0, _) = call(SERVICE, w0, 0);
assert_eq!(r0, proto::OK);
```

### Authenticate a client

From anything holding the verify endpoint in slot 0:

```rust
let w0 = proto::place(page, b"chris", presented, proto::verify::VERIFY).unwrap();
let (r0, _) = call(SERVICE, w0, 0);
if proto::authenticated(r0) {
    // let them in
}
```

`authenticated` is the whole client-side API. There is no way to ask "does this identity exist",
"what is the salt", or "how many identities are there", because there is no message that would
answer.

### Check a secret without a service (host, in a test)

```rust
use cred::{Block, Cost, Store, Verdict};

let cost = Cost::new(256, 2, 1).unwrap();          // cheap, for a test
let mut scratch = vec![Block::default(); cost.blocks()];
let mut store = Store::<3>::new(cost, decoy_salt, decoy_tag);
store.put(b"chris", b"correct horse", salt, &mut scratch).unwrap();

assert_eq!(store.verify(b"chris", b"correct horse", &mut scratch).unwrap(), Verdict::Match);
assert_eq!(store.verify(b"chris", b"wrong",         &mut scratch).unwrap(), Verdict::Mismatch);
assert_eq!(store.verify(b"nobody", b"correct horse", &mut scratch).unwrap(), Verdict::Mismatch);
```

Note what is missing: there is no `store.get()`, no `store.record_for()`, no iterator. The absence
is the API expressing what the process boundary enforces.

## What is proven, and where

Host tests (`cargo test -p cred -p cred_proto`, milliseconds, no emulator):

- RFC 9106's and the reference implementation's **known-answer vectors**, through the same entry
  point the service uses.
- The right secret matches; the wrong one does not; an unprovisioned identity does not; and one
  person's password does not open another person's account.
- **Every single-byte corruption of an encoded record** is rejected or decodes to a different
  record. Exhaustive over positions and four bit patterns.
- A **hostile cost** is refused before it reaches the library (the overflow above).
- The lookup lands on the decoy for a miss and on the record for a hit, at every slot position.
- A miss and a hit take comparable time.

Proofs (`script/verify`, three Kani harnesses over `cred_proto`, 30 checks, 0.2 s). Both properties
are about what an adversary can send or receive, and an adversary is not limited to the values a
test author thought of:

- **No request word makes the server's parse read outside the page.** For every one of the 2^64
  first words a client can send, `read` either refuses it or returns two slices inside the page with
  exactly the lengths the word claimed. This is what lets the serve loop have no arithmetic in it
  that could go wrong.
- **Nothing but `MATCH` authenticates**, for every one of the 2^64 words a caller can receive. The
  host test sweeps a few dozen values around the boundary; this sweeps all of them.
- A request word round-trips its opcode and both lengths, over every combination `place` can build.

Guest tests (`kernel::user::credential_tests`, on aarch64 **and** riscv64, same assertions):

- **Provisioning fills the store and the seal closes it.** Three identities in, the fourth refused
  with `FULL` rather than silently replacing somebody, the seal accepted, and the service's
  readiness message arriving *after* the seal, which is the evidence that the provisioning loop was
  left rather than merely that a `SEAL` was answered.
- **A userspace client with one endpoint and no store** gets the right answer to four questions,
  over a real Argon2id verification with a salt drawn from a real virtio-rng.
- **The identical endowment cannot write the store.** The attacker holds exactly what the honest
  client holds and tries `PUT`, `SEAL`, an undefined opcode, and lengths outside the contract. All
  four are `MALFORMED`, and the credential it tried to install does not work.
- **The service survives all of it** and answers correctly afterwards, because a credential service
  a malformed request can kill is a login outage anybody can cause.
- **The kernel reads the shared frame through the direct map**, which no userspace program can do,
  and finds neither the presented secret nor any nonzero byte.

## BUGS

Named here rather than in a tracker, because a reader who meets the feature should meet its limits
in the same place.

- **Nothing survives a reboot.** The store is memory only, provisioned at boot. Secrets at rest is
  the open question and it is the same chicken-and-egg as milestone 51's NTS problem: encrypted
  under what key, held where? `cred::Record` has a versioned encoding with a round-trip test
  precisely so that question has a starting point, but nothing in the tree writes one to a disk and
  this note does not imply a durability we do not have.
- **This cannot serve NTLMv2, and that is a real gap rather than a detail.** NTLMv2's
  challenge-response requires the server to hold the **NT hash** (MD4 of the UTF-16LE password) and
  compute HMAC-MD5 over it. An Argon2id tag cannot produce that; the two derivations are different
  functions of the same password. So milestone 55 needs the credential service to hold a *second*
  derivation and serve a *second* operation ("here is a challenge, give me the response"), which is
  the use-not-read pattern this file is built for but is not code that exists yet. It also means
  shipping MD4 and MD5, known-broken primitives, on purpose and for wire compatibility. **That is
  the next piece of this milestone and it was deliberately not started here**, because the
  credential primitive and the SMB compatibility layer are separable and only one of them requires
  choosing to ship a broken hash.
- **One verify page means one client at a time.** The page is per service, not per channel, so two
  clients sharing the endpoint would share the frame each writes its presented secret into. Nothing
  detects that. `fs_proto`'s answer (one page per channel) is the shape to copy when a second client
  exists; today the intended client is the single SMB adapter.
- **No rate limit, no lockout, no attempt counter.** A client holding the verify endpoint can guess
  as fast as it can `CALL`. Each guess costs the service one Argon2id derivation, which is the only
  thing slowing an online attack down and is also a way to make the service unresponsive to
  everyone else. A store with three identities and human-chosen passwords is not safe against an
  unlimited online guesser at any KDF cost.
- **The cost parameters are below OWASP's**, for the reason given above. On real hardware they
  should be raised, and nothing currently checks that they were.
- **No rehash on verify.** When the cost parameters move, existing records keep their old ones. The
  encoding carries per-record parameters so this is implementable; it is not implemented. A
  consequence: if two identities were ever provisioned at *different* costs, the verify time would
  distinguish them. Nothing today can produce that, because a store has one cost and `put` uses it.
- **The identity is an opaque byte string and nothing more.** No uid, no group, no home directory,
  no session, no login. DECISIONS §49 (users, login, and attribution) is a different milestone and
  this one deliberately does not start it. What is built here is the credential *primitive*; who
  gets to ask, and what an answer of "yes" then permits, is §49's question. The roadmap's answer,
  which this does not contradict: the adapter authenticates the client because the protocol demands
  it, then uses the directory capability it already holds. **Identity never becomes ambient
  authority.**
- **The provisioner's plaintext exists somewhere.** Provisioning takes a secret in the clear, so
  whatever hands the provisioner its passwords is holding them in memory. Today that is a test
  program with the strings compiled in, which is fine for a test and is not a deployment. Where a
  real deployment's passwords come from is unanswered and is part of the secrets-at-rest question.
- **The decoy is one record.** Every miss derives against the same salt, so an attacker who can
  time verifies precisely enough to distinguish *two* misses from a miss and a hit learns nothing,
  but an attacker who can observe the service's memory access pattern is outside this threat model
  entirely, and so is one who shares a core with it. There is no defence here against a local
  side-channel adversary.
- **The kernel-side test driver is not a capability holder.** The strongest form of "no capability
  to the provision endpoint exists" is demonstrated by the userspace clients' endowments (they hold
  only the verify endpoint), not by the test harness, which names endpoints by id and reaches around
  the capability system by construction. The claim is true of the system; the test proves it of
  userspace.

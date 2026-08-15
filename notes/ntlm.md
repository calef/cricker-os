# NTLMv2, and the operation a secret exposes

Milestone 65: **hold the key, expose the operation, never the key.** The half of the secrets
service that lets an SMB server authenticate a Mac without ever holding the thing that
authenticates it.

The arithmetic is `crates/ntlm`, the store is `crates/cred`, the wire contract is
`crates/cred_proto`, and the service is `user/src/credentialer.rs`. The password half of the same
store is [credentials](credentials.md), and every salt in it comes from [entropy](entropy.md).

## The observation that made this a milestone

Milestone 56 wrote down the principle: *hand out the operation, not the secret.* It then assumed
the operation, and the assumption was wrong.

**NTLMv2 does not verify a presented secret.** The client never sends the password, and the server
never receives anything it can compare against a stored tag. The server holds a key, computes a MAC
over a challenge it chose, and compares that to the MAC the client sent. So the NT hash is not a
verifier, it is **a key the server computes with**, and "secret in, boolean out" does not describe
the operation at all.

That is the whole reason 65 exists as a milestone rather than as a second opcode on the
credentialer. The principle survived; the shape did not.

## What the service holds, and what it computes

```text
  password ──MD4(UTF-16LE)──► NT hash ──HMAC-MD5 over UPPER(user)||domain──► NTOWFv2
                                                                               │
     ····························································  provisioning ends here
                                                                               │
  server challenge (8) ┐                                                       │
  the client's blob    ├──────────────HMAC-MD5 keyed by NTOWFv2────────────────┤
                       ┘                                                       ▼
                                                                         NTProofStr (16)
                                                                               │
                                   HMAC-MD5 keyed by NTOWFv2 over NTProofStr ──┤
                                                                               ▼
                                                                    SessionBaseKey (16)
```

Everything above the dotted line happens once, at provisioning, from a password the provisioner
holds in the clear. Everything below happens per authentication, from inputs that are entirely
public.

**The store holds `NTOWFv2`, not the NT hash**, and that is a design decision rather than an
implementation detail. Two things follow.

The account name and the domain are **bound at provisioning time**, so a caller of the runtime
operation cannot choose them. A caller that could would be choosing half the key derivation, which
is how "compute a proof" quietly becomes "compute a proof for `Administrator`". The runtime request
therefore carries only public inputs: a resource name, a challenge, a blob, and a proof.

And a stolen `NTOWFv2` authenticates as **one account in one domain**, where a stolen NT hash
authenticates as that account anywhere the password was reused. Reuse is what normally makes
password-equivalent storage dangerous, and per-resource scoping cuts it at the root.

## The operation, and why it is folded

Milestone 65's own table names the operation `ntlm_response(challenge) -> response`. What shipped
is `NTLM_PROOF`: *is this the proof a holder of the password would have computed, and if so, here
is the session key.* The proof is computed inside and compared inside; it never leaves.

Folding the comparison into the operation is the same move `VERIFY` makes for a password, and it is
strictly stronger, because anything that can obtain the expected proof can do the comparison
itself. The unfolded version is what an SMB **client** needs, and nothing in this tree is one.
Milestone 55 is a server: calef's Mac connects to cricker-os as a Time Machine target.

If an SMB client ever arrives, the unfolded operation is a new opcode and a decision to make on
purpose, not a gap to fill in quietly.

## What crosses the boundary

This is the question DECISIONS §25 forces, because the frame is the real granted resource: a
secrets capability that hands back a shared frame full of key material has defeated itself.

**Never, under any opcode, in any reply, in any page:** the Argon2id salt, the Argon2id tag, the NT
hash, `NTOWFv2`. There is no message that would return one. There is also no message that answers
"does this resource exist", "which kinds of secret does it have", or "how many are there".

**Crosses, once, on a match:** the 16-byte `SessionBaseKey`, written into the shared page at
`cred_proto::SESSION_KEY_OFF`.

It crosses because an SMB server that authenticates a session and cannot sign it cannot serve SMB2,
so an operation that returned only a boolean would be useless for the thing it exists to serve. It
is defensible because the value is a function of the stored key **and** of a challenge this server
chose and a client challenge inside the client's blob: it is per-session, it signs one session, and
recovering the key it came from is an HMAC key recovery.

**It is released only against a proof that verified.** A caller of this endpoint cannot manufacture
one, because manufacturing one is exactly the thing the key it does not hold would let it do. So
the rule is not "ask nicely and receive a key", it is "prove that a real client authenticated, and
receive that session's key".

**A stricter design exists and is named rather than pretended away:** keep the session key inside
too and expose *signing* as a further operation. That is the same move one level up, it is where
milestone 65's third row (a signing key) would take this, and it is not what shipped.

## Two hazards the store handles that a naive one would not

**A record with no NTLM half must never answer.** The first design left its key as sixteen zero
bytes, which is a key an attacker **knows**: anyone could compute the proof under it and be told
`MATCH`. Filling it with entropy instead works and was the second design, but it makes every
provisioning path need a randomness source for a field nothing reads. What shipped is a `has_ntlm`
flag, folded into the verdict **after** the MAC has run, so it costs no branch anybody can time.
`cred`'s tests present exactly that forgery and require the answer to be no.

**A refusal must cost what an acceptance costs.** The session key is derived on every well-formed
request and then zeroed by conditional assignment rather than skipped by a branch, and the lookup
that finds the record is the same branch-free scan the password half uses. A miss on a resource
nobody provisioned and a wrong proof on one that exists are the same reply and the same work.

## Three broken primitives, on purpose

MD4, MD5 and HMAC-MD5 are what NTLMv2 specifies. Shipping them is **protocol compliance, not a
security choice**, the way implementing DES to talk to old hardware would not be an endorsement of
DES. What matters is what is stored and what is claimed about it.

They arrive as dependencies, which is DECISIONS §46 applied unchanged: depend rather than vendor,
so `cargo-deny` and `cargo-audit` can see the graph, and **make the specification's own answers the
tests**. `md4` 0.10, `md-5` 0.10 and `hmac` 0.12 add four crates to a graph that already had
`digest` under Argon2, and `script/supply-chain` passes unchanged: advisories, licences, bans,
sources.

The blast radius, stated plainly: **a record with an NTLM half is crackable offline at roughly the
speed of MD4**, whatever the Argon2id cost beside it says. Provisioning one lowers that record's
offline strength to the weaker of its two derivations. That is the price of speaking NTLMv2, it is
bounded by scope rather than by strength (a cracked key opens one share), and `Record::derive_ntlm`
says so where the choice is made.

## The vectors are the point

A dependency whose answers you never check is a dependency you have merely hoped about. So:

- **RFC 1320**'s full MD4 test suite, seven vectors. The first, `MD4("")`, is the NT hash of the
  empty password.
- **RFC 2202** §2's HMAC-MD5 vectors, the three whose keys are 16 bytes or shorter, which is every
  key NTLM ever uses.
- **[MS-NLMP] §4.2.4**, the NTLMv2 authentication example, which publishes every intermediate value
  in the chain: the NT hash of `Password`, `NTOWFv2` for `Domain\User`, `NTProofStr`, and
  `SessionBaseKey`.

The last one is the one that pins **our wiring** rather than the libraries' arithmetic: the UTF-16LE
encoding, which of the two names is uppercased, the order of the challenge and the blob, and the key
each HMAC runs under. It is transcribed twice on purpose, once in `crates/ntlm` and once in
`crates/cred`, because the two crates should be checked against the published document and not
against each other.

**A correction worth keeping.** The first transcription of the blob carried four extra trailing
zeros, and every published value up to `NTOWFv2` still matched, because the blob does not enter the
chain until the proof. The machine caught it at exactly one assertion, where it looked like a bug in
the code under test. A vector transcribed by shape rather than by count is a vector that lies.

## EXAMPLES

### Provision a share, then seal the store

From a provisioner holding the provision endpoint in slot 0 and its page mapped. A share is a
resource name, a password, and the account name and domain the key is bound to.

```rust
use cred_proto as proto;

let page = /* the mapped provision page, 4096 bytes */;
for (resource, password, user, domain) in [
    (&b"backups-chris"[..],   &b"Password"[..], &b"User"[..],    &b"Domain"[..]),
    (b"backups-corinne",      b"another share secret", b"corinne", b"WORKGROUP"),
    (b"backups-graeme",       b"a third share secret", b"graeme",  b"WORKGROUP"),
] {
    let (w0, w1) = proto::place_ntlm_put(page, resource, password, user, domain).unwrap();
    let (r0, _) = call(SERVICE, w0, w1);
    assert_eq!(r0, proto::OK);
}

let w0 = proto::place(page, b"seal", b"seal", proto::provision::SEAL).unwrap();
call(SERVICE, w0, 0);
```

The account name and the domain go in the **second** request word's length fields; they are the
only fields in this contract that do.

### Answer a client's authentication

From an SMB server holding the verify endpoint in slot 0. It has the challenge it issued and the
`NtChallengeResponse` the client sent, which is `NTProofStr || temp`, so it already has both halves
and never needs the service to split them.

```rust
let (proof, blob) = ntchallengeresponse.split_at(proto::KEY_LEN);
let w0 = proto::place_ntlm_proof(
    page,
    b"backups-chris",
    &server_challenge,          // the 8 bytes we put in the CHALLENGE_MESSAGE
    blob,                       // the client's temp, unexamined
    proof.try_into().unwrap(),
).unwrap();

let (r0, _) = call(SERVICE, w0, 0);
if proto::authenticated(r0) {
    let session_key = proto::session_key(page).unwrap();
    // ... derive the SMB2 signing key from it, sign the session ...
}
proto::wipe(page);              // the key is ours now; it should not outlive the exchange
```

`authenticated` is the whole client-side API for the verdict, and it collapses "there is no secrets
service", "the request was malformed", "the service died" and "the proof is wrong" into one
`false`. Testing whether the session key is nonzero instead would be a second, weaker
authentication check sitting beside the real one.

### Check the chain without a service (host, in a test)

```rust
let key = ntlm::ntowfv2(b"Password", b"User", b"Domain").unwrap();
let proof = ntlm::proof(&key, &server_challenge, &blob);
let session = ntlm::session_base_key(&key, &proof);
```

Note what is missing from the service's API and present here: there is no `store.ntowfv2_for()`,
no way to ask the store for a key. `crates/ntlm` is a function of inputs a caller already has;
`crates/cred` is where the secret lives, and it has no getter.

## What is proven, and where

Host tests (`cargo test -p ntlm -p cred -p cred_proto`, milliseconds, no emulator):

- The **published vectors** above, through the same entry points the service uses.
- A different challenge, an edited blob, or a proof with any single byte flipped is a mismatch, and
  releases no session key. Swept over every byte position of the blob and of the proof.
- A **proof for a different account or domain** does not open the account the key is bound to.
- A record with no NTLM half refuses a proof computed under the key of zeros it holds, and is
  indistinguishable from a resource nobody provisioned.
- One password, two derivations: a share provisioned for NTLM still answers an ordinary verify.
- An NTLM record survives its encoding and still answers, and **every single-byte corruption** of
  one is rejected or decodes to a different record. Exhaustive over positions and four bit patterns.
- The page layout does not overlap itself, and fits in a page (a `const` assertion, so a build that
  runs no test still cannot get it wrong).

Proofs (`script/verify`, five Kani harnesses over `cred_proto`, 30 checks, 0.16 s; two are new):

- **No request word makes the NTLM parse read outside the page**, for every one of the 2^64 first
  words a client can send. This matters more than the password parse's twin, because the NTLM
  request is larger and every field in it is attacker-supplied.
- **No pair of words makes the provisioning parse read outside the page**, over both words, because
  the account name and the domain have their lengths in the second one.

Guest tests (`kernel::user::credential_tests`, on aarch64 **and** riscv64, same assertions):

- **A userspace program with one endpoint, no key, no store and no entropy authenticates a session**
  against [MS-NLMP] §4.2.4's published proof and comes away with §4.2.4.1.2's published session key.
  Nothing in the test or in the program under test computes an expected value.
- A proof with one bit flipped is refused **and publishes no session key**.
- A password-only identity and a resource nobody provisioned answer identically.
- **The kernel reads the shared frame through the direct map**, which no userspace program can do,
  and finds neither the stored key, the session key, nor the presented proof.

## BUGS

Named here rather than in a tracker, because a reader who meets the feature should meet its limits
in the same place.

- **It does not protect against an attacker who holds the endpoint right now.** They can
  authenticate sessions for as long as they hold it, and obtain a session key for each one. The
  claim is that compromise is *bounded and revocable*, not that the key is safe from a live
  intruder. What it buys over Samba is that the key cannot be extracted, cracked offline, or
  carried anywhere else, and that revoking the endpoint ends the access.
- **Revocation is per holder, not per secret.** Destroying a client's endpoint cuts it off. Revoking
  one *secret* is a different question and this service cannot answer it: the store is sealed, so
  there is no object through which a record could be removed, which is the same property that makes
  the seal worth having. Rotating a share's password means restarting the service and
  reprovisioning, which restarts every other secret with it. A deployment that needs finer
  granularity runs more than one service, which is cheap here and is the shape the capability model
  suggests anyway. See [credentials](credentials.md) for the seal's argument.
- **`NTLM_PROOF` has no rate limit and, unlike a password verify, no incidental one either.** An
  NTLM answer is three MD5-shaped operations, roughly four orders of magnitude cheaper than an
  Argon2id derivation, so the KDF's accidental throttling does not apply. Online guessing against it
  is useless (a guess is a 128-bit MAC, not a password), but it is a free way to spin the one
  service that answers every login on the machine.
- **The uppercasing is ASCII-only.** [MS-NLMP] says `Uppercase(User)` and Windows means its own
  locale-aware uppercasing. Doing that properly needs Unicode case tables this tree does not carry.
  An account name outside ASCII derives a different key here than on Windows and fails to
  authenticate: a wrong answer rather than a crash.
- **No replay detection.** The service does not remember challenges, so if a caller reuses a
  challenge it already issued, the same proof verifies again and yields the same session key.
  Freshness is the SMB server's job (the challenge is its to choose, from the entropy service), and
  nothing here checks that it did it. A stateful service could, and would then need a table whose
  size is a denial-of-service question.
- **Nothing survives a reboot.** The store is memory only, provisioned at boot. Secrets at rest is
  the open question, and it is sharper for a key than for a password verifier: an `NTOWFv2` written
  to a disk is a password-equivalent secret at rest, where an Argon2id tag is a one-way image.
  `cred::Record`'s encoding is versioned (version 2 carries the NTLM half) so the question has a
  starting point, and nothing in the tree writes one to a disk.
- **The provisioner holds every password in the clear.** Provisioning takes a password, not a
  derived key, because the service derives both halves itself. Today that is a test program with
  the strings compiled in, which is fine for a test and is not a deployment.
- **SMB3 needs more than this.** Signing is AES-CMAC, encryption is AES-CCM or GCM, and SMB 3.1.1
  preauth integrity is SHA-512. None of them is here. What is here is the authentication step and
  the key the rest would hang off.
- **`crates/ntlm` is not constant-time and does not need to be**: its inputs are either public or
  keys whose bytes never branch. The one comparison that must be constant-time is the proof
  comparison, and it lives in `cred` next to the store, where `subtle` already is. There is no
  defence here against an adversary who can observe the service's memory access pattern or shares a
  core with it; that is outside this threat model, as it is for the password half.

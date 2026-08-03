# 56. Secrets, credentials, and the entropy to make them safe

**Status: BUILT.**

**In brief.** Milestone 55 needs the Mac to authenticate, so it needs an identity, a secret, and
unguessable challenges. We had none of the three, and one of the gaps was a hard blocker rather than
a gap. **Prerequisite for 55; feeds milestone 49 (users, login, and attribution).** Chris's existing setup
serves **three** family members with separate passwords, so the credential service holds multiple
identities from the start rather than growing into that later.

**Status: both halves built** (entropy 2026-07-30, credentials 2026-07-31). All three gaps are
closed: unguessable bits come from a virtio-rng behind a capability, and an identity plus a secret
you can check and cannot read is a service with five kernel tests on both ISAs. **What remains is
the SMB-specific derivation** (the NT hash and HMAC-MD5, so the service can answer a challenge
without the adapter seeing the hash), and it is described at the end of this entry.

## The entropy half: BUILT, 2026-07-30 (DECISIONS §44, notes/entropy.md)

The RNG used to be splitmix64 seeded from the virtual counter, predictable to anyone who could guess
boot-relative time, which blocked SMB authentication outright: an NTLMv2 server challenge that is
guessable is precomputable. That file has been **replaced, not patched**, exactly as its own last
paragraph said it should be.

What shipped: a **virtio-rng driver over both transports** (mmio and PCIe, §18's seam, one binary),
inside an **entropy service** that is the only thing in the system that can read the device. Clients
hold one endpoint that means *"you may obtain randomness"* and names no device, which is the fourth
appearance of attenuation by operation rather than by object. The service passes the device's bytes
through and computes nothing, because whitening without a one-way function is a reversible
permutation that obscures the claim rather than strengthening it. **The fork is settled**:
`std::random` improves transparently, split on std's own seam, so `SystemRng` (which promises
cryptographic strength) panics when the capability is absent while `HashMap`'s seed degrades to the
old stream and says so. Proven on aarch64 and riscv64, over both buses, plus a std program drawing
through the PAL.

What it does **not** promise: under QEMU the device is backed by the host's `/dev/urandom`, which is
a fact about the emulator. On real silicon the StarFive JH7110's TRNG is the candidate and **needs
verifying** before it is relied on, and there is no health test, so a device that started returning a
constant would be passed straight through. notes/entropy.md carries the full list.

## The credential half: BUILT, 2026-07-31 (notes/credentials.md)

An identity, a secret, and a way to check the second against the first without ever being able to
read it. `crates/cred` (Argon2id, the store, constant-time verification), `crates/cred_proto` (the
wire contract), `user/src/credentialer.rs` (the service), `user/src/credentialer_test_client.rs` (its provisioner,
client, and attacker). Five kernel tests on both ISAs, 26 host tests, three Kani harnesses.

**The bearer-token problem below is answered, and the answer is sharper than "hand out the
operation".** Writing the store is not an operation at all, it is a **phase**: the service serves a
provision endpoint until `SEAL`, then deletes its receive end while the provisioner drops its send
end. Nothing in the system can name the object afterwards, so a client is not refused permission to
write the store; there is no object through which the request could travel. That shape was forced by
a real constraint (this kernel has one wait point, so a process serves one endpoint) and turned out
to be better than the guarded-opcode design it replaced.

**Argon2id, as a dependency, from RustCrypto** (§46's amendment: depend, do not vendor, because a
vendored copy is invisible to cargo-deny and crypto is what most needs to be visible to it). RFC
9106's and the reference implementation's known-answer vectors run against the version we link, which
is the whole point of depending. The exhaustive record-corruption test found a **debug-build overflow
panic inside argon2 0.5.3**: `Params::new` multiplies `p_cost * 8` before range-checking `p_cost`, so
`Cost::new` enforces the bounds before the value crosses the boundary.

Honest gaps, in full in notes/credentials.md: the cost parameters are **below OWASP's** (4 MiB rather
than 19, because the whole machine is 128 MiB of QEMU RAM); nothing survives a reboot; one verify
page means one client; there is no rate limit or lockout. And the one that matters for milestone 55,
below.

## The thing we still do not have

~~**There is no crypto in the tree at all.**~~ There is now: RustCrypto's `argon2`, `blake2` and
`subtle`, via the credential half above, plus the precedent for how a crypto dependency enters (a
`deny.toml`-clean graph and the specification's own test vectors as tests). What remains is the SMB
side, and it is unchanged in substance: NTLMv2 needs MD4 (the NT hash) and HMAC-MD5; SMB3 signing
needs AES-CMAC; encryption needs AES-CCM or GCM; SMB 3.1.1 preauth integrity needs SHA-512.

**The credential service cannot serve NTLMv2 yet, and this is the next piece rather than a detail.**
NTLMv2's challenge-response requires the server to hold the **NT hash** and compute HMAC-MD5 over
it; an Argon2id tag cannot produce that, because the two are different functions of the same
password. So the store needs a second derivation and the service a second operation ("here is a
challenge, give me the response"), which is exactly the use-not-read shape already built and is not
code that exists. It also means shipping MD4 and MD5 on purpose. The credential primitive and the
SMB compatibility layer are separable, and only one of them requires choosing to ship a broken hash,
which is why the split fell here.

## Identity lives at the boundary, and stops there

Milestone 49 records that identity is not authority here. SMB requires an identity, and the two
reconcile without compromise: **the adapter authenticates the client because the protocol demands it,
then uses the directory capability it already holds.** Identity never becomes ambient authority
inside the system, which is 49's login model exactly: authentication produces or permits the use of
capabilities rather than setting a field.

The consequence is worth stating plainly because it is the security claim: compromise the SMB adapter
and you get the share it holds. You do **not** get "authenticated as user X" with powers elsewhere,
because there is no elsewhere and no user X.

## The hard part: a secret is a bearer token, a capability is an unforgeable reference

This is the genuinely new problem and it is a real tension in the model. Once a component can **read**
a password hash it holds it forever and can copy it anywhere; **knowledge cannot be revoked**. Every
other authority in this system can be.

**The answer is to hand out the operation, not the secret.** A credential service holds the NT hash
and computes the HMAC on request: the adapter sends a challenge and receives a response, and never
sees the hash. So the adapter holds a capability to **use** a credential, not to **read** one.

That is an improvement over the reference implementation rather than a reframing of it. In Samba,
`smbd` reads the password database directly, so compromising it leaks every hash: crackable offline,
reusable wherever the password was reused. Here a compromised adapter can use the credential while it
runs and cannot exfiltrate it, and revoking the capability ends the access.

**This is the third appearance of one pattern**, and it should be named as a principle rather than
rediscovered a fourth time: the NTP client that may *propose* a time but not *set* it (milestone 51),
the clock's read / set / propose ladder (§43), and now use-but-not-read. **Attenuation by operation,
not by object.**

**Built 2026-07-31, and the answer went one step further than this entry expected.** "Hand out the
operation, not the secret" is right for *reading*, and it is what the verify endpoint is. But
*writing* the store turned out not to need an attenuated operation at all: it is a **phase**, and
the phase ends. The provision endpoint is deleted at both ends at the seal, so there is no narrow
write operation to hand out and no wide one to withhold. The forcing constraint was that this kernel
has one wait point per process, which is the same wall the clock service hit (§43) and answered
differently; the answer here is better, because "the object no longer exists" is a stronger claim
than "the service checks". See notes/credentials.md.

## Decisions to make before building

- **Take the crypto as a dependency, do not write it and do not vendor it** (§46, amended
  2026-07-31: vendoring is for what must be patched, and RustCrypto needs no patch; a vendored copy is
  also invisible to `cargo-deny`/`cargo-audit`, which is the one thing crypto most needs). Its crates are `no_std` and reviewed, and the
  supply-chain tooling from milestone 44 (`deny.toml`, `script/supply-chain`, `script/vendor-verify`)
  already exists for exactly this shape. Writing our own AES or SHA is a bad idea and the entry should
  say so rather than leaving it open. **Done for the KDF, 2026-07-31**: `argon2` 0.5.3 plus `blake2`,
  `subtle` and `zeroize`, `default-features = false`, nine crates, `deny.toml` clean unchanged. The
  discipline that came with it and should hold for the SMB primitives too: **the specification's own
  test vectors are tests**, because a dependency whose answers are never checked is one we have merely
  hoped about, and **the bounds get re-checked at our boundary**, because argon2's `Params::new`
  panics in a debug build on a large `p_cost` and a service a cost value can kill is a login outage.
- **We will be shipping known-broken primitives on purpose.** MD4 and MD5 are required by NTLMv2 for
  wire compatibility. Record that as a deliberate compatibility cost with its blast radius stated, not
  as an oversight, and keep them out of anything that is not SMB.
- **Secrets at rest are unsolved and should be scoped small.** Where does the hash live across
  reboots, and encrypted under what key? That is the same chicken-and-egg as milestone 51's NTS
  problem (certificates need time, time needs the network). The honest v1 is provisioned at boot and
  held only in memory; say so plainly rather than implying durability we do not have. **Still
  unsolved, and scoped exactly that small 2026-07-31**: the store is memory only and dies with the
  process. `cred::Record` has a versioned encoding with a round-trip test so the question has a
  starting point, and nothing writes one to a disk.
- ~~**Entropy is a capability**, and the service that holds it should be the only thing that can read
  the device. Whether `std::random` transparently improves or programs must ask for a real RNG is a
  design fork.~~ **Settled and built 2026-07-30**, DECISIONS §44: transparent, split on std's own
  `fill_bytes` / `hashmap_random_keys` seam, so the caller that promises cryptographic strength
  refuses rather than degrading. The service passes bytes through and does not pool or whiten.

**Sequencing.** Before milestone 55. **Both halves are done** as of 2026-07-31: entropy on 07-30, the
credential store and its service on 07-31. Each was worth doing on its own, and each was testable in
QEMU with no board.

**What is left of this milestone** is the SMB-facing derivation: the NT hash, HMAC-MD5, and a second
service operation that computes a challenge response without the adapter ever seeing the hash. That
is the use-not-read pattern already built, applied to a second secret, and it is the first place this
project chooses to ship a known-broken primitive. Secrets at rest remains unanswered and is not on
milestone 55's critical path, because provisioning at boot is enough to authenticate a Mac.

# 65. A secrets service: hold the key, expose the operation, never the key

**Status: BUILT.** Merged 2026-08-04 (pull request #125; the `cred` crates and credentialer are on main). The status said IN-PROGRESS for eleven days after the merge, found 2026-08-15 alongside milestone 107's identical staleness. Raised 2026-08-01, from a question about MD4 and MD5 that turned out to be
asking something else.

**Gate: NONE.** Deliberately decoupled from milestone 49's identity model, because a secret is
keyed by what it authenticates to rather than by who holds it. Persistence, and the secrets-at-rest
question under it, is named as the real open question rather than as a blocker.

## The observation that reframes it

**NTLMv2 does not verify a presented secret.** The client never sends the password. The server takes
a challenge, computes `HMAC-MD5(NT hash, ...)` **itself**, and compares. So the NT hash is not a
verifier, it is **a key the server computes with**, and §54's shape (secret in, boolean out) does not
fit it at all.

The principle §54 states is still right: *hand out the operation, not the secret*. It is the
**operation** that was wrong.

## What the service is

A process that **holds secrets and exposes keyed operations**, never the secrets:

| Secret kind | Operation exposed | Never exposed |
|---|---|---|
| Argon2id tag | `verify(presented) -> bool` | the tag |
| NT hash | `ntlm_response(challenge) -> response` | the hash |
| a future signing key | `sign(bytes)` | the key |

**The credentialer becomes one operation in this service**, not a separate thing. A second service
holding secrets is precisely what this design exists to avoid.

## Why it is worth a milestone rather than a second operation on the credentialer

Because of what it does to the SMB server. **The NT hash never enters that address space.** The SMB
server holds an endpoint that computes responses; compromise it and an attacker can authenticate
sessions *while they hold the endpoint*, and cannot extract the hash, crack it offline, or carry it
anywhere else. Storing the hash in the SMB server offers none of that.

And **revocation already exists** (§32, §41): destroying the endpoint cuts a compromised server off.
A stored hash could never be taken back.

**Prior art, and it is strong.** This is what a TPM or an HSM *is*: hold the key, expose operations,
never emit the key. It is macOS Keychain's model and `systemd-creds`'s. Structurally it is
`libcasper` a third time, which is the convergence §31 already records: a process holding authority
its caller should not have, serving a narrow interface.

## Secrets are scoped to resources, not identities

calef's setup is the evidence: **each share has its own username and password.** That is a
credential per resource, which is a capability per resource, and it means this service **does not
depend on milestone 49's identity model**. Secrets are keyed by what they authenticate *to*, not by
who holds them.

That decoupling is deliberate and worth keeping: an identity model can arrive later and consume this
service, rather than this service having to wait for one.

It also bounds the damage from the NT hash's password-equivalence, structurally rather than by
policy. A leaked hash authenticates to **one share** and nothing else, because there is nothing else
it is the credential for. What normally makes password-equivalent storage dangerous is **reuse**, and
per-resource scoping cuts that at the root.

## Dependencies

- **Entropy (§44, built).** Challenges, nonces and salts. Already wired.
- **Crypto as dependencies (§46).** `argon2` is in the tree; NTLM adds MD4, MD5 and HMAC-MD5, and
  §46's exposure argument applies to them exactly as it did to Argon2id.
- **Persistence, which is the real one.** The store is **memory only, provisioned at boot**
  (`notes/credentials.md`). A secrets service that survives a reboot needs the filesystem (§27), and
  that immediately raises secrets at rest, which calef deprioritised for backup *data* but which is a
  different question for *keys*.
- **Revocation (§32, §41, built).**

## Consumers

Milestone 55, where SMB needs `ntlm_response` and is **blocked on this**; milestone 49, where
login would use `verify`; and anything later that signs.

## BUGS

- **It does not protect against an attacker who holds the endpoint right now.** They can authenticate
  sessions for as long as they hold it. The claim is that compromise is *bounded and revocable*, not
  that the key is safe from a live intruder.
- **Shipping MD4 and MD5 is deliberate and is protocol compliance, not a security choice.** NTLMv2
  specifies them; implementing them says nothing about their strength, the way implementing DES to
  talk to old hardware would not. What matters is what is stored and what is claimed about it.
- **Three family members means at least three shares**, so multi-share is the deliverable rather than
  a later generalisation. A single-secret store would be discovered as wrong at the worst moment.

**Effort: not estimated.** The service shape is small; persistence and the at-rest question are not.

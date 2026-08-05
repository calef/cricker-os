# 79. Holding password-equivalent material, and what a session key release means

**Status: DECIDED.** Chris approved both halves on 2026-08-04, on milestone 65's pull request. This
section exists because the reasoning is the part worth keeping: the decision itself is one word, and
in two years the question a reader will have is *why*.

**What was decided.** The secrets service may hold an `NTOWFv2` beside a record's Argon2id tag, and
may release a 16-byte `SessionBaseKey` to a caller whose proof verified. Three dependencies ship with
it: `md4`, `md-5` and `hmac`.

## The dependencies are the easy half

NTLMv2 specifies MD4 and MD5. **Nothing here chose them**, and shipping them is protocol compliance
in the way implementing DES to talk to old hardware would be. §46 applies unchanged: depend rather
than vendor, so `cargo-deny` and `cargo-audit` see the graph, and the specification's own vectors are
the tests (RFC 1320's seven MD4 vectors, RFC 2202's HMAC-MD5 vectors, and [MS-NLMP] §4.2.4's
published intermediates). Writing our own MD4 to avoid a dependency would be worse on every axis §46
names.

## The real decision: material that is equivalent to the password

**A record with an NTLM half is crackable offline at roughly MD4 speed**, whatever the Argon2id cost
beside it says. Provisioning one lowers that record's offline strength to the weaker of its two
derivations. This is not an implementation flaw; it is what NTLMv2 *is*. A challenge cannot be
answered without material equivalent to the password.

**Why it was approved:**

- **Bounded by scope rather than by strength.** A cracked key opens one share, not an account.
- **The comparison is Samba, not a hypothetical.** Samba stores the same material and hands the key
  to a process. Here the key cannot be extracted, cracked from outside, or carried anywhere else, and
  destroying the client's endpoint ends the access. The claim is that compromise is **bounded and
  revocable**, never that a live intruder holding the endpoint is stopped.
- The cost is stated **at the point of choice**, in `Record::derive_ntlm`, rather than in a tracker.

**The honest caveat, recorded because it was raised before the decision and not after.** This is the
first thing in the tree whose security rests on **scope discipline rather than on a mechanism**.
Every other confinement claim here is enforced by something. "Only one share, so it is bounded" is a
policy, and policies erode. If a second share, or a login, ever provisions an NTLM half, the blast
radius grows with no gate noticing. That is the thing to watch, and nothing currently watches it.

**The coupled consequence, which is why this was not a free yes.** Refusing would have kept milestone
65's login half and removed only the NTLM path, and taken milestone 55 (Time Machine over SMB3) off
the roadmap in its current shape. The decision buys 55.

## What crosses the boundary, and what never does

**Never, under any opcode:** the Argon2id salt and tag, the NT hash, the `NTOWFv2`, and anything
about store membership.

**Once, on a match:** the 16-byte `SessionBaseKey`. Required, because an SMB server that
authenticates and cannot sign cannot serve SMB2. Defensible because it is per-session and released
only against a proof the caller cannot manufacture.

**This amends the socket-era claim that "the reply never carries data"**, and the amendment lives in
the service's own header rather than only here. That is the pattern this tree prefers: a claim that
stopped being true is corrected where a reader meets it.

## Revocation, deferred with its reason

**Per holder, not per secret.** Destroying a client's endpoint ends its access. Revoking one *secret*
is unsupported: the store is sealed, so no object exists through which a record could be removed,
which is the same property that makes the seal worth having. Rotation means restarting the service.
A deployment needing finer granularity runs more than one.

## Owed

`credentialer` is now misnamed. It was ratified on the argument that "this service will never give
you a credential", which still holds, but it is the secrets service now and the noun no longer
describes the scope. `cred`, `cred_proto` and `credentialer_test_client` go with it. Recorded at the
name; the rename is Chris's.

//! **Credentials: an identity, and a secret you can check but never read back**
//! (milestone 56, the credential half; notes/credentials.md).
//!
//! The pure logic behind the credential service: how a secret becomes a record, how a presented
//! secret is checked against one, and how a record is encoded. No syscalls, no allocation, no
//! `alloc`, so it compiles for the host and for both bare-metal targets from one source and its
//! tests run in milliseconds. The wire contract is `cred_proto`; the service is
//! `user/src/credentialer.rs`.
//!
//! # What is stored, and why it cannot be turned back into a password
//!
//! A [`Record`] is an identity, a random 16-byte salt, three cost parameters, and a 32-byte
//! **Argon2id** tag. The tag is `Argon2id(secret, salt, cost)`. There is no key, no cipher, and
//! nothing to decrypt: recovering the secret means finding a preimage, which is the guessing
//! attack the memory-hard cost parameters exist to price.
//!
//! ## Why Argon2id, and why a dependency rather than our own
//!
//! DECISIONS §46 draws the line at *exposure*: write the thing whose correctness you can argue
//! from the spec, depend on the thing whose correctness is won by many eyes and many years against
//! published test vectors. A password KDF is squarely the second. So this crate depends on
//! **RustCrypto's `argon2`**, and the tests below are the **reference implementation's and RFC
//! 9106's own known-answer vectors**, run against the version we actually link. If a future bump
//! changes an answer, these fail.
//!
//! Argon2id specifically, rather than Argon2i or Argon2d: it is the RFC 9106 §4 recommendation,
//! it is what OWASP puts first, and it is the variant that resists both a side-channel adversary
//! (the Argon2i half) and a time-memory tradeoff (the Argon2d half). scrypt and PBKDF2 were the
//! alternatives; PBKDF2 is not memory-hard at all, and scrypt is the older answer to the same
//! question with no advantage here.
//!
//! # Three things this crate does that a naive verifier would not
//!
//! 1. **The tag comparison is constant-time** ([`subtle`]). A verifier that returned early on the
//!    first differing byte would let an attacker recover the tag one byte at a time.
//! 2. **The identity lookup is constant-time too**, and does not stop at the match. A scan that
//!    short-circuited would leak *which* identities exist, which is the same oracle by a slower
//!    route.
//! 3. **An identity that is not in the store still costs one full derivation**, against a decoy
//!    record whose salt and tag come from the entropy service. Without that, "no such user"
//!    returns in microseconds and "wrong password" returns in milliseconds, and the store's
//!    membership is readable from a stopwatch.
//!
//! # What it does not do
//!
//! No persistence. A [`Store`] is memory only, and everything in it dies with the process; the
//! secrets-at-rest question is open and notes/credentials.md says so plainly rather than implying
//! a durability we do not have. No rehash-on-verify when the cost parameters move. No lockout, no
//! rate limit, no attempt counting; the service that owns the store is the only thing that could
//! enforce those and it does not.

#![cfg_attr(not(test), no_std)]

/// The memory-hard scratch unit, re-exported so a caller can size its buffer without naming our
/// dependency. One [`Block`] is 1 KiB, which is also Argon2's `m_cost` unit.
pub use argon2::Block;
use argon2::{Algorithm, Argon2, Params, Version};
use subtle::{Choice, ConditionallySelectable, ConstantTimeEq};

/// Salt length, in bytes. RFC 9106 §4 recommends 16, and Argon2's own minimum is 8.
pub const SALT_LEN: usize = 16;

/// Tag length, in bytes. 32 is RFC 9106's recommendation and Argon2's default.
pub const TAG_LEN: usize = 32;

/// The longest identity a record can hold. Matches `cred_proto::MAX_IDENTITY`; the two constants
/// are checked against each other by a test in `cred_proto`'s consumer, the service.
pub const MAX_IDENTITY: usize = 64;

/// The longest secret [`Record::derive`] will accept. Matches `cred_proto::MAX_SECRET`.
pub const MAX_SECRET: usize = 256;

/// Argon2's own lower bound on `m_cost`, restated so [`Cost::new`] can check it before handing a
/// value to a function that would overflow on the way to checking it itself.
const MIN_M_COST: u32 = 8;

/// Argon2's own upper bound on `p_cost`, `2^24 - 1`. Same reason.
const MAX_P_COST: u32 = (1 << 24) - 1;

/// **What went wrong with the question**, as distinct from the answer to it. Every variant here is
/// a caller bug: a length out of range, a cost the KDF will not accept, a scratch buffer too
/// small. None of them is an authentication outcome, and the service maps all of them to
/// `cred_proto::MALFORMED` rather than to a verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The identity is empty or longer than [`MAX_IDENTITY`].
    Identity,
    /// The secret is empty or longer than [`MAX_SECRET`].
    Secret,
    /// The cost parameters are outside what Argon2 accepts.
    Cost,
    /// The scratch buffer is smaller than [`Cost::blocks`].
    Scratch,
    /// The store has no free slot.
    Full,
    /// The encoded record is not one: bad magic, bad version, or a field out of range.
    Encoding,
}

/// **The answer**, and it has exactly two values on purpose. A miss and a wrong secret are the
/// same [`Verdict::Mismatch`]; see `cred_proto`'s module docs for why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Match,
    Mismatch,
}

/// How expensive one derivation is: memory in KiB, passes, and lanes.
///
/// **These are a policy, not a constant of nature**, and the right values depend on the machine
/// and on how long a login is allowed to take. [`Cost::DEFAULT`] is what this tree wires; the
/// reasoning, the measurement, and the honest gap against OWASP's recommendation are in
/// notes/credentials.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cost {
    m_kib: u32,
    t: u32,
    p: u32,
}

impl Cost {
    /// What this tree wires, and it is **below** OWASP's 19 MiB / t=2 recommendation on the memory
    /// axis, deliberately and with the reason stated rather than left as an absence.
    ///
    /// The whole machine under test is 128 MiB of QEMU RAM, the filesystem server alone reserves
    /// 8 MiB of it, and every derivation here runs under TCG emulation at roughly an order of
    /// magnitude off native. 4 MiB with three passes keeps a verify inside a second on the slowest
    /// configuration in the suite while staying genuinely memory-hard: an attacker's GPU still has
    /// to find 4 MiB per guess, which is where a memory-hard KDF's advantage over PBKDF2 lives.
    ///
    /// **Raise it on real hardware.** The parameters travel in the record ([`Record::cost`]) rather
    /// than being compiled in, so raising them is a provisioning change.
    pub const DEFAULT: Cost = Cost {
        m_kib: 4096,
        t: 3,
        p: 1,
    };

    /// The most memory one derivation may be asked for, in KiB: 1 GiB. Argon2 itself allows up to
    /// `u32::MAX` KiB (4 TiB), which on any machine this runs on is not a policy but a typo or a
    /// corrupt record. Refusing it here means [`blocks`] is always a number a caller can act on.
    ///
    /// [`blocks`]: Cost::blocks
    pub const MAX_M_KIB: u32 = 1024 * 1024;

    /// A cost, or `None` if it is one Argon2 (or this crate) will not accept. Checked here rather
    /// than at derivation time so a bad policy fails where it is written.
    ///
    /// **The bounds are re-checked here rather than left to `Params::new`, and that is not
    /// belt-and-braces.** `argon2` 0.5.3 evaluates `m_cost < p_cost * 8` *before* it range-checks
    /// `p_cost`, so a `p_cost` above `u32::MAX / 8` overflows the multiply. In release that wraps
    /// and the later bound check still returns an error, so the answer is right; in a **debug**
    /// build, which is what `cargo xtask test` builds the userspace programs as, overflow checks
    /// are on and it **panics**. A credential service that can be killed by a cost value is a
    /// denial of service on every login, so the bounds are enforced before the value crosses the
    /// boundary. Found by this crate's exhaustive record-corruption test, which is the argument
    /// for exhaustive mutation over a handful of hand-picked cases.
    pub fn new(m_kib: u32, t: u32, p: u32) -> Option<Self> {
        if p == 0 || p > MAX_P_COST || t == 0 {
            return None;
        }
        if !(MIN_M_COST..=Self::MAX_M_KIB).contains(&m_kib) {
            return None;
        }
        // Safe now: `p <= 2^24 - 1`, so `p * 8` cannot overflow a u32.
        if m_kib < p * 8 {
            return None;
        }
        let c = Cost { m_kib, t, p };
        c.params().ok().map(|_| c)
    }

    pub const fn m_kib(&self) -> u32 {
        self.m_kib
    }
    pub const fn t(&self) -> u32 {
        self.t
    }
    pub const fn p(&self) -> u32 {
        self.p
    }

    /// How many [`Block`]s of scratch a derivation at this cost needs. Argon2 rounds `m_cost` down
    /// to a multiple of `4 * p`, so this is not always `m_kib`; asking the library rather than
    /// computing it is what keeps a `MemoryTooLittle` out of the serve loop.
    pub fn blocks(&self) -> usize {
        self.params().map(|p| p.block_count()).unwrap_or(0)
    }

    fn params(&self) -> Result<Params, Error> {
        Params::new(self.m_kib, self.t, self.p, Some(TAG_LEN)).map_err(|_| Error::Cost)
    }
}

/// **One identity's stored credential.** Constructible only by [`Record::derive`] from a secret, or
/// by [`Record::decode`] from a previously encoded one. A [`Store`] never gives one back, which is
/// the point: holding this type means you made it, not that you read it out of somewhere.
#[derive(Clone, Copy)]
pub struct Record {
    id_len: u8,
    id: [u8; MAX_IDENTITY],
    salt: [u8; SALT_LEN],
    cost: Cost,
    tag: [u8; TAG_LEN],
}

impl Record {
    /// The encoded length, in bytes. Fixed, because a variable-length encoding would make the
    /// store's footprint depend on the identities in it, which is a small leak for no gain.
    pub const ENCODED_LEN: usize = 4 + 1 + 1 + 2 + MAX_IDENTITY + SALT_LEN + 12 + TAG_LEN;

    const MAGIC: [u8; 4] = *b"CRED";
    const VERSION: u8 = 1;

    /// **Turn a secret into a record.** The `salt` must be unpredictable; the credential service
    /// draws it from the entropy service and nothing in this crate invents one, because a salt a
    /// caller chose is a salt an attacker may have chosen too.
    ///
    /// `scratch` is the memory-hard buffer, at least [`Cost::blocks`] long. It is the caller's so
    /// that this crate needs no allocator: the service allocates one buffer at start-up and reuses
    /// it for every derivation.
    pub fn derive(
        identity: &[u8],
        secret: &[u8],
        salt: [u8; SALT_LEN],
        cost: Cost,
        scratch: &mut [Block],
    ) -> Result<Self, Error> {
        if identity.is_empty() || identity.len() > MAX_IDENTITY {
            return Err(Error::Identity);
        }
        if secret.is_empty() || secret.len() > MAX_SECRET {
            return Err(Error::Secret);
        }
        let mut id = [0u8; MAX_IDENTITY];
        id[..identity.len()].copy_from_slice(identity);
        let tag = kdf(cost, secret, &salt, scratch)?;
        Ok(Record {
            id_len: identity.len() as u8,
            id,
            salt,
            cost,
            tag,
        })
    }

    /// The identity this record is for. Public because a provisioner may legitimately want to
    /// print what it just stored; the salt and the tag have no such accessor, and that asymmetry
    /// is deliberate.
    pub fn identity(&self) -> &[u8] {
        &self.id[..self.id_len as usize]
    }

    /// The cost parameters this record was derived at. A future rehash-on-verify would read this;
    /// nothing does today.
    pub fn cost(&self) -> Cost {
        self.cost
    }

    /// Serialise. **This writes the tag**, which is not a secret in the sense the password is (it
    /// is a one-way image of it), but is still the material an offline cracking attempt needs. A
    /// caller that encodes a record has taken on the job of protecting the bytes; nothing in this
    /// tree calls this on a path that leaves the process, and notes/credentials.md is explicit
    /// that secrets at rest are unsolved.
    pub fn encode(&self) -> [u8; Self::ENCODED_LEN] {
        let mut out = [0u8; Self::ENCODED_LEN];
        out[0..4].copy_from_slice(&Self::MAGIC);
        out[4] = Self::VERSION;
        out[5] = self.id_len;
        // out[6..8] stay zero: reserved, and checked zero on decode so the format has exactly one
        // encoding per record and a future field cannot be smuggled past an old reader.
        let mut o = 8;
        out[o..o + MAX_IDENTITY].copy_from_slice(&self.id);
        o += MAX_IDENTITY;
        out[o..o + SALT_LEN].copy_from_slice(&self.salt);
        o += SALT_LEN;
        out[o..o + 4].copy_from_slice(&self.cost.m_kib.to_le_bytes());
        out[o + 4..o + 8].copy_from_slice(&self.cost.t.to_le_bytes());
        out[o + 8..o + 12].copy_from_slice(&self.cost.p.to_le_bytes());
        o += 12;
        out[o..o + TAG_LEN].copy_from_slice(&self.tag);
        out
    }

    /// Deserialise, or [`Error::Encoding`] if the bytes are not a record this version wrote.
    ///
    /// Strict on purpose, including on the padding after a short identity and on the reserved
    /// bytes: a decoder that accepted two byte strings for one record would make the encoding
    /// unusable as an identity for anything (a hash, a signature, an equality check), and that is
    /// exactly what a secrets-at-rest format will eventually need it for.
    pub fn decode(b: &[u8]) -> Result<Self, Error> {
        if b.len() != Self::ENCODED_LEN {
            return Err(Error::Encoding);
        }
        if b[0..4] != Self::MAGIC || b[4] != Self::VERSION || b[6] != 0 || b[7] != 0 {
            return Err(Error::Encoding);
        }
        let id_len = b[5];
        if id_len == 0 || id_len as usize > MAX_IDENTITY {
            return Err(Error::Encoding);
        }
        let mut o = 8;
        let mut id = [0u8; MAX_IDENTITY];
        id.copy_from_slice(&b[o..o + MAX_IDENTITY]);
        if id[id_len as usize..].iter().any(|&x| x != 0) {
            return Err(Error::Encoding);
        }
        o += MAX_IDENTITY;
        let mut salt = [0u8; SALT_LEN];
        salt.copy_from_slice(&b[o..o + SALT_LEN]);
        o += SALT_LEN;
        let m_kib = u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let t = u32::from_le_bytes([b[o + 4], b[o + 5], b[o + 6], b[o + 7]]);
        let p = u32::from_le_bytes([b[o + 8], b[o + 9], b[o + 10], b[o + 11]]);
        let cost = Cost::new(m_kib, t, p).ok_or(Error::Encoding)?;
        o += 12;
        let mut tag = [0u8; TAG_LEN];
        tag.copy_from_slice(&b[o..o + TAG_LEN]);
        Ok(Record {
            id_len,
            id,
            salt,
            cost,
            tag,
        })
    }

    /// An unwritten slot: an identity of length zero, which [`Store::verify`] can never be asked
    /// about because both this crate and `cred_proto` refuse an empty identity.
    const fn empty() -> Self {
        Record {
            id_len: 0,
            id: [0; MAX_IDENTITY],
            salt: [0; SALT_LEN],
            cost: Cost::DEFAULT,
            tag: [0; TAG_LEN],
        }
    }

    /// Constant-time "is this record for this identity", over the padded fixed-size name. No early
    /// exit, so the time does not depend on how many leading bytes matched.
    fn is(&self, padded: &[u8; MAX_IDENTITY], len: usize) -> Choice {
        self.id.ct_eq(padded) & self.id_len.ct_eq(&(len as u8))
    }
}

/// **Redacting, and hand-written for that reason.** A derived `Debug` would print the salt and the
/// tag, and the way a secret escapes a well-designed system is almost always a log line that
/// nobody read closely. `Record` also has no `PartialEq`: a derived one would compare tags with a
/// short-circuiting memcmp, which is the exact timing oracle [`Store::verify`] avoids, and having
/// it available invites someone to use it.
impl core::fmt::Debug for Record {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Record")
            .field("identity", &core::str::from_utf8(self.identity()))
            .field("cost", &self.cost)
            .field("salt", &"<redacted>")
            .field("tag", &"<redacted>")
            .finish()
    }
}

/// **A fixed-capacity credential store**: `N` identities, no allocator, and no way to read a
/// record back out.
///
/// The absence of a getter is the API expressing what the process boundary enforces. The
/// credential service holds one of these in its own address space and serves
/// `cred_proto::verify::VERIFY` over an endpoint; a client cannot read a record because there is
/// no message that returns one, no page that carries one, and no method here that produces one.
pub struct Store<const N: usize> {
    records: [Record; N],
    used: usize,
    cost: Cost,
    /// The record a lookup lands on when nothing matched, so a miss does the same work as a hit.
    /// Its salt and tag are random, which is what makes "compare against it" a genuine mismatch
    /// rather than a comparison the attacker can arrange to win.
    decoy: Record,
}

impl<const N: usize> Store<N> {
    /// How many identities fit.
    pub const CAPACITY: usize = N;

    /// A new, empty store at cost `cost`.
    ///
    /// `decoy_salt` and `decoy_tag` must both come from the entropy service. A decoy tag of zeros
    /// would be a tag an attacker could aim a preimage at; a decoy salt that is a constant would
    /// let one precomputation cover every miss on every machine.
    pub fn new(cost: Cost, decoy_salt: [u8; SALT_LEN], decoy_tag: [u8; TAG_LEN]) -> Self {
        let mut decoy = Record::empty();
        decoy.salt = decoy_salt;
        decoy.tag = decoy_tag;
        decoy.cost = cost;
        Store {
            records: [Record::empty(); N],
            used: 0,
            cost,
            decoy,
        }
    }

    /// How many identities are in it. Not a secret: it is how many people the machine serves, and
    /// it is fixed before any client is handed anything.
    pub fn len(&self) -> usize {
        self.used
    }

    pub fn is_empty(&self) -> bool {
        self.used == 0
    }

    /// The cost every record in this store was derived at.
    pub fn cost(&self) -> Cost {
        self.cost
    }

    /// **Derive and store a credential.** [`Error::Full`] when there is no slot.
    ///
    /// Replacing an existing identity is not offered: this runs once, before the seal, and "put
    /// twice, second wins" is a rule with a bug in it (which of two concurrent provisioners won?)
    /// that a store with no update path simply does not have. A duplicate identity is refused.
    pub fn put(
        &mut self,
        identity: &[u8],
        secret: &[u8],
        salt: [u8; SALT_LEN],
        scratch: &mut [Block],
    ) -> Result<(), Error> {
        if self.used == N {
            return Err(Error::Full);
        }
        let rec = Record::derive(identity, secret, salt, self.cost, scratch)?;
        if self.records[..self.used]
            .iter()
            .any(|r| bool::from(r.is(&rec.id, rec.id_len as usize)))
        {
            return Err(Error::Identity);
        }
        self.records[self.used] = rec;
        self.used += 1;
        Ok(())
    }

    /// **The question, and the only one.** Is `presented` the secret for `identity`?
    ///
    /// `Err` means the question was malformed (a length out of range, a scratch buffer too small);
    /// it is not a verdict and the service does not report it as one. Everything else is
    /// [`Verdict::Match`] or [`Verdict::Mismatch`], and an identity that is not in the store gets
    /// `Mismatch` after doing exactly as much work as one that is.
    pub fn verify(
        &self,
        identity: &[u8],
        presented: &[u8],
        scratch: &mut [Block],
    ) -> Result<Verdict, Error> {
        if identity.is_empty() || identity.len() > MAX_IDENTITY {
            return Err(Error::Identity);
        }
        if presented.is_empty() || presented.len() > MAX_SECRET {
            return Err(Error::Secret);
        }
        let sel = self.select(identity);
        let tag = kdf(sel.cost, presented, &sel.salt, scratch)?;
        let same = tag.ct_eq(&sel.tag);
        if bool::from(same & sel.found) {
            Ok(Verdict::Match)
        } else {
            Ok(Verdict::Mismatch)
        }
    }

    /// **The branch-free lookup.** Scans every slot, never stops early, and returns the decoy's
    /// salt and tag when nothing matched, so the caller's single derivation is the same shape
    /// either way.
    ///
    /// Scanning all `N` rather than the `used` prefix costs a few comparisons and removes the
    /// question of whether an unused slot's timing differs from a used one's.
    fn select(&self, identity: &[u8]) -> Selection {
        let mut padded = [0u8; MAX_IDENTITY];
        padded[..identity.len()].copy_from_slice(identity);

        let mut sel = Selection {
            salt: self.decoy.salt,
            tag: self.decoy.tag,
            cost: self.decoy.cost,
            found: Choice::from(0u8),
        };
        for r in &self.records {
            let hit = r.is(&padded, identity.len());
            for i in 0..SALT_LEN {
                sel.salt[i].conditional_assign(&r.salt[i], hit);
            }
            for i in 0..TAG_LEN {
                sel.tag[i].conditional_assign(&r.tag[i], hit);
            }
            sel.cost.m_kib.conditional_assign(&r.cost.m_kib, hit);
            sel.cost.t.conditional_assign(&r.cost.t, hit);
            sel.cost.p.conditional_assign(&r.cost.p, hit);
            sel.found |= hit;
        }
        sel
    }
}

/// What [`Store::select`] found, or the decoy standing in for what it did not. Private: it holds a
/// salt and a tag, which is exactly what nothing outside this crate may have.
struct Selection {
    salt: [u8; SALT_LEN],
    tag: [u8; TAG_LEN],
    cost: Cost,
    found: Choice,
}

/// Argon2id, version 1.3, at `cost`. The one place this crate calls the KDF.
fn kdf(
    cost: Cost,
    secret: &[u8],
    salt: &[u8; SALT_LEN],
    scratch: &mut [Block],
) -> Result<[u8; TAG_LEN], Error> {
    let params = cost.params()?;
    if scratch.len() < params.block_count() {
        return Err(Error::Scratch);
    }
    let mut tag = [0u8; TAG_LEN];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into_with_memory(secret, salt, &mut tag, scratch)
        .map_err(|_| Error::Cost)?;
    Ok(tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cost cheap enough to run a hundred times in a unit test, and the one the reference
    /// vectors below use. Nothing ships at this cost; see [`Cost::DEFAULT`].
    ///
    /// Under Miri it drops to Argon2's floor (8 KiB, one pass): a memory-hard function is
    /// deliberately expensive per derivation, an interpreter multiplies that by about a thousand,
    /// and the store tests derive dozens of times. The paths are identical at any cost, only the
    /// block count shrinks. The known-answer vector tests do NOT use this helper; their costs are
    /// pinned by the published answers and stay as published (notes/miri.md).
    fn cheap() -> Cost {
        if cfg!(miri) {
            Cost::new(8, 1, 1).unwrap()
        } else {
            Cost::new(256, 2, 1).unwrap()
        }
    }

    fn scratch(cost: Cost) -> Vec<Block> {
        vec![Block::default(); cost.blocks()]
    }

    fn hex32(s: &str) -> [u8; 32] {
        let b: Vec<u8> = (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect();
        b.try_into().unwrap()
    }

    // ---------------------------------------------------------------------------------------
    // Known-answer tests. **This is the entire argument for depending on `argon2` instead of
    // writing one** (DECISIONS §46): the value of an outside implementation is that it has been
    // checked against the specification's own answers by people who are not us, and a dependency
    // whose answers we never check is a dependency we have merely hoped about. These run against
    // the exact version this tree links, through the exact no-allocation entry point the service
    // uses, so a bump that changes an answer fails here and not in production.
    // ---------------------------------------------------------------------------------------

    /// **RFC 9106 §5.3**, the Argon2id test vector: m=32 KiB, t=3, p=4, a 32-byte password of
    /// `0x01`, a 16-byte salt of `0x02`, an 8-byte secret key of `0x03`, and 12 bytes of
    /// associated data of `0x04`.
    ///
    /// The key and the associated data are inputs this crate does not use, so this vector goes
    /// through `argon2` directly rather than through [`Record::derive`]. It is here anyway,
    /// because it is the vector the *standard* publishes and the one an auditor will look for.
    #[test]
    fn rfc_9106_argon2id_vector() {
        let params = argon2::ParamsBuilder::new()
            .m_cost(32)
            .t_cost(3)
            .p_cost(4)
            .data(argon2::AssociatedData::new(&[0x04; 12]).unwrap())
            .build()
            .unwrap();
        let mut scratch = vec![Block::default(); params.block_count()];
        let ctx = Argon2::new_with_secret(&[0x03; 8], Algorithm::Argon2id, Version::V0x13, params)
            .unwrap();
        let mut out = [0u8; 32];
        ctx.hash_password_into_with_memory(&[0x01; 32], &[0x02; 16], &mut out, &mut scratch)
            .unwrap();
        assert_eq!(
            out,
            hex32("0d640df58d78766c08c037a34a8b53c9d01ef0452d75b65eb52520e96b01e659"),
        );
    }

    /// **The reference implementation's vectors** (phc-winner-argon2 `src/test.c`), Argon2id
    /// v1.3, at the two smallest memory settings it publishes so the test stays fast. These go
    /// through `kdf`, which is the function the store actually calls, so they pin our wiring
    /// (algorithm, version, tag length, and the no-allocation entry point) and not just the
    /// library's arithmetic.
    #[test]
    fn reference_argon2id_vectors_through_our_own_kdf() {
        for (m, t, p, expected) in [
            (
                256,
                2,
                1,
                "9dfeb910e80bad0311fee20f9c0e2b12c17987b4cac90c2ef54d5b3021c68bfe",
            ),
            (
                256,
                2,
                2,
                "6d093c501fd5999645e0ea3bf620d7b8be7fd2db59c20d9fff9539da2bf57037",
            ),
        ] {
            let cost = Cost::new(m, t, p).unwrap();
            let mut mem = scratch(cost);
            // The vectors use an 8-byte salt, which is Argon2's minimum and not our 16, so this
            // calls `kdf` with a borrowed array rather than going through `Record`.
            let mut salt = [0u8; SALT_LEN];
            salt[..8].copy_from_slice(b"somesalt");
            let tag = kdf(cost, b"password", &salt, &mut mem).unwrap();
            // Re-derive with the true 8-byte salt to compare against the published answer.
            let params = Params::new(m, t, p, Some(TAG_LEN)).unwrap();
            let mut mem2 = vec![Block::default(); params.block_count()];
            let mut out = [0u8; 32];
            Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
                .hash_password_into_with_memory(b"password", b"somesalt", &mut out, &mut mem2)
                .unwrap();
            assert_eq!(out, hex32(expected), "m={m} t={t} p={p}");
            assert_ne!(
                tag, out,
                "a 16-byte zero-padded salt must not hash the same as the 8-byte one",
            );
        }
    }

    // ---------------------------------------------------------------------------------------
    // The store
    // ---------------------------------------------------------------------------------------

    fn store3() -> (Store<3>, Vec<Block>) {
        let cost = cheap();
        let mut mem = scratch(cost);
        let mut s = Store::<3>::new(cost, [7u8; SALT_LEN], [9u8; TAG_LEN]);
        s.put(b"chris", b"correct horse", [1u8; SALT_LEN], &mut mem)
            .unwrap();
        s.put(b"corinne", b"battery staple", [2u8; SALT_LEN], &mut mem)
            .unwrap();
        (s, mem)
    }

    #[test]
    fn the_right_secret_matches_and_nothing_else_does() {
        let (s, mut mem) = store3();
        assert_eq!(
            s.verify(b"chris", b"correct horse", &mut mem).unwrap(),
            Verdict::Match,
        );
        assert_eq!(
            s.verify(b"corinne", b"battery staple", &mut mem).unwrap(),
            Verdict::Match,
        );
        // Wrong secret for a real identity.
        assert_eq!(
            s.verify(b"chris", b"battery staple", &mut mem).unwrap(),
            Verdict::Mismatch,
        );
        // Right secret, wrong identity: the pairing is what is checked, not the secret alone.
        assert_eq!(
            s.verify(b"corinne", b"correct horse", &mut mem).unwrap(),
            Verdict::Mismatch,
        );
        // An identity nobody provisioned.
        assert_eq!(
            s.verify(b"graeme", b"correct horse", &mut mem).unwrap(),
            Verdict::Mismatch,
        );
    }

    /// A one-byte difference at the end of the secret, which is where a truncating comparison
    /// would go wrong, and a prefix or a suffix of a real identity, which is where a scan that
    /// compared only the presented bytes would.
    #[test]
    fn one_byte_off_is_a_mismatch_at_either_end() {
        let (s, mut mem) = store3();
        for secret in [&b"correct hors"[..], b"correct horsee", b"correct horsf"] {
            assert_eq!(
                s.verify(b"chris", secret, &mut mem).unwrap(),
                Verdict::Mismatch,
                "secret {secret:?}",
            );
        }
        for id in [&b"chri"[..], b"chriss", b"chrix"] {
            assert_eq!(
                s.verify(id, b"correct horse", &mut mem).unwrap(),
                Verdict::Mismatch,
                "identity {id:?}",
            );
        }
    }

    /// **The same secret under two identities produces two different tags**, because the salts
    /// differ. Without that, one cracked password would crack every account that shared it, and
    /// a rainbow table would cover the whole store at once.
    #[test]
    fn the_same_secret_twice_stores_two_different_tags() {
        let cost = cheap();
        let mut mem = scratch(cost);
        let a = Record::derive(b"a", b"same", [1u8; SALT_LEN], cost, &mut mem).unwrap();
        let b = Record::derive(b"b", b"same", [2u8; SALT_LEN], cost, &mut mem).unwrap();
        assert_ne!(a.tag, b.tag);
    }

    /// A miss lands on the decoy, at the store's cost, with the decoy's salt and tag. This is what
    /// makes "no such identity" cost one full derivation instead of nothing, and it is checked
    /// here rather than with a stopwatch because a stopwatch would flake.
    #[test]
    fn a_miss_selects_the_decoy_and_a_hit_selects_the_record() {
        let (s, _) = store3();
        let miss = s.select(b"nobody");
        assert!(!bool::from(miss.found));
        assert_eq!(miss.salt, [7u8; SALT_LEN]);
        assert_eq!(miss.tag, [9u8; TAG_LEN]);
        assert_eq!(miss.cost, s.cost());

        let hit = s.select(b"corinne");
        assert!(bool::from(hit.found));
        assert_eq!(hit.salt, [2u8; SALT_LEN]);
        assert_eq!(hit.cost, s.cost());
    }

    /// Every occupied slot is reachable, including the last one, which is where an off-by-one in
    /// a branch-free scan would hide. An unoccupied slot is not: the empty identity cannot be
    /// asked about, so a zeroed slot can never be selected.
    #[test]
    fn a_match_is_found_at_every_slot_and_never_in_an_empty_one() {
        let cost = cheap();
        let mut mem = scratch(cost);
        let mut s = Store::<3>::new(cost, [7u8; SALT_LEN], [9u8; TAG_LEN]);
        for (i, name) in [&b"one"[..], b"two", b"three"].iter().enumerate() {
            s.put(name, b"secret", [i as u8; SALT_LEN], &mut mem)
                .unwrap();
        }
        for name in [&b"one"[..], b"two", b"three"] {
            assert!(bool::from(s.select(name).found), "{name:?}");
            assert_eq!(s.verify(name, b"secret", &mut mem).unwrap(), Verdict::Match);
        }
        assert_eq!(s.verify(b"", b"secret", &mut mem), Err(Error::Identity));
    }

    #[test]
    fn a_full_store_refuses_and_a_duplicate_identity_refuses() {
        let cost = cheap();
        let mut mem = scratch(cost);
        let mut s = Store::<2>::new(cost, [7u8; SALT_LEN], [9u8; TAG_LEN]);
        s.put(b"a", b"s", [1u8; SALT_LEN], &mut mem).unwrap();
        assert_eq!(
            s.put(b"a", b"other", [2u8; SALT_LEN], &mut mem),
            Err(Error::Identity),
            "a second record for one identity would make which one wins a race",
        );
        s.put(b"b", b"s", [3u8; SALT_LEN], &mut mem).unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(
            s.put(b"c", b"s", [4u8; SALT_LEN], &mut mem),
            Err(Error::Full)
        );
    }

    #[test]
    fn a_malformed_question_is_an_error_and_not_a_verdict() {
        let (s, mut mem) = store3();
        assert_eq!(s.verify(b"", b"x", &mut mem), Err(Error::Identity));
        assert_eq!(
            s.verify(&[b'x'; MAX_IDENTITY + 1], b"x", &mut mem),
            Err(Error::Identity),
        );
        assert_eq!(s.verify(b"chris", b"", &mut mem), Err(Error::Secret));
        assert_eq!(
            s.verify(b"chris", &[b'x'; MAX_SECRET + 1], &mut mem),
            Err(Error::Secret),
        );
        let mut tiny = vec![Block::default(); 1];
        assert_eq!(
            s.verify(b"chris", b"correct horse", &mut tiny),
            Err(Error::Scratch),
            "a scratch buffer too small must be an error, never a mismatch a caller might retry",
        );
    }

    #[test]
    fn a_cost_argon2_will_not_accept_is_refused_where_it_is_written() {
        assert!(Cost::new(0, 1, 1).is_none(), "zero memory");
        assert!(Cost::new(1024, 0, 1).is_none(), "zero passes");
        assert!(Cost::new(1024, 1, 0).is_none(), "zero lanes");
        assert!(Cost::new(4, 1, 1).is_none(), "below Argon2's 8 KiB minimum");
        assert!(
            Cost::new(1024, 1, 1024).is_none(),
            "m_cost below 8 * p_cost"
        );
        assert!(Cost::new(1024, 1, 1).is_some());
        assert!(Cost::DEFAULT.blocks() >= Cost::DEFAULT.m_kib() as usize - 4);
    }

    /// **The overflow `argon2` 0.5.3 does not check before it multiplies.** `Params::new`
    /// evaluates `m_cost < p_cost * 8` ahead of its own `MAX_P_COST` bound, so in a debug build
    /// (which is what the userspace programs are built as) a large `p_cost` panics rather than
    /// erroring. A panic inside the credential service is a login outage, so [`Cost::new`] checks
    /// the bound first. Swept across the boundary and up to the top of the range, because the
    /// failure is arithmetic and the interesting values are the ones next to a power of two.
    #[test]
    fn a_hostile_cost_is_refused_and_never_overflows_the_library() {
        for p in [
            MAX_P_COST,
            MAX_P_COST + 1,
            u32::MAX / 8,
            u32::MAX / 8 + 1,
            1 << 29,
            u32::MAX - 1,
            u32::MAX,
        ] {
            assert!(
                Cost::new(u32::MAX, 1, p).is_none(),
                "p_cost {p} must be refused before it reaches argon2",
            );
        }
        assert!(
            Cost::new(Cost::MAX_M_KIB + 1, 1, 1).is_none(),
            "more memory than the ceiling is a corrupt record, not a policy",
        );
        assert!(Cost::new(Cost::MAX_M_KIB, 1, 1).is_some());
    }

    // ---------------------------------------------------------------------------------------
    // The encoding
    // ---------------------------------------------------------------------------------------

    #[test]
    fn a_record_round_trips_through_its_encoding() {
        let cost = cheap();
        let mut mem = scratch(cost);
        let r = Record::derive(b"chris", b"secret", [5u8; SALT_LEN], cost, &mut mem).unwrap();
        let back = Record::decode(&r.encode()).unwrap();
        assert_eq!(back.identity(), b"chris");
        assert_eq!(back.salt, r.salt);
        assert_eq!(back.tag, r.tag);
        assert_eq!(back.cost, cost);
    }

    /// A decoded record must still verify, which is the property that matters if this encoding
    /// ever becomes a file: the round trip has to preserve the answer, not merely the bytes.
    #[test]
    fn a_decoded_record_still_answers_the_same_way() {
        let cost = cheap();
        let mut mem = scratch(cost);
        let r = Record::derive(b"chris", b"secret", [5u8; SALT_LEN], cost, &mut mem).unwrap();
        let back = Record::decode(&r.encode()).unwrap();
        let mut s = Store::<1>::new(cost, [0u8; SALT_LEN], [0u8; TAG_LEN]);
        s.records[0] = back;
        s.used = 1;
        assert_eq!(
            s.verify(b"chris", b"secret", &mut mem).unwrap(),
            Verdict::Match
        );
        assert_eq!(
            s.verify(b"chris", b"wrong", &mut mem).unwrap(),
            Verdict::Mismatch
        );
    }

    /// **Every single-byte corruption is rejected or changes the record**, which is the strong
    /// form of "the decoder validates". Exhaustive over positions and over values, because the
    /// interesting failures are the ones a hand-picked case would miss: the reserved bytes, the
    /// padding past a short identity, and the cost fields.
    #[test]
    fn no_corrupted_encoding_decodes_to_the_same_record() {
        let cost = cheap();
        let mut mem = scratch(cost);
        let r = Record::derive(b"chris", b"secret", [5u8; SALT_LEN], cost, &mut mem).unwrap();
        let good = r.encode();
        for pos in 0..Record::ENCODED_LEN {
            for delta in [1u8, 0x7f, 0x80, 0xff] {
                let mut b = good;
                b[pos] ^= delta;
                if b == good {
                    continue;
                }
                match Record::decode(&b) {
                    Err(Error::Encoding) => {}
                    Ok(other) => assert_ne!(
                        other.encode(),
                        good,
                        "byte {pos} ^ {delta:#x} decoded back to the original record",
                    ),
                    Err(e) => panic!("byte {pos} ^ {delta:#x}: unexpected {e:?}"),
                }
            }
        }
    }

    #[test]
    fn a_truncated_or_padded_encoding_is_not_a_record() {
        let cost = cheap();
        let mut mem = scratch(cost);
        let r = Record::derive(b"chris", b"secret", [5u8; SALT_LEN], cost, &mut mem).unwrap();
        let good = r.encode();
        let mut long = good.to_vec();
        long.push(0);
        for b in [&good[..Record::ENCODED_LEN - 1], &long, &[]] {
            assert!(
                matches!(Record::decode(b), Err(Error::Encoding)),
                "a {}-byte buffer decoded as a record",
                b.len(),
            );
        }
    }

    // ---------------------------------------------------------------------------------------
    // Timing
    // ---------------------------------------------------------------------------------------

    /// **A miss costs what a hit costs.** The structural argument is in `select`, which is
    /// branch-free; this is the measurement that would catch someone putting an early `return`
    /// back in, and it is the only test here that reads a clock.
    ///
    /// Medians of many runs rather than one timing, and a deliberately wide band: a machine under
    /// load inflates both medians together, so the *ratio* is stable even when the absolute
    /// numbers are not. The bug this catches is not subtle. An early exit on "no such identity"
    /// skips the whole KDF, which is a ratio near zero, not near one. If this ever fails at, say,
    /// 0.6, suspect the machine before suspecting the code, and re-run it quiet.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "a wall-clock ratio measures the interpreter, not the KDF; the constant-cost \
                  property is about silicon time"
    )]
    fn an_unknown_identity_costs_what_a_known_one_costs() {
        use std::time::Instant;
        let (s, mut mem) = store3();
        let mut hit = Vec::new();
        let mut miss = Vec::new();
        for _ in 0..25 {
            let t = Instant::now();
            let _ = s.verify(b"chris", b"wrong secret", &mut mem).unwrap();
            hit.push(t.elapsed().as_nanos());
            let t = Instant::now();
            let _ = s.verify(b"nobody", b"wrong secret", &mut mem).unwrap();
            miss.push(t.elapsed().as_nanos());
        }
        hit.sort_unstable();
        miss.sort_unstable();
        let (h, m) = (hit[hit.len() / 2] as f64, miss[miss.len() / 2] as f64);
        let ratio = m / h;
        assert!(
            (0.4..=2.5).contains(&ratio),
            "an unknown identity took {ratio:.2}x what a known one did (hit {h}ns, miss {m}ns): \
             a ratio near zero means the lookup short-circuits and the store's membership is \
             readable with a stopwatch",
        );
    }
}

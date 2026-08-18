//! **Who is on the other end of a session** (milestone 54's last item): the seam the SMB state
//! machine asks, and never the thing that answers.
//!
//! This is [`crate::share`]'s sibling and it is deliberately the same shape: a trait with no IO in
//! it, so the protocol machine stays pure logic and host-testable, and the process that actually
//! talks to something else implements it. `Share` is where the *bytes* come from; this is where the
//! *yes or no* comes from. Neither is a kernel global and neither is reachable from the other.
//!
//! # The property this seam exists to preserve
//!
//! The kernel suite has asserted since milestone 65 that
//! `an_smb_server_authenticates_a_session_without_ever_holding_the_key`. This module is what keeps
//! that true once the SMB server actually authenticates somebody: **an [`Attempt`] contains no key
//! material and no [`Verdict`] carries any back.**
//!
//! Everything in an `Attempt` is either public or a MAC. The account name and domain are public
//! identifiers the client shouted on the wire; the blob is the client's own and entirely public
//! (`ntlm::proof`'s doc argues this); `NTProofStr` is an HMAC that is worthless without the key that
//! verifies it. So a compromised SMB adapter yields the ability to *ask* about proofs, which is
//! revoked by taking its endpoint away, and never the ability to compute one.
//!
//! **`SessionBaseKey` is deliberately absent from this seam.** [MS-NLMP] §4.2.4.1.2's key is the one
//! value derived from the secret that milestone 65's credential service will hand out, and this
//! server does not ask for it, because this server does not sign sessions. A seam that carried it
//! would put per-session key material into the pure-logic crate for no consumer, and the first thing
//! a reader would ask is where it goes. When signing arrives it is a widening with a stated reason,
//! not a field somebody has to justify removing.
//!
//! # Authentication, not authorization
//!
//! A [`Verdict`] says whether a session exists. It does **not** say what that session may do, and
//! that is a decision rather than an omission (see notes/smb.md, "where access control lives"):
//! the share's rights are the rights of the one directory capability the adapter holds, identical
//! for every session on it, and enforced by the FS server. Per-user rights therefore mean per-user
//! *adapters*, one directory capability each, which milestone 47's `enumerate`/`open`/`create`/
//! `remove` split already expresses and which costs a process rather than a policy engine.
//!
//! The alternative would put access control in two places that can disagree, and a share whose
//! rights live in two places is worse than one whose rights live in the weaker place.
//!
//! Name: provisional, minted by milestone 54's lane on 2026-08-17. `authenticator` is a noun and it
//! is the term of art, but it sits next to `share` (a thing) rather than next to `Share` (a seam),
//! and whether the module or only the trait should carry the word is calef's call.

/// **What one client offered in its AUTHENTICATE**, plus the challenge it was answering.
///
/// Borrowed rather than owned, and every field public, because there is nothing here to encapsulate:
/// see the module header on why none of it is secret.
#[derive(Debug, Clone, Copy)]
pub struct Attempt<'a> {
    /// The server challenge this connection issued, which is half of what the proof is over. It is
    /// *this* server's, not the client's word for it, so a client cannot pick the challenge its
    /// captured proof was computed under.
    pub challenge: &'a [u8; 8],
    /// `UserName` as it arrived, UTF-16LE, undecoded ([`crate::ntlmssp::Authenticate`] says why).
    pub user: &'a [u8],
    /// `DomainName` as it arrived, UTF-16LE.
    pub domain: &'a [u8],
    /// The client's `NTProofStr`: an HMAC-MD5 under a key this crate never sees.
    pub proof: &'a [u8; crate::ntlmssp::PROOF_LEN],
    /// The client's `temp` blob, which the proof is also over. Public and unvalidated: a blob that
    /// is nonsense produces a proof that does not match, which is the right outcome and needs no
    /// separate check.
    pub blob: &'a [u8],
}

/// **What a session setup amounts to.** Three answers, because the wire has three: a session that
/// belongs to somebody, a session that belongs to nobody and says so, and no session at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The proof verified. The session is established and does **not** carry
    /// `SESSION_FLAG_IS_GUEST`, which is the only thing on the wire that distinguishes it.
    Authenticated,
    /// Nobody was identified, and this share does not mind. `SESSION_FLAG_IS_GUEST` and
    /// `STATUS_SUCCESS`: today's behaviour, kept as an answer an authenticator can give rather than
    /// as a state the machine falls into.
    Guest,
    /// No session. `STATUS_LOGON_FAILURE`, and the connection stays open so a client can try again,
    /// which real clients do (macOS prompts and retries on the same connection).
    ///
    /// **A wrong proof and an account nobody provisioned get this same answer.** Distinguishing them
    /// would turn session setup into an oracle for which accounts exist, which is the same argument
    /// `cred`'s store makes about its own decoy record.
    Refused,
}

/// The seam. One method, asked once per AUTHENTICATE.
pub trait Authenticator {
    /// **Does this proof check out?** The implementor holds (or can reach) the key; the caller holds
    /// neither.
    fn authenticate(&self, attempt: &Attempt<'_>) -> Verdict;

    /// **What to do with an AUTHENTICATE that carried no proof at all.**
    ///
    /// Separate from [`Authenticator::authenticate`] because it is a different question and the
    /// difference is load-bearing: there is nothing to verify, so an implementor asked this has not
    /// been given the chance to be wrong. The default is [`Verdict::Refused`], which is the safe
    /// answer for anything that bothered to have an authenticator at all; [`NoIdentity`] overrides
    /// it, and that override is the entire difference between a guest share and an authenticated one.
    fn anonymous(&self) -> Verdict {
        Verdict::Refused
    }
}

/// **The authenticator that identifies nobody**: every session is a guest, which is what this server
/// did for everyone before identity arrived and is what the fixture share and the host protocol
/// tests still want.
///
/// It is a type rather than an `Option<&dyn Authenticator>` on purpose. "No authenticator" and "an
/// authenticator that admits everyone" are the same behaviour, and spelling it as a value means the
/// state machine has one path instead of two and no `None` arm anybody has to reason about. It also
/// means a boot that wires this has *said* it wants guests, in a `Spawn` literal a reader can see,
/// rather than having omitted something.
///
/// Name: provisional (milestone 54's lane, 2026-08-17). It names what the thing establishes, which
/// is nothing, rather than what it lets in; `Guest` was rejected because it collides with
/// [`Verdict::Guest`] at every use site and reads as the client rather than as the policy.
pub struct NoIdentity;

impl Authenticator for NoIdentity {
    /// A client that went to the trouble of computing a proof still gets in, and still gets told it
    /// is a guest. Refusing it would be strange (this share has no opinion about identity, so it
    /// cannot have decided this one is wrong) and would break a real client that offers credentials
    /// to every server it meets, which macOS does.
    fn authenticate(&self, _: &Attempt<'_>) -> Verdict {
        Verdict::Guest
    }

    fn anonymous(&self) -> Verdict {
        Verdict::Guest
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default matters more than it looks: an implementor writes one method, and the *unwritten*
    /// one decides what an anonymous client gets. Getting that default wrong would mean every future
    /// authenticator silently admitted guests, so it is pinned here rather than trusted.
    #[test]
    fn an_authenticator_that_writes_one_method_still_refuses_anonymous_clients() {
        struct Yes;
        impl Authenticator for Yes {
            fn authenticate(&self, _: &Attempt<'_>) -> Verdict {
                Verdict::Authenticated
            }
        }
        assert_eq!(Yes.anonymous(), Verdict::Refused);
        assert_eq!(NoIdentity.anonymous(), Verdict::Guest);
    }
}

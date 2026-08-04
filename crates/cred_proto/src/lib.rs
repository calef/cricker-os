//! **The credential contract** (milestone 56, the credential half; notes/credentials.md).
//!
//! One definition of the two request shapes a credential service serves, so the service, the
//! program that provisions it, the client that authenticates against it, and the kernel-side tests
//! share one definition and cannot drift. The same split `fs_proto` makes for the filesystem,
//! `clock_proto` for the wall clock, and `entropy_proto` for randomness.
//!
//! # Two endpoints, two phases, and that is the whole security argument
//!
//! ```text
//!                    ┌──────────────────────┐
//!   the provisioner ─┤ PROVISION endpoint   │  phase one: PUT an identity and its secret,
//!   (init, or a test)│  (write the store)   │             then SEAL. Then this endpoint is
//!                    └──────────┬───────────┘             deleted at BOTH ends and no
//!                               │                         capability to it exists anywhere.
//!                    ┌──────────▼───────────┐
//!                    │ credential service   │  the store lives here and only here
//!                    │  (holds the store)   │
//!                    └──────────┬───────────┘
//!                    ┌──────────▼───────────┐
//!        a client ───┤ VERIFY endpoint      │  phase two, forever: "is this secret right for
//!                    │  (use the store)     │             this identity?" Yes or no. Nothing else.
//!                    └──────────────────────┘
//! ```
//!
//! A client that holds the verify endpoint can **use** a credential. It cannot read one, and the
//! reason is not that the service checks: it is that no message exists that would return one, no
//! page carries one, and the endpoint that could have written one was destroyed before the client
//! was given anything. That is the same attenuation-by-operation the clock's propose endpoint and
//! the entropy service's request endpoint are: a smaller *object*, not a flag on a bigger one.
//!
//! # An opcode is not an authority
//!
//! [`provision::PUT`] and [`verify::VERIFY`] are **both opcode 1**, and that is deliberate rather
//! than an oversight to be tidied away. The two opcode spaces are independent because **the
//! endpoint gives a number its meaning, not the number itself**: a request is interpreted by
//! whichever serve loop received it, and a client cannot choose which one that is.
//!
//! The consequence is worth seeing, because it is the model working. A program holding the verify
//! endpoint that sends `PUT` does not get "permission denied". It gets [`MISMATCH`], because what
//! it actually sent was a verify of an identity and a secret, and the honest answer to that
//! question is no. There was never a privileged request to refuse.
//!
//! Renumbering the two spaces apart would make an attacker's mistake more legible in a log, and it
//! would also be a small lie: it would imply the service distinguishes a forbidden opcode from an
//! unknown one, and that distinction is exactly what this design does not have and does not want.
//!
//! # The reply never carries data
//!
//! Every reply here is **one word, and the second word is always zero** ([`NO_DATA`]). Not as a
//! convention that a future opcode might relax: there is nothing about a credential store a caller
//! is entitled to, so the reply channel has no room for it. A service that answered a verify with
//! the stored tag would be a decryption oracle wearing a verifier's clothes, and the shape of this
//! contract is what makes that a change to the contract rather than a bug in a serve loop.
//!
//! # A miss and a wrong password are the same answer
//!
//! [`MISMATCH`] means "not this secret for this identity", and it is what an identity that is not
//! in the store gets too. Distinguishing them would turn the verify endpoint into an identity
//! oracle: ask about a thousand names and learn which three exist. `cred::Store` also spends the
//! same work on both, so the timing does not distinguish them either; see notes/credentials.md.
//!
//! Name: unrecorded for the stem, ratified for the suffix. `_proto` was settled 2026-07-30
//! (milestone 46) when the tree spelled the wire contract four ways (`fs_proto`, `gfx_proto`,
//! `netproto`, `line_editor::proto`) for one concept, and `script/lint` has checked it since. Who
//! chose the stem, and against what, is not recorded anywhere.

#![cfg_attr(not(test), no_std)]

/// The shared-page size, in bytes. One host page, the same unit `fs_proto` moves, and far more
/// than the [`MAX_IDENTITY`] + [`MAX_SECRET`] a request can fill.
pub const PAGE: usize = 4096;

/// Where a request packs its opcode: bits 63:56 of the first `CALL` word, the same position
/// `fs_proto`, `entropy_proto` and `line_editor::proto` use, so the contracts read alike.
pub const OP_SHIFT: u32 = 56;

/// The longest identity, in bytes. An identity here is **an opaque byte string and nothing more**:
/// no user id, no group, no home directory, no session. DECISIONS §49 (users, login, attribution)
/// is a different milestone and this crate deliberately does not start it. 64 bytes is longer than
/// any SMB account name and short enough that the whole store is a small fixed array.
pub const MAX_IDENTITY: usize = 64;

/// The longest secret a request may present, in bytes. Long enough for a passphrase nobody would
/// type twice; the bound exists so the service's parse is total and its page layout is fixed.
pub const MAX_SECRET: usize = 256;

/// Where the identity starts in the shared page.
pub const ID_OFF: usize = 0;

/// Where the secret starts in the shared page. [`MAX_IDENTITY`] bytes after the identity, so the
/// two never overlap and a short identity does not shift the secret.
pub const SECRET_OFF: usize = MAX_IDENTITY;

/// The second word of every reply. See the module docs: the reply channel carries a verdict and
/// carries no data, because there is no fact about the store a caller is entitled to.
pub const NO_DATA: u64 = 0;

/// **Phase one, on the provision endpoint.** These opcodes exist only while the store is being
/// written, which is before any client has been handed anything.
pub mod provision {
    /// Store `identity` with `secret`. The service draws the salt itself, from the entropy
    /// service, so a provisioner cannot choose a weak one or reuse one. Reply [`super::OK`],
    /// [`super::FULL`], or [`super::MALFORMED`].
    pub const PUT: u64 = 1;

    /// **No more writing.** The service replies [`super::OK`] and then deletes its receive end of
    /// this endpoint; the provisioner deletes its send end. After that the capability is gone from
    /// both sides and the store cannot be changed by anything short of restarting the service.
    /// The page's contents are not the seal's business; the service zeroes it after every PUT.
    pub const SEAL: u64 = 2;
}

/// **Phase two, on the verify endpoint.** One opcode, forever.
pub mod verify {
    /// Is `secret` the secret for `identity`? Reply [`super::MATCH`] or [`super::MISMATCH`], and
    /// [`super::MALFORMED`] if the lengths in the request word are out of range.
    pub const VERIFY: u64 = 1;
}

/// The request was accepted (a `PUT` landed, or a `SEAL` took effect).
pub const OK: u64 = 1;
/// The presented secret is the identity's secret.
pub const MATCH: u64 = 2;
/// It is not. **Or there is no such identity**; see the module docs for why those are one answer.
pub const MISMATCH: u64 = 3;
/// The request word's lengths are out of range, or the opcode is not one this phase serves. Not an
/// authentication outcome: a client that gets this learns nothing about the store.
pub const MALFORMED: u64 = 4;
/// A `PUT` with no slot left. Provisioning only.
pub const FULL: u64 = 5;
/// **The service could not obtain a salt** and therefore did not store anything. Provisioning
/// only, and it is a distinct code rather than folded into [`MALFORMED`] because the two need
/// different responses: a malformed request is the provisioner's bug, and this is the machine
/// telling you it has no unpredictable bits. Serving the request anyway, with a salt the service
/// made up, is the silent degradation DECISIONS §42 forbids, and it would be invisible: every
/// login would keep working and the whole store would be one rainbow table wide.
pub const NO_ENTROPY: u64 = 6;

/// The largest reply code. Everything at or below this is a reply; see [`code`].
pub const MAX_CODE: u64 = NO_ENTROPY;

/// Build a request's first word: the opcode, the identity's length, and the secret's length.
///
/// The lengths ride in the word rather than in the page so the page is payload only, which is what
/// lets the service zero it unconditionally after reading.
pub const fn req(op: u64, id_len: usize, secret_len: usize) -> u64 {
    (op << OP_SHIFT) | ((id_len as u64 & 0xffff) << 16) | (secret_len as u64 & 0xffff)
}

/// The opcode of a request word.
pub const fn op(w0: u64) -> u64 {
    w0 >> OP_SHIFT
}

/// The identity length a request word claims. Not yet checked against [`MAX_IDENTITY`]; that is
/// [`read`]'s job, and it is the reason [`read`] exists rather than two accessors.
pub const fn id_len(w0: u64) -> usize {
    ((w0 >> 16) & 0xffff) as usize
}

/// The secret length a request word claims. Same caveat as [`id_len`].
pub const fn secret_len(w0: u64) -> usize {
    (w0 & 0xffff) as usize
}

/// **Client side**: write `identity` and `secret` into the shared page and return the request word
/// to `CALL` with. `None` if either is empty or over its bound, which is a caller bug rather than
/// a wire condition, so it never becomes a message.
///
/// An empty identity is refused here and not merely at the server, because "the empty name" is the
/// one string that would otherwise collide with an unwritten slot in the service's fixed-size
/// store. `cred::Store` refuses it too; two refusals for one hazard is deliberate.
pub fn place(page: &mut [u8], identity: &[u8], secret: &[u8], op: u64) -> Option<u64> {
    if page.len() < SECRET_OFF + MAX_SECRET {
        return None;
    }
    if identity.is_empty() || identity.len() > MAX_IDENTITY {
        return None;
    }
    if secret.is_empty() || secret.len() > MAX_SECRET {
        return None;
    }
    page[ID_OFF..SECRET_OFF].fill(0);
    page[ID_OFF..ID_OFF + identity.len()].copy_from_slice(identity);
    page[SECRET_OFF..SECRET_OFF + MAX_SECRET].fill(0);
    page[SECRET_OFF..SECRET_OFF + secret.len()].copy_from_slice(secret);
    Some(req(op, identity.len(), secret.len()))
}

/// **Server side**: the identity and the secret a request word points at, or `None` if the lengths
/// are not ones this contract allows. Total: every `u64` either names a well-formed request or is
/// rejected, so the serve loop has no arithmetic that could run off the page.
pub fn read(page: &[u8], w0: u64) -> Option<(&[u8], &[u8])> {
    let (i, s) = (id_len(w0), secret_len(w0));
    if i == 0 || i > MAX_IDENTITY || s == 0 || s > MAX_SECRET {
        return None;
    }
    if page.len() < SECRET_OFF + MAX_SECRET {
        return None;
    }
    Some((&page[ID_OFF..ID_OFF + i], &page[SECRET_OFF..SECRET_OFF + s]))
}

/// **Zero the request area of a shared page.** The service calls this after every request it
/// reads, so the frame both parties map holds neither the presented secret nor anything else once
/// the reply lands. A client can call it too, and should if it is going to keep running.
///
/// `write_volatile` rather than `fill`, because a compiler that can prove nobody reads these bytes
/// again is entitled to delete a plain store, and "the optimiser removed the wipe" is the classic
/// way a zeroing loop turns into a comment.
pub fn wipe(page: &mut [u8]) {
    let n = page.len().min(SECRET_OFF + MAX_SECRET);
    for b in &mut page[..n] {
        // SAFETY: `b` is a live, unique, aligned reference to one byte of the caller's slice.
        unsafe { core::ptr::write_volatile(b, 0) };
    }
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
}

/// **Read a reply's first word.** `Some(code)` when the credential service answered; `None` when
/// the word did not come from it at all.
///
/// The same free discrimination `entropy_proto::delivered` gets, and for the same reason: every
/// reply code is a small positive, while every failure the *kernel* can return from a `CALL` is
/// one of its small negatives (`abi::Error`, -1 to -8), which read as enormous `u64`s. So "there
/// is no credential service" is distinguishable from "the answer is no" with no probe request,
/// which matters more here than anywhere else: a caller that mistook a missing capability for a
/// successful authentication would be the single worst bug this crate could permit.
pub const fn code(r0: u64) -> Option<u64> {
    if r0 >= OK && r0 <= MAX_CODE {
        Some(r0)
    } else {
        None
    }
}

/// **The one question this whole contract exists to answer**, with the failure modes collapsed the
/// way a caller must treat them: anything that is not an unambiguous [`MATCH`] is a refusal.
///
/// A missing capability, a malformed request, a service that died, and a wrong password all become
/// `false` here. That is the safe direction, and having it in the contract means no caller has to
/// remember which of five codes were the good ones.
pub const fn authenticated(r0: u64) -> bool {
    matches!(code(r0), Some(MATCH))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_round_trips_its_opcode_and_both_lengths() {
        let w = req(verify::VERIFY, 7, 260);
        assert_eq!(op(w), verify::VERIFY);
        assert_eq!(id_len(w), 7);
        assert_eq!(secret_len(w), 260);
    }

    /// **The wire layout, pinned as one exact number.** The serve loop on the far side of the
    /// endpoint decodes this word with its own copy of these shifts, so the bit positions are the
    /// contract, not an implementation detail a refactor may move. The opcode is `SEAL` on
    /// purpose: `PUT` and `VERIFY` are both 1, and a test that only ever builds opcode 1 cannot
    /// tell a working `op` from one that returns a constant.
    #[test]
    fn the_request_word_is_the_documented_bit_layout() {
        let w = req(provision::SEAL, MAX_IDENTITY, MAX_SECRET);
        assert_eq!(w, 0x0200_0000_0040_0100);
        assert_eq!(op(w), provision::SEAL);
        assert_eq!(id_len(w), MAX_IDENTITY);
        assert_eq!(secret_len(w), MAX_SECRET);
    }

    #[test]
    fn place_and_read_are_inverses() {
        let mut page = [0xAAu8; PAGE];
        let w = place(&mut page, b"corinne", b"hunter2", verify::VERIFY).unwrap();
        let (id, secret) = read(&page, w).unwrap();
        assert_eq!(id, b"corinne");
        assert_eq!(secret, b"hunter2");
    }

    /// A short identity must not shift the secret, and must not leave the previous caller's
    /// identity visible past its end. Both are the kind of thing a fixed-offset layout gets right
    /// by construction and a packed one gets wrong once.
    #[test]
    fn a_second_request_leaves_nothing_of_the_first() {
        let mut page = [0u8; PAGE];
        place(
            &mut page,
            b"a-very-long-identity-name",
            b"a-long-secret-value",
            verify::VERIFY,
        )
        .unwrap();
        let w = place(&mut page, b"cd", b"ef", verify::VERIFY).unwrap();
        let (id, secret) = read(&page, w).unwrap();
        assert_eq!(id, b"cd");
        assert_eq!(secret, b"ef");
        assert!(
            page[..SECRET_OFF + MAX_SECRET]
                .windows(4)
                .all(|w| w != b"very"),
            "the previous identity survived into the next request",
        );
    }

    /// **The parse is total.** Nothing a client can put in the request word makes the server read
    /// outside the page or believe a length it should not.
    #[test]
    fn every_out_of_range_length_is_refused() {
        let page = [0u8; PAGE];
        for (i, s) in [
            (0, 8),
            (8, 0),
            (MAX_IDENTITY + 1, 8),
            (8, MAX_SECRET + 1),
            (0xffff, 0xffff),
        ] {
            assert!(
                read(&page, req(verify::VERIFY, i, s)).is_none(),
                "id_len {i}, secret_len {s} should be refused",
            );
        }
        assert!(read(&page, req(verify::VERIFY, MAX_IDENTITY, MAX_SECRET)).is_some());
    }

    /// A page too small to hold the fixed layout is refused rather than indexed into. The service
    /// maps a whole frame, so this is a contract that cannot be violated in the tree today; the
    /// check is here so a future caller with a smaller buffer fails loudly instead of panicking.
    #[test]
    fn a_page_too_small_for_the_layout_is_refused_at_both_ends() {
        let mut small = [0u8; SECRET_OFF + MAX_SECRET - 1];
        assert!(place(&mut small, b"x", b"y", verify::VERIFY).is_none());
        assert!(read(&small, req(verify::VERIFY, 1, 1)).is_none());
    }

    /// A page of exactly `SECRET_OFF + MAX_SECRET` bytes is the smallest the fixed layout fits,
    /// and both ends must accept it: the size check refuses a page the layout runs off, not a
    /// page with no slack after it.
    #[test]
    fn the_smallest_page_the_layout_fits_is_accepted_at_both_ends() {
        let mut page = [0u8; SECRET_OFF + MAX_SECRET];
        let w = place(&mut page, b"id", b"secret", verify::VERIFY).unwrap();
        let (id, secret) = read(&page, w).unwrap();
        assert_eq!(id, b"id");
        assert_eq!(secret, b"secret");
    }

    #[test]
    fn an_empty_identity_or_secret_never_becomes_a_request() {
        let mut page = [0u8; PAGE];
        assert!(place(&mut page, b"", b"secret", verify::VERIFY).is_none());
        assert!(place(&mut page, b"id", b"", verify::VERIFY).is_none());
    }

    #[test]
    fn place_refuses_what_will_not_fit() {
        let mut page = [0u8; PAGE];
        assert!(place(&mut page, &[b'x'; MAX_IDENTITY + 1], b"s", verify::VERIFY).is_none());
        assert!(place(&mut page, b"i", &[b'x'; MAX_SECRET + 1], verify::VERIFY).is_none());
        assert!(
            place(
                &mut page,
                &[b'x'; MAX_IDENTITY],
                &[b'x'; MAX_SECRET],
                verify::VERIFY
            )
            .is_some()
        );
    }

    #[test]
    fn wipe_clears_the_whole_request_area_and_nothing_past_it() {
        let mut page = [0xFFu8; PAGE];
        place(&mut page, b"identity", b"secret", verify::VERIFY).unwrap();
        wipe(&mut page);
        assert!(page[..SECRET_OFF + MAX_SECRET].iter().all(|&b| b == 0));
        // The rest of the page is not wipe's to clear: the frame is shared, and a wipe whose
        // bound has grown is a wipe scribbling on state it was never told about.
        assert!(page[SECRET_OFF + MAX_SECRET..].iter().all(|&b| b == 0xFF));
    }

    /// **The property the whole error story rests on**: no reply code collides with any error the
    /// kernel can return, so a caller holding no credential capability cannot mistake a refusal
    /// for a `MATCH`. `abi::Error` is -1..-8; those are the words a `CALL` on an empty slot puts
    /// in the first register.
    #[test]
    fn a_kernel_refusal_is_never_mistaken_for_a_verdict() {
        for c in OK..=MAX_CODE {
            assert_eq!(code(c), Some(c));
        }
        for err in 1i64..=8 {
            assert_eq!(code((-err) as u64), None, "kernel error -{err}");
            assert!(!authenticated((-err) as u64), "kernel error -{err}");
        }
        assert_eq!(code(0), None);
        assert_eq!(code(MAX_CODE + 1), None);
    }

    /// Only [`MATCH`] authenticates. Written as an exhaustive sweep rather than a handful of cases
    /// because the failure this guards against is a *new* code being added later and quietly
    /// falling on the permissive side.
    #[test]
    fn nothing_but_match_authenticates() {
        for w in 0u64..=MAX_CODE + 4 {
            assert_eq!(authenticated(w), w == MATCH, "reply word {w}");
        }
        assert!(!authenticated(u64::MAX));
    }
}

/// **Proofs, for the two properties a test can only sample** (`script/verify`).
///
/// The tests above check the interesting cases. These check *every* case, which matters here more
/// than it does for most of this tree, because both properties are about what an adversary can
/// send or receive and an adversary is not limited to the values a test author thought of. There
/// are 2^64 request words and 2^64 reply words; the tests cover a few dozen each.
#[cfg(kani)]
mod proofs {
    use super::*;

    /// **The server's parse is total.** For *any* first word a client can send, `read` either
    /// refuses it or hands back two slices that lie inside the page and have exactly the lengths
    /// the word claimed. Kani proves the memory safety (no index runs off the page for any input);
    /// the assertions pin the meaning, so a future rewrite that stayed in bounds while returning
    /// the wrong bytes would still fail.
    ///
    /// This is the property that lets the credential service's serve loop have no arithmetic in it
    /// that could go wrong. It is also the one an attacker probes first.
    #[kani::proof]
    fn no_request_word_makes_the_parse_read_outside_the_page() {
        let page = [0u8; SECRET_OFF + MAX_SECRET];
        let w0: u64 = kani::any();
        match read(&page, w0) {
            None => {}
            Some((identity, secret)) => {
                assert!(identity.len() == id_len(w0));
                assert!(secret.len() == secret_len(w0));
                assert!(!identity.is_empty() && identity.len() <= MAX_IDENTITY);
                assert!(!secret.is_empty() && secret.len() <= MAX_SECRET);
                assert!(ID_OFF + identity.len() <= SECRET_OFF);
                assert!(SECRET_OFF + secret.len() <= page.len());
            }
        }
    }

    /// **Nothing but `MATCH` authenticates, for every word in the space.** The failure this rules
    /// out is the worst one this contract could permit: a caller holding no credential capability,
    /// or talking to a service that died, reading the kernel's refusal as a successful login. The
    /// host test sweeps a few dozen values around the boundary; this sweeps all of them.
    #[kani::proof]
    fn no_reply_word_but_match_ever_authenticates() {
        let r0: u64 = kani::any();
        assert!(authenticated(r0) == (r0 == MATCH));
        // And the discrimination itself: a code is a code exactly when it is in range, so no
        // arithmetic on a kernel error word can land inside the reply space.
        match code(r0) {
            Some(c) => assert!(c == r0 && (OK..=MAX_CODE).contains(&c)),
            None => assert!(!(OK..=MAX_CODE).contains(&r0)),
        }
    }

    /// **A request word round-trips its three fields** for every combination the builder accepts,
    /// so the opcode a server dispatches on is the opcode the client chose and the lengths it
    /// parses are the lengths the client meant. Bounded to the ranges `place` can produce, because
    /// outside them the packing is deliberately lossy (the fields are masked) and there is nothing
    /// to prove.
    #[kani::proof]
    fn a_request_word_round_trips_every_field() {
        let op_in: u64 = kani::any();
        let i: usize = kani::any();
        let s: usize = kani::any();
        kani::assume(op_in <= 0xff);
        kani::assume(i <= MAX_IDENTITY);
        kani::assume(s <= MAX_SECRET);
        let w = req(op_in, i, s);
        assert!(op(w) == op_in);
        assert!(id_len(w) == i);
        assert!(secret_len(w) == s);
    }
}

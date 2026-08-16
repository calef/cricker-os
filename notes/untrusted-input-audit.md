# Auditing untrusted counterparty input: the network and the bus

Done 2026-08-15, as milestone 43 continued. Milestone 43 is the second security audit, and its
headline lens (time of check to time of use across the shared pages) was already carried out and is
written up in full in [shared-page-audit.md](shared-page-audit.md), seven findings and a merged set
of fixes. This note takes the milestone's **further** lenses, the ones that block names and that the
double-fetch pass explicitly left for later, and points them at the surface that landed *after* that
pass was written.

The value of a repeated audit is a lens the previous one lacked. The first audit
([arch-audit.md](arch-audit.md)) read the hand-written assembly for state staged across instructions.
The second ([shared-page-audit.md](shared-page-audit.md)) read the shared-page contracts for a value
one party checks and another can rewrite before it is used. This one reads for a third shape.

## The lens, and why this one

> A value a **hostile counterparty supplies in a single message or completion**, which code inside
> the machine parses or trusts in one read.

The double-fetch lens needed a *concurrent writer*: two reads of one page with a window between them.
This lens needs no window and no second read. It is the older and simpler question, "is this one
value from an untrusted source bounded before it is believed," asked of the code that has an
untrusted source for the first time.

Two things make it the right lens for today's tree rather than a re-run of
[security.md](security.md)'s general review:

- **The counterparty is now genuinely outside the machine.** `crates/mdns_proto` decodes datagrams
  that arrive from the local network, including the DNS name-compression pointers that are the
  canonical decompression-bomb and pointer-loop vector. `crates/nvme` is a kernel driver that reads
  16-byte completions a PCIe device writes into memory. Neither existed when the shared-page audit
  read the tree, and both take input from a party the threat model (DECISIONS §20, §23, §30, and
  SECURITY.md) declares untrusted.
- **The secret-material crates were explicitly out of the previous scope.** The shared-page audit
  recorded that `crates/cred_proto` and `user/src/credentialer.rs` were "being substantially
  rewritten with an NTLM path" and that "the clearance recorded below is of the version on `main` and
  does not transfer." That rewrite has landed (`crates/ntlm`, `crates/cred`), so §79's secret-material
  rules want a fresh read.

The four questions asked of each site are the arch audit's, transposed one more time:

- **(a) The value.** Which field arrives from the untrusted party, and where is it used?
- **(b) The source.** Is the party a network peer, a bus device, or a local caller, and what does the
  threat model say about trusting it?
- **(c) The corrupted state.** What does the code do with the value if it is a lie?
- **(d) Reachable?** Is the misuse closed by a bound, by a proof, or only by the wiring not yet
  existing?

## What was audited

Every crate on `main` at `32f835a1` that takes input from a network peer or a bus device and was
added or rewritten after the shared-page audit read the tree (it read `313a055` on 2026-08-04).

| Crate / driver | Untrusted source | First landed |
|---|---|---|
| `crates/mdns_proto` | a datagram from a network peer | 2026-08-15 |
| `crates/nvme` + `kernel/src/nvme.rs` | a PCIe device's completions and identify data | 2026-08-14 |
| `crates/ntlm`, `crates/cred` | a presented secret and an NTLM client blob | 2026-08-04 |

## What was deliberately not examined

Stated because a scope nobody wrote down is a scope nobody can check.

- **The SMB server, `smb_proto`, and the mDNS *wiring*.** None is on `main` at the audited commit;
  all are in lanes in flight this session. `mdns_proto` is audited here as a crate, with the
  reachability caveat below, but the responder that will feed it live datagrams is not on the base and
  is not read. The SMB request parser, which will be the first hand-written parser of attacker bytes
  actually reachable at runtime, is a lane of its own the day it merges, and this note does not claim
  to cover it.
- **The double-fetch lens itself**, which is [shared-page-audit.md](shared-page-audit.md)'s and would
  be the failure this milestone exists to avoid.
- **Capability-lifetime races** (revocation against an in-flight use) and **the `unsafe` census**,
  both named in the block as candidate lenses and both a whole audit each. Untouched here.
- **The arch and assembly layer**, which is [arch-audit.md](arch-audit.md)'s.
- **`crates/dma_validator` and the IOMMU descriptor path**, which have their own machine-checked
  proofs (DECISIONS §30); reading them by hand adds nothing a prover has not already said. This note
  reads what the driver does with what the device *writes back*, which is the direction those proofs
  do not cover, exactly as shared-page-audit.md's finding 6 established.

## Findings

### 1. The NVMe kernel driver turns two device-written completion fields into a kernel panic

**(a) The value.** `kernel/src/nvme.rs`'s `submit_and_poll` reads a 16-byte completion the controller
wrote and then consumes two of its fields:

```rust
// self.io_sq.note_head(c.sq_head), for the I/O queue:
self.admin_sq.note_head(c.sq_head);          // c.sq_head is the controller's

assert!(
    c.cid == expect_cid,                     // c.cid is the controller's, echoed back
    "NVMe completion for cid {} while {} was in flight", c.cid, expect_cid
);
```

`note_head` (in `crates/nvme`) is:

```rust
pub fn note_head(&mut self, head: u16) {
    assert!(head < self.entries, "controller reported an impossible head");
    self.head = head;
}
```

Both `c.sq_head` and `c.cid` come straight out of the dwords the device wrote
(`Completion::from_dwords`). A device that writes `sq_head >= entries`, or any `cid` other than the
one command in flight, hits an `assert!` and **panics the kernel**.

**(b) The source.** A PCIe NVMe controller, confined behind the machine's IOMMU
(`kernel/src/nvme.rs:9` "confines the device to it," and the test at line 437 refuses to run without
the IOMMU "without it the confinement claim is untested"). Confined is not trusted: the IOMMU exists
precisely because the device is not (DECISIONS §20, §23, §30). And the IOMMU confines *where* the
device may write, not *what* it writes. The completion queue is memory the device legitimately owns
and fills; the bytes in it are entirely the device's to choose.

**(c) The corrupted state.** A kernel panic, taken deliberately. The `cid` assert's own comment calls
it "a protocol violation worth dying on legibly." For a userspace driver, dying is one process; for
this driver, which runs in the kernel, dying is the whole system. **Memory safety is not at risk**:
the completion is read from `self.dma_va + ... + head*16` where `head` is the driver's own
value, and the comment at line 274 is correct that "reads of our own DMA region are always safe,
whatever the device is writing there." The failure is liveness only. A confined but hostile or merely
buggy controller can halt the machine by writing one wrong `u16`.

**(d) Reachable? By a device, yes, and by nothing else.** Under QEMU with its own NVMe model the
device completes synchronously and honestly, so it is not reachable in the suite. On real hardware,
a firmware bug or a hostile controller reaches it with a single malformed completion, and the panic
surfaces as a kernel crash that reads like a kernel bug rather than a device one.

This is the exact reciprocal of shared-page-audit.md's finding 6, one layer down. That finding read
`user/src/net_transport.rs` and `kbd.rs` trusting a `u32` the device wrote into a used ring, and its
disposition was to **fail closed**: consume the bad completion and drop it, costing one buffer per
lie. The NVMe driver, newer and in the kernel, made the opposite choice for the same class of value,
and the pattern shared-page-audit.md named for finding 6 applies verbatim: **a guarantee assumed
from the wrong side of a boundary.** The IOMMU's guarantee is about where the device may touch. The
driver read it as a guarantee about what the device may say.

**Disposition: recorded, and it wants a lane.** The fix is not one line and not zero risk: turning
`note_head` and the `cid` assert into an `Err(Error::...)` that the caller propagates changes the
error contract of `submit_and_poll` and every path above it, and it needs a negative control (a
device that lies) to prove the new path fails closed rather than mis-serving. That control is
precisely the hostile-device harness shared-page-audit.md already proposed as its lane candidate B,
and this finding extends that candidate's justification rather than adding a new one. Note this is
consistent with `crates/nvme`'s own design comment on `SqState`: it deliberately does not model the
controller's head as a free-slot count "instead of carrying a free-slot count nothing would
exercise," which is the right call for the honest path and is exactly why the dishonest path lands on
an assert.

## Candidates cleared, and why each is safe

Recorded because "we looked and it is fine" is the other half of an audit, and because each is a
place a future change could break something.

**`crates/mdns_proto`'s name decoder, `decode_name_into`.** This is the classic DNS parser
vulnerability surface (a compression pointer that loops, or a name that expands without bound), and
it is written to close both by construction:

- **Termination.** A `fence`, initialised to the name's start offset, bounds every compression
  pointer: a pointer must target strictly below the current `fence`, and following it lowers the
  fence to the target. Each pointer therefore strictly decreases a non-negative integer, so the
  number of pointers is bounded and a loop is impossible. Between pointers, each label emits at least
  its length byte into the output, which is bounded at 255 (`Error::NameTooLong`), so the forward
  runs are bounded too. `src/proofs.rs` carries a Kani harness proving termination over the same loop
  against a small output buffer, with the measurement (a 255-byte symbolic output was twenty-plus
  solver minutes; the small buffer is CI-fast and the loop is identical).
- **Bounds.** Every read of the message is `msg.get(pos)`, `msg.get(pos + 1)`, or
  `msg.get(pos + 1..pos + 1 + l)`, each returning `Error::Truncated` on overrun; every write is
  bounds-checked against the output length before it happens. There is no raw index on the decode
  path. `Reader::record` bounds RDATA with `.get(rdata_off..rdata_off + rdlen)`, and `rdlen` is a
  `u16`, so the sum cannot overflow a `usize` on this kernel's 64-bit targets.
- **Reachability caveat.** No program on `main` at the audited commit calls `mdns_proto`. It is a
  protocol crate whose responder is in a lane not yet merged, so the decoder is *not reachable at
  runtime today*. It is cleared as a crate, on the reading above and its own proof; the wiring that
  will feed it real datagrams is a separate read the day it lands.

**`crates/nvme`'s `parse_identify_namespace`.** Reads the namespace size and LBA format from the
4096-byte identify page the device fills. `data.len()` is checked against 384; `flbas` is a 4-bit
field so `data[128 + 4*flbas + 2]` reaches at most index 190; and `lbads` (the device's bytes-per-block
shift) is rejected unless it is in `9..=12`, so `1 << lba_shift` is at most 4096 and cannot
over-shift. `bytes()` can overflow a `u64` if the device reports an absurd `nsze`, but the product is
not used to bound any read into a fixed buffer (transfers go to the device through PRPs), so the
overflow is benign. The only device-written values that reach control flow unbounded are the two
completion fields of finding 1.

**`crates/nvme`'s completion read itself.** The completion is read from the driver's own `head` slot,
not from any device-supplied index, and `CqState::owned` distinguishes fresh from stale by the phase
tag, not by `cid`. So `cid` is never used to index anything (finding 1 is that it is used in an
*assert*, not that it indexes memory), and the read is memory-safe whatever the device writes.

**`crates/cred` and `crates/ntlm`, against §79.** The secret-material rules are followed, and in
several places the code is already at the standard an audit would ask for:

- **The tag comparison is constant-time** (`subtle`), and the identity lookup is constant-time and
  does not stop at the first match, so neither a wrong secret nor a missing identity is
  distinguishable by timing. `Record` deliberately has **no `PartialEq`**, with a comment naming the
  reason: a derived one would compare tags with a short-circuiting `memcmp`, the exact timing oracle
  `Store::verify` avoids.
- **The service stores `NTOWFv2`, not the password and not the NT hash** (`crates/ntlm`'s header,
  [MS-NLMP] §3.3.2), which is §79's whole point: a stolen `NTOWFv2` authenticates as one account in
  one domain and is not the reusable secret an NT hash is. The `has_ntlm` flag is selected in
  constant time with the key material so an unprovisioned record does not carry a known HMAC key that
  anyone could forge a proof under.
- **The no-`zeroize` choice is deliberate and written down**, not an omission: `crates/ntlm`'s header
  argues that the whole address space is the secret's blast radius already, so scrubbing one local is
  theatre. That is a recorded decision, which is the right rung for it.
- **The honest limits are named where a reader meets them**: secrets-at-rest is unsolved
  (notes/credentials.md), there is no rehash-on-verify when cost parameters move, and no lockout.
  Provisioning an NTLM secret *lowers* the strength of a record (an unsalted `NTOWFv2` beside a
  salted Argon2id tag), and `crates/cred` says so at the method rather than hiding it. None of these
  is a finding; each is a limitation recorded in the place §71 wants it.

## The honest summary

| | What | Disposition |
|---|---|---|
| 1 | The NVMe kernel driver panics on a device-written `sq_head` out of range and on any `cid` but the one in flight; the IOMMU confines where the device writes, not what it says | **Recorded**, wants a lane (extends shared-page-audit.md's hostile-device harness) |

**Nothing found is a memory-safety hole or a live privilege escalation.** The one finding is a
device-triggered kernel denial of service, not reachable under QEMU, reachable on real hardware behind
a hostile or buggy controller. It is the same class shared-page-audit.md's finding 6 fixed in
userspace, reappearing in the kernel with the opposite disposition, which is the single most useful
thing this pass hands forward:

> **The IOMMU confines placement, not values.** A driver behind an IOMMU may still not trust the
> numbers the device writes into the memory it legitimately owns. shared-page-audit.md's finding 6
> failed closed on exactly this; the newer NVMe driver asserts, and an assert in the kernel is a
> crash. The rule the two together state is that a confined device's *accounting* is as untrusted as
> its *reach*.

The limit is the one every audit-by-reading carries and this one carries twice over: it reads one
commit, and two of its three subjects are half-built. `mdns_proto`'s decoder is proven and its wiring
is not here; the SMB parser that will be the first attacker-reachable hand-written parser is not on
the base at all. The clearances above are of the crates as they stand, and the responder and the SMB
server each want their own read the day they land.

## What wants a lane

**Extend the hostile-device harness (shared-page-audit.md's candidate B) to NVMe, and fail closed.**
The case is finding 1. Something that can write an arbitrary completion under the driver, whether a
fake transport behind a trait or a QEMU device model, would let a test assert that a bad `sq_head` or
`cid` fails the operation rather than the kernel. The fix and its proof are one lane: convert the two
asserts to a propagated error, and prove with the harness that the honest path is unchanged and the
hostile path returns an error instead of panicking. This is the same deliverable shared-page-audit.md
named, with a second driver now depending on it.

---

*See also [shared-page-audit.md](shared-page-audit.md) for the second audit and its double-fetch
lens (finding 6 is finding 1's sibling), [arch-audit.md](arch-audit.md) for the first,
[security.md](security.md) for the general kernel review, [iommu.md](iommu.md) and DECISIONS §20/§23/§30
for the confinement claim finding 1 tests, [credentials.md](credentials.md) and notes/ntlm.md for the
secret-material contract, and `SECURITY.md` for what this project claims and what it does not.*

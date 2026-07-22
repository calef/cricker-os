# How authority moves, narrows, and ends

The companion to [ipc-naming.md](ipc-naming.md). That note is about *naming* (IPC names an
endpoint, never the peer). This one is about the *lifecycle* of the capabilities themselves: how
authority is copied, how it narrows, and, at the end, why it cannot yet be revoked.

## Authority moves by copy-with-narrowing, never by widening

A `Cap<O>` is `Copy` (`crates/caps`). Authority spreads by **deriving** a copy, and the one rule
is that a derivative's rights are a **subset** of the source's:

```rust
// CSpace::derive: "copy a capability into another slot, with rights that are no greater."
if !rights.is_subset_of(src.rights) { return Err(NoRights); }
```

`Rights` are three bits: `READ`, `WRITE`, `GRANT`. `is_subset_of` is the whole enforcement; there
is no code path that widens rights, which is the point (DECISIONS.md §10): if delegation could
widen authority, the model is theatre.

## `SEND_CAP` is share, not move

Delegating a capability over IPC (`syscall.rs`, `SEND_CAP`) **reads** the sender's cap and delivers
a *new* one to the receiver:

```rust
let src = current_cap(a0)?;                 // read; the sender's slot is NOT emptied
if !src.rights.allows(GRANT) { return NotPermitted; }   // may I pass it on at all?
let narrowed = Rights::from_bits(a1);
if !narrowed.is_subset_of(src.rights) { return NotPermitted; }  // only narrow
ipc_send_cap(ep, data, Cap { object: src.object, rights: narrowed });
```

So the sender **keeps its capability**; the receiver gets a narrowed derivative pointing at the same
object. That is exactly what lets a frame be shared: a producer holding `READ|WRITE|GRANT` keeps its
writable mapping while handing a consumer a read-only view of the same physical page.

## Two independent narrowings

Delegation answers two separate questions, and they narrow independently:

| Question | Right | Example |
|---|---|---|
| What may the holder **do**? | `READ`, `WRITE` | a `Frame` with `READ` alone maps read-only, never writable |
| May the holder **pass it on**? | `GRANT` | a derivative sent *without* `GRANT` is a dead end: the receiver may use it but not re-delegate |

`SEND_CAP` needs `WRITE` on the *endpoint* (may I send here?) **and** `GRANT` on the *delegated*
capability (was I trusted to lend it?). Two rights, two objects, two questions.

## Frames, end to end

The frame path shows every piece confining the next:

1. `Untyped::RETYPE` mints the owner a `Frame` with `READ|WRITE|GRANT` (`syscall.rs:181`).
2. The owner maps it writable (`Frame::MAP` with the writable flag needs `WRITE`).
3. The owner delegates a **`READ`-only, no-`GRANT`** derivative with `SEND_CAP`.
4. The consumer's `Frame::MAP` sees `READ` without `WRITE`, so it is confined to `user_rodata`: it
   maps the same physical page but **cannot write it, and cannot pass it on**.

The test `a_frame_capability_shares_a_page_and_a_read_only_view_cannot_write_it` pins exactly this.
This is DECISIONS.md §10's "shared memory carries data," composed by the processes at runtime rather
than wired by the kernel at spawn. Read-only derivatives at send time: yes, and enforced all the way
to the page-table bits.

## What a read-only shared frame does and does NOT promise (the consumer's contract)

This is the part that gets misremembered later, so it is worth stating as a contract rather than
leaving as an emergent property of a tested mechanism.

A read-only derivative is *share, not move* (above): the producer keeps `WRITE` and a **writable
mapping of the same physical page.** So what the read-only bit actually delivers is narrow:

> **It stops the *consumer* from writing the shared page. It does nothing to stop the *producer*
> from writing it while the consumer reads.**

The property is "the consumer cannot corrupt the shared page," **not** "the page is stable under
the consumer's feet," and **not** "the data is trustworthy." A consumer that validates the page and
then acts on it is exposed to the producer mutating it *after* the check (a TOCTOU). So, for a
server reading a buffer shared by an untrusted client:

1. **Take structural data from the message, never from the page.** A length, offset, count, or
   index that you will compute on must ride the IPC message (registers, immutable once sent), not
   live in the mutable shared page. Otherwise the producer edits it under you.
2. **Copy-into-private-then-validate.** If you must validate content and then act on the validated
   form, copy it into your own memory first and validate the copy. The shared page can change
   between your check and your use.
3. **Bound everything by the frame size yourself.** Never trust a producer-supplied count to stay
   within the page.

**The console server is the worked example, and it is safe *because* it follows this** (checked):
the length rides the message (`recv(REQUEST)`), the shared page holds only bytes to print (a
content TOCTOU just prints different bytes — benign), and an over-long length is a *read out of the
server's own mapping* that faults the server, i.e. a crashed driver, not a corrupted kernel
(user/src/hello.rs). A future server that read a length or offset *from the page*, or indexed on
page contents, would not be safe, and the read-only bit would not save it.

## The end of the line: no revocation (yet)

**A capability, once granted, cannot be retracted.** There is no capability-derivation tree, no
refcount, no `revoke`. The only trace of the idea is `untyped.rs`: "revocation of derived objects is
the harder seL4 story parked for later."

The crucial thing is *what that does and does not cost*, because the lifetime makes it narrower than
it sounds:

**It is not a memory-safety hole.** Frames come from **spend-only untyped**: `retype_page` only
advances a watermark and never reclaims (`untyped.rs`). And address-space teardown deliberately does
**not** free a mapped frame's leaf, only the page tables reaching it (`user.rs`: *"the frame is not
recorded for freeing, because we do not own it"*; see [teardown.md](teardown.md)). So a peer that
still maps a shared frame after the granter has exited is mapping **valid, non-reused** memory. No
use-after-free, no double-free. The safety is structural.

**What it does cost is control and reclamation:**

- You cannot **un-share**. Hand a peer a read-only view and then distrust it, and you cannot take
  the mapping back. The only lever is the blunt one: destroy the peer (tear down its address space,
  which unmaps everything it holds). There is no fine-grained "revoke just this frame."
- You cannot **reclaim**. A retyped page is spent from the untyped forever; sharing is one-way until
  the whole untyped region is destroyed.

seL4's answer is a capability-derivation tree plus a recursive `revoke` that walks it, unmapping the
object from every holder. It is expensive (a tree walk) and it is kernel-tracked (every derivation
recorded), which is precisely why it is a first-class object there and parked here. See
[DECISIONS.md](../DECISIONS.md) "Open design ideas" for the deferral and its trigger.

## Where authority can enter at all

Only three ways, and none is ambient: **retype** it out of untyped you hold, be **handed** it
(`SEND_CAP` / spawn-time grant), or **derive** a narrower copy of one you already have. There is no
`open(path)`, no global name, no way to mint authority from who you are. A thread's cspace is empty
until something puts a capability in it. That is the whole of §10, seen from the object's side.

---

*Add to this file as new capability-lifecycle questions come up.*

# 44. Entropy is a capability, `std::random` improves transparently, and the refusal is loud

**Status: DECIDED.**

**Built 2026-07-30** (milestone 56, the entropy half: the virtio-rng driver, the entropy service,
and the std PAL). Concept note: notes/entropy.md. The contract is `crates/entropy_proto`.

Before this, `std::random` on this target was splitmix64 seeded from the virtual counter, and its
own first line said so: "not cryptographic, and saying so is the point". The counter is the ABI's
one ambient readable, so the stream was predictable to anyone who could guess boot-relative time.
That caveat tainted anything security-adjacent anywhere in the tree and blocked SMB authentication
outright, because an NTLMv2 server challenge that is guessable is precomputable.

## The device is held by one process; everyone else holds "you may ask"

The service owns the `Virtio` capability, the `Irq`, and the DMA page the device writes into.
A client holds **one endpoint**, and that endpoint names no device: it cannot program the queue,
cannot map the page, and cannot ask the device for anything the service did not ask on its behalf.

This is the same attenuation-by-operation the roadmap names as a principle: the NTP client that may
propose a time but not set it, the clock's read/set/propose ladder ([§43](43-clock-authority.md)), and now
obtain-but-not-reach. It also needed **nothing new**: no capability type, no syscall, no method
number. An `Endpoint` with WRITE is the whole grant, and `caps` already prints it.

The blast radius is worth stating because it is the security claim: compromise the entropy service
and you choose the machine's random numbers. That is a lot, which is exactly why it is one small
process holding one device rather than a facility inside every program.

## The service passes bytes through, and computes nothing

**No pool, no whitening, no mixing, no DRBG.** There is no cryptographic primitive in this tree
(vendoring one is milestone 56's other half and its own decision), and without a one-way function
every transformation available here is a *reversible permutation*: it would change the bytes without
adding an unpredictability an attacker could not undo, while making the security property harder to
state. So the property stays one sentence: **these are the device's bytes.**

What the service does keep is a **256-byte buffer**, and the distinction from a pool is the point:
byte *i* out is byte *i* in, unmodified, served to exactly one client, and zeroed behind the cursor.
It is a cache for round trips, not an entropy transformation, and it turns thirty-two device
requests into one.

**A short read is asked again, and the boundary is not the client's problem.** virtio-rng may return
fewer bytes than the buffer holds and says how many in the used ring's `len`. **QEMU's really does**,
which is a measurement rather than a spec allowance: the first version passed the short buffer
through to the client and the test caught a five-byte reply to an eight-byte request thirty draws
in. So the service gathers across the boundary, and a count below what was asked means one thing
only, that the device went dry part-way through. It never pads, never repeats a byte it has served,
and never substitutes a pseudo-random stand-in. A device that produces nothing across four attempts
gets `NO_ENTROPY` and the caller finds out, which is [§42](42-truthful-filesystem.md)'s no-silent-degradation rule applied to
the one payload where degrading quietly is worst.

## The bytes ride in the reply, not in a shared page

[§10](10-capability-microkernel.md) says bulk rides in a page and control rides in
the message, and this contract deliberately does not. A page shared with a client is a place the
bytes **persist** and a second party can read, and random bytes are the payload whose entire value
is that nobody else has seen them; registers and the client's own stack are a smaller footprint than
a page both parties map. The cost is one round trip per eight bytes, which is a real cost and is
recorded rather than waved away: a 32-byte key is four round trips.

The reply's first word is a **byte count in `0..=8`**, which cannot collide with any of the kernel's
own errors (-1..-8, which read as enormous `u64`s). So "there is no entropy service" and "the
service has no entropy" are distinguishable with no probe request and no ambiguity. `fs_proto` could
not manage that (its errno space collides with the kernel's, a wart notes/std.md records) and a
contract this new had no excuse to inherit the collision.

## The fork: `std::random` improves transparently, split on std's own seam

The milestone block left this open: does `std::random` improve transparently, or must a program ask
for a real RNG? **Transparent**, and the honesty that "explicit" would have bought is preserved by
refusing rather than by degrading. The two callers split where std already splits them:

| std entry point | promise | with the capability | without it |
|---|---|---|---|
| `fill_bytes` (`std::random::SystemRng`) | std documents it as "suitable for cryptographic purposes such as key generation" | the device's bytes | **panic**, naming the reason |
| `hashmap_random_keys` (`RandomState`) | DoS resistance for a hash table, nothing more | the device's bytes | the old counter-seeded splitmix64 |

Explicit was tempting and is the wrong trade here. A nife-only "ask for a real RNG" API means
every program that wants entropy is a program that will not build anywhere else, which is a real
cost against a demonstration OS whose claim is that ordinary Rust runs on it. And it would not have
bought the honesty: the thing that makes a caller safe is that the weak path **cannot be reached
from the strong one**, and that is a property of this file's structure, not of the caller's spelling.

`fill_bytes` has no error channel, so the only loud refusal available is a panic. That is §43's
`SystemTime::now()` decision applied a second time, and it is the same trade: a program that never
asks is unaffected, and a program that asks gets told instead of quietly stamping a key with
something guessable. **`hashmap_random_keys` is the one place a fallback is right**, because a
`HashMap` in a program nobody granted entropy must still work, and because std's own `unsupported`
backend degrades that same function (to allocation addresses) rather than failing. The splitmix64
stream survives there, clearly labelled, and no key is ever minted from it.

The mechanical cost is one more anchor in the std-src patcher: exporting `hashmap_random_keys`
means std's blanket `#[cfg(not(any(...)))]` definition of it must exclude nife, or the two
collide.

## Both transports, because a random source that works on one bus is not a random source

virtio-mmio and PCIe, one binary, chosen by the wiring (§18's seam). The PCIe instance sits behind
the IOMMU and the test asserts it: the buffer this device writes is where the machine's key material
comes from, so an unconfined device writing it is the last thing to leave unchecked.

The driver **looks at the used ring before it blocks**, which is a change from the disk driver's
shape and is a fact about the board rather than an optimisation. `pci::intx_irq` swizzles INTx by
device number modulo four, `sched::bind_irq` routes an intid to exactly one endpoint, and the test
leg now attaches five PCI functions. There is no unshared line left, so a driver that blocked before
looking would be betting on owning its line. The interrupt wait is still there and is what a
genuinely asynchronous device gets; QEMU completes inside `NOTIFY`, so the fast path never blocks.

## What this lane deliberately did not do

- **No cryptography.** No hash, no cipher, no DRBG. Vendoring RustCrypto is the other half of
  milestone 56 and is its own decision; this lane would have had to make it badly and early.
- **No hardware TRNG.** The StarFive JH7110's TRNG is the candidate for the VisionFive 2 and
  **needs verifying** before it is relied on. Under QEMU the device is backed by the host's
  `/dev/urandom`, which is what makes these bytes real, and which is a fact about the emulator
  rather than a property of the driver.
- **No entropy for every program by default.** `init` does not endow the shell or its children with
  the entropy endpoint; the std wiring does, and the milestone-56 tests do. Ambient entropy would be
  ambient authority, and the point of the grant is that a program's dependence on randomness is
  visible in what it holds.
- **No rate limit and no quota.** A client holding the endpoint can drain the service as fast as it
  can `CALL`. Eight bytes per round trip is a cost, not a defence, and nothing here should be read
  as one.

# Entropy

Where random numbers come from on this machine, who is allowed to reach the device, and what
`std::random` does when nobody granted you any. Milestone 56, the entropy half; the decision and its
argument are [DECISIONS §44](../design/decisions/44-entropy-capability.md), the contract is `crates/entropy_proto`.

## The thing this replaced

`std::random` used to be splitmix64 seeded from the virtual counter, and the file said so in its
first line: *"not cryptographic, and saying so is the point."* The counter is the ABI's one ambient
readable, so the stream was **predictable to anyone who could guess boot-relative time**. Fine for a
`HashMap` seed. Useless for a key, a token, or a challenge.

That is not a small caveat sitting in one file. It taints anything security-adjacent anywhere in the
tree, because a caller cannot tell a weak source from a strong one by looking at the call. And it
blocked SMB authentication outright: an NTLMv2 server challenge that an attacker can guess is a
challenge an attacker can precompute a response for, so the whole exchange stops proving anything.

The old file named its own replacement ("a virtio-rng driver feeding a capability-granted service is
future work and would replace this file, not patch it"), which is what this is.

## The shape

```text
   virtio-rng ──virtio (mmio or PCIe, §18)──► entropy ──an endpoint──► clients
    (a device)                                  │      (CALL, bytes in the reply)
                                                └── its DMA page: nobody else maps it
```

One process holds the device. Everything else holds **one endpoint that names no device**, and that
is the entire difference between "may obtain randomness" and "may reach the random-number
generator". A client cannot program the queue, cannot map the page the device writes into, and
cannot ask the device for anything the service did not ask on its behalf.

The service's whole authority is four things placed before it ran: the request endpoint (RECV), an
`Irq`, a `Virtio` transport, and a readiness endpoint. No initrd, no budget, no filesystem, no
network. A compromised entropy service is a machine whose random numbers an attacker chooses, which
is exactly as much damage as owning the entropy source should be worth, and it is the reason the
device is not sitting inside every program.

**It needed nothing new in the kernel.** No capability type, no syscall, no method number: an
`Endpoint` with WRITE is the whole grant, and `caps` already prints it. That is the third time this
pattern has paid (the framebuffer, the clock, now this), and it is worth noticing that a capability
system's answer to "add a new privilege" keeps being "hand out a smaller object you already have".

## Attenuation by operation, not by object

This is the fourth appearance of one idea and it deserves the name the roadmap gives it:

| milestone | the object | the attenuated power |
|---|---|---|
| 51 | the wall clock | an NTP client may **propose** a time, not set it |
| 51 (§43) | the clock page | read is a read-only mapping, set is a writable one |
| 56 (§44) | the virtio-rng | **obtain** bytes, without reaching the device |
| 56 (planned) | a password hash | **use** a credential, without reading it |

In each case the narrow power is a different object rather than a flag on the wide one, so the
narrowing survives delegation: a client that passes its endpoint on passes the small power, because
the small power is all it ever held.

## The bytes are the device's bytes

**No pool, no whitening, no mixing, no DRBG.** There is no cryptographic primitive in this tree yet,
and without a one-way function every transformation available is a reversible permutation: it would
change the bytes without adding an unpredictability an attacker could not undo, while making the
security claim harder to state. So the claim stays one sentence long, and it is auditable:

> These are the bytes the device produced.

What the service does keep is a **256-byte buffer**, and the distinction from a pool matters. Byte
*i* out is byte *i* in, unmodified, handed to exactly one client, and zeroed behind the cursor. It
is a cache for round trips, not an entropy transformation, and it turns thirty-two device requests
into one.

### Short reads

virtio-rng is allowed to return fewer bytes than the buffer holds, and the used ring's `len` is
where it says so. **QEMU's really does**, which is worth recording as a measurement rather than a
spec allowance the code defends against on principle: the first version of the service passed a
short buffer straight through to the client, and the milestone-56 test caught a five-byte reply to
an eight-byte request thirty draws in.

So the service gathers across the buffer boundary, and a count below what was asked means one thing
only: the device went dry part-way through. It never pads, never repeats a byte it has already
served, and never substitutes a pseudo-random stand-in. A device that produces nothing across four
attempts gets `NO_ENTROPY` back to the caller, because a caller who cannot be given randomness must
find out.

### Why the bytes ride in the reply and not in a shared page

[§10](../design/decisions/10-capability-microkernel.md) says bulk rides in a page and control rides in the message, and this contract
deliberately does not. A page shared with a client is a place the bytes **persist** and a second
party can read, and random bytes are the one payload whose entire value is that nobody else has seen
them. Registers and the client's own stack are a smaller footprint than a page both parties map.

The cost is real and is not hidden: one round trip per eight bytes, so a 32-byte key is four round
trips. `bench/` prices a round trip; nothing about this is free.

### One number that could not collide

The reply's first word is a byte count in `0..=8`. Every failure the kernel can return from a `CALL`
is one of its small negatives (-1..-8), which read as enormous `u64`s. So "there is no entropy
service" and "the service has no entropy" are distinguishable with no probe request and no ambiguity
to reason about. `fs_proto` could not manage that (its errno space collides with the kernel's, a
wart notes/std.md records), and a contract this new had no excuse to inherit the collision.

## What `std::random` does

Transparent, and split where std already splits it:

| std entry point | what std promises | granted | not granted |
|---|---|---|---|
| `std::random::SystemRng` / `fill_bytes` | "random data suitable for cryptographic purposes such as key generation" | the device's bytes | **panic**, naming the reason |
| `RandomState` / `hashmap_random_keys` | DoS resistance for a hash table | the device's bytes | the old counter-seeded splitmix64 |

A program that calls `SystemRng` gets real entropy the moment it is granted the capability, with no
cricker-specific API to learn. `fill_bytes` has no error channel, so the only loud refusal available
is a panic, which is [§43](../design/decisions/43-clock-authority.md)'s `SystemTime::now()` decision applied a second time: a
program that never asks is unaffected, and a program that asks gets told rather than quietly
stamping a key with something guessable.

`hashmap_random_keys` is the one place a fallback is right. A `HashMap` in a program nobody granted
entropy must still work, and std's own `unsupported` backend degrades that same function (to
allocation addresses) rather than failing, so a platform is permitted to. The splitmix64 stream
survives there, clearly labelled, and **nothing in the file lets it reach `fill_bytes`**. That
separation is the milestone's point: the caller that promises cryptographic strength refuses when it
cannot keep the promise, and the caller that promises nothing degrades and says so.

The grant is **slot 6** of the std slot convention (`sys/pal/cricker/rt.rs`), and it is an endpoint
with no mapping alongside it, unlike the clock, whose read authority *is* a page.

## Both transports, and a finding about interrupts

virtio-mmio and PCIe, one binary, chosen by the wiring ([§18](../design/decisions/18-pcie-transport.md)'s seam). The PCIe
instance sits behind the IOMMU and the test asserts it: the buffer this device writes is where the
machine's key material comes from, so an unconfined device writing it is the last thing you would
leave unchecked.

**The driver looks at the used ring before it blocks**, which is a change from the disk driver's
shape and a fact about the board rather than an optimisation. `pci::intx_irq` swizzles INTx by
device number modulo four; `sched::bind_irq` routes an intid to exactly one endpoint; the test leg
now attaches five PCI functions. There is no unshared line left, so a driver that blocked before
looking would be betting on owning its line, and losing that bet is a hang rather than a wrong
answer. The interrupt wait is still there and is what a genuinely asynchronous device gets. QEMU
completes a virtio request inside `NOTIFY`, so the fast path never blocks in practice.

## What is proven, and where

Four kernel tests, not arch-gated and not transport-gated, so aarch64 and riscv64 run literally the
same assertions over both buses (`kernel/src/user/entropy_tests.rs`):

- **512 bytes across a refill boundary, all 64 words distinct and none zero**, over mmio and over
  PCIe. A stuck device, a buffer served twice, and a driver reading a stale used ring all present
  exactly as a repeat, and a collision among 64 draws from a real source is a 2^-58 event.
- **Two devices do not agree.** The mmio and PCIe services draw disjoint bytes, which is what says
  they came from the devices rather than from something shared underneath.
- **The count in a reply is the truth about the reply**: a three-byte request writes three bytes and
  leaves the rest of the caller's buffer alone, an oversized one is clamped and answered rather than
  refused, and an unimplemented opcode is answered with nothing rather than killing the service.
- **A std program gets there**: `std_exerciser` draws two 32-byte values through `std::random` and asserts
  they differ (`entropy ok` in the pinned transcript), on both ISAs.

**And since 2026-08-03 the endpoint is load-bearing for something a machine cannot do without it.**
Milestone 57's write half made this service the thing that decides whether a disk can be partitioned
or formatted at all: a GPT partition and a RedoxFS volume each carry an identifier that must be
globally unique, and neither `crates/gpt` nor a `no_std` RedoxFS has any randomness of its own. So
`disk_partitioner` and `mkfs` hold this endpoint beside their disk, and **withholding it is what
the test does to prove the pair is necessary**: the same binary, the same disk, one capability fewer,
and a disk that afterwards still reads as unpartitioned. That is a stronger statement of "an
endpoint is the authority to obtain randomness" than a client that merely draws bytes and compares
them, because here the refusal is visible on the platter.

It is also the first client whose *correct* behaviour on `NO_ENTROPY` is to do nothing at all.
`std::random` has no way to fail, which notes/std.md records as a wart; these two do, and they take
it.

## Honest limits

- **Under QEMU the device is backed by the host's `/dev/urandom`.** That is what makes these bytes
  real, and it is a fact about the emulator rather than a property of the driver. On hardware the
  quality of the bytes is the quality of the board's generator, full stop, because nothing here
  conditions them.
- **No hardware TRNG yet.** The StarFive JH7110's TRNG is the candidate for the VisionFive 2 and
  **needs verifying** before it is relied on. Until it is, "cricker-os has a cryptographic random
  source" is a claim about QEMU.
- **No health test.** A device that started returning a constant would be passed straight through.
  The kernel tests would catch it, a running system would not. NIST SP 800-90B's repetition-count and
  adaptive-proportion tests are the cheap standard answer and are not implemented.
- **No rate limit and no quota.** A client holding the endpoint can drain the service as fast as it
  can `CALL`. Eight bytes per round trip is a cost, not a defence.
- **`init` does not endow the shell with entropy.** The std wiring and the milestone-56 tests do.
  Ambient entropy would be ambient authority, and the point of the grant is that a program's
  dependence on randomness is visible in what it holds. A shell that needs to hand entropy to a child
  is future work with no design problem in it.
- **No cryptography anywhere.** No hash, no cipher, no key derivation. Milestone 56's other half
  (vendoring RustCrypto) is a separate decision, and this lane deliberately did not pre-empt it.

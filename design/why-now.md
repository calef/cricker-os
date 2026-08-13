# Why now: the case for building this operating system

A position document. It assumes no knowledge of this project and answers three questions in order:
what is wrong with the operating systems we have, why the known fix has not been adopted in forty
years, and what changed recently enough to make the attempt worth making today.

The technical decisions behind it are DECISIONS §82 (the thesis), §14 (the project's direction) and
§10 (the capability microkernel). The honest status is the last section, and readers who want the
caveats before the argument should start there.

## 1. The problem is that authority is ambient

On every mainstream operating system, a program's authority comes from **who ran it** rather than
from **what it was handed**. A process inherits its user's entire reach: every file that user can
read, every socket that user can open, every other process that user can signal.

This is not an abstraction. It is why a build-time dependency in a package tree can read a
developer's private keys, why an archive extractor can write outside the directory it was pointed at,
and why a document viewer asked to render one file can enumerate the disk it lives on. None of those
are bugs in those programs. Each is the operating system's model working exactly as designed, and the
program merely doing something the design permitted and nobody expected.

The industry has understood this for a long time and has answered it by **adding fences after the
fact**: `chroot`, then jails, then containers, then seccomp, then mandatory access control, then
per-application sandboxes, then permission prompts. Each narrows some ambient authority for some
program. Each is opt in, separately configured, and enforced somewhere other than where the authority
is used. A program is confined when somebody remembered to confine it, which means the default is
still the dangerous one.

The alternative is not to narrow ambient authority but to **never grant it**. In a capability system a
program holds exactly what it was handed and can name nothing else. An extractor cannot traverse out
of its directory because there is no "out" it is able to express. The confinement is not a check the
program passes; it is the absence of a way to ask.

## 2. The answer has been known for forty years

Capability operating systems are not a new idea and have never been refuted. KeyKOS in the 1980s,
EROS and later Coyotos in the 1990s and 2000s, and seL4 today, which is formally verified, shipping,
and deployed in real safety-critical and security-critical systems.

So the interesting question is not whether the model works. It is why, given a working answer with
four decades of research behind it, the operating system you are reading this on does not use it.

## 3. Two experiments tell us what actually blocks it, and it is not the model

**FreeBSD's Capsicum** is the strongest existing counter-argument to building anything new. It adds
capability mode to a production operating system: a process calls `cap_enter()` and from then on holds
only the descriptors it already has. It works, it ships, and it sandboxes real software that people
run in anger. If capabilities can be retrofitted, no new system is needed.

The catch is documented by Capsicum's own authors in their experience reports: converting
applications is laborious, because **the surrounding API assumes ambient authority everywhere.**
`getaddrinfo` is the canonical example and far from the only one. Every converted program needs a
helper service, an audit, and a reorganisation into "acquire authority, then drop it." The cost is
per application, and it recurs for every application, forever, because the environment those programs
were written against has not changed.

**CloudABI** went the whole way. Ed Schouten built a POSIX-like runtime with no ambient authority at
all, where a process starts with exactly the descriptors it was given. It is the closest thing to a
pure capability system that has ever shipped as a general-purpose runtime.

**It is deprecated and effectively dead.** Not because the model failed. Because it needed software
recompiled and adapted against it, the ecosystem stayed small, and maintenance did not survive its
maintainer moving on.

Those two results, read together, are the most useful thing in this document. Capsicum shows the
model works on real software and that retrofitting it costs a permanent per-application tax. CloudABI
shows that doing it properly instead of retrofitting moves the entire cost into one place: **you need
the software rewritten, and nobody could afford that.**

The binding constraint on capability operating systems has never been the kernel. It has been the
ecosystem.

## 4. What changed

The cost of rewriting software fell by something like an order of magnitude, and it fell recently.

That is the whole argument for the timing. If the reason a capability operating system never
displaced the ambient-authority one was that somebody had to rewrite the world, and rewriting the
world stops being prohibitive, then the question that was settled by economics rather than by
engineering is open again.

This project is the first evidence for the claim, because it is itself built that way: many agents
working in parallel lanes, with one person reviewing architecture and outcomes rather than lines. The
numbers are in the final section. They are numbers about **writing new software to a new design**,
which is the easier half, and the honest limits of that evidence are stated there too.

It is worth being precise about what the argument is not. It is not that language models make
software correct, or that they remove the need for review, proofs and gates. Everything in this
repository that makes the method survive contact with reality is a mechanism that assumes the
opposite. The claim is narrower and it is about price: the thing that killed CloudABI was the cost of
porting an ecosystem, and that cost is now different enough to change the answer.

## 5. What we are building

A **capability microkernel**, **proven**, in **Rust**. Three parts doing three jobs that are easy to
run together and should not be.

**Capabilities remove ambient authority.** This is the thesis and everything else serves it. A process
holds explicit, unforgeable references to the objects it may use, it cannot widen them, and what it
was not given it cannot name.

**The microkernel makes the trusted core small enough to prove.** Almost everything a monolithic
system runs in the kernel runs here as an ordinary confined program: the filesystem, the network
stack, the display, the shell. What remains is small enough that machine-checked proofs about it are
tractable.

**Rust removes the memory-safety class by construction**, so the proof does not have to carry it.
This is the difference from seL4 and the reason a new system rather than a contribution to that one.
seL4's proof bears the entire safety burden because C gives it nothing; here the language eliminates
roughly the largest category of vulnerabilities before verification begins, and the proofs are spent
on the security-critical logic instead. A verified-Rust capability kernel running real workloads is a
position no shipping operating system currently occupies.

## 6. What we are demonstrating

Four claims, ordered by how well the evidence supports them today. The ordering is the point.

**First, that a capability core can be machine-checked in Rust.** 110 proof harnesses run over the
capability logic, proving properties for every input rather than for sampled cases: that deriving a
capability never widens its rights, that userspace cannot forge one. These run in CI on every change.
This is demonstrated.

**Second, that it can be a complete system rather than a kernel demo.** The tree boots on two
architectures at parity, aarch64 and riscv64, and runs a shell, a filesystem, a network stack, a
compositor and a windowing scene, an NTP client, a measured boot chain and about fifty user programs,
all as confined userspace components. Where a monolith would have put a filesystem in the kernel, this
one has a program you can kill. This is demonstrated.

**Third, that a system of this size can be built this way at all.** This is the method claim, and it
is partly demonstrated: the system exists and its gates are real. What it does not yet show is that
the same approach ports somebody else's software, which is a different problem whose difficulty is
semantic compatibility rather than speed.

**Fourth, that software can run under narrow authority and still be useful.** This is the claim that
decides whether the thesis holds, and **it is not yet demonstrated.** No third-party application runs
here today. The measurements that would settle it are how narrow a grant a real ported program needs,
and whether it stays narrow once porting is cheap.

That fourth item is where an honest reader should apply pressure, and it is why the roadmap's next
real work is a port rather than another kernel feature.

## 7. What would prove us wrong

- **If ported programs turn out to be useful only under grants wide enough to be ambient in
  practice**, the model has lost on the axis that matters, and adding fences to existing systems was
  the right call all along.
- **If the porting economics do not hold outside new code**, then "now" is the wrong answer even if
  the model is right, and this is CloudABI again with better tooling.
- **If the proofs do not scale beyond the capability core**, then "proven" is decoration and what
  remains is an ordinary microkernel with unusually good hygiene.
- **If cheap porting produces ports that reconstruct ambient authority inside the capability system**,
  the box holds nothing. This is the failure we consider most likely, because it is the one that looks
  like success while it is happening.

## 8. Where this actually is

Measured from the merged tree on 2026-08-13:

| | |
|---|---|
| milestones built | 63 of 120 |
| crates | 44 |
| user programs | 52 |
| Kani proof harnesses | 110 |
| recorded design decisions | 83 |
| lines of Rust | about 126,000 |
| architectures at parity | aarch64, riscv64 |

Started 2026-07-12. Every number above is size and rate rather than quality: "built" counts
milestones marked built, and a single audit of that record found nine of them misrecorded.

**What is not here.** No third-party application runs on this system yet. There is no released
distribution, no installer, no hardware bring-up beyond emulation for the primary targets, and no
users other than its author. The `std` support that a real Rust program needs is partial and its gaps
are catalogued rather than closed. A recent survey of fifty crates.io crates found thirty-five build
unchanged, which is encouraging, and the fifteen that failed cluster in exactly the places that are
hardest to fix.

**And a caution we take seriously.** This project is a **demonstrator**, not a product. CloudABI is
what happens when a pure capability runtime is judged as a product before its ecosystem exists, and
the temptation to make product claims for a demonstrator is the specific mistake we are trying not to
repeat. The end state described in §82, replacing the ambient-authority ecosystem rather than
confining it, is a destination and an argument. It is not a description of what runs today, and any
sentence in this document that reads as though it were should be treated as a defect in the document.

## Reading further

- `design/decisions/82-ambient-authority-and-the-rewrite.md`, the thesis and its falsification
  conditions.
- `design/decisions/14-project-direction.md`, the technical direction and why verified-in-Rust is the
  differentiator.
- `design/capsicum-and-the-retrofit-question.md`, the strongest argument against building this, taken
  seriously and at length, including what Capsicum does better than us.
- `notes/verification.md`, what the proofs cover and what they do not.

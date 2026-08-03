# Security policy

cricker-os is a capability microkernel built as a **demonstrator** (DECISIONS.md §14): a small
machine-checked core that confines unverified workloads. Nobody runs it in production, including us.
It boots under QEMU and under HVF on an Apple Silicon laptop, and it has never run on physical
hardware.

That shapes what a security report means here. A bug that lets a confined process escape is
interesting because escaping is the exact thing the project claims cannot happen. A bug that says
"this OS is not ready to face the internet" is not a report; it is the premise.

It is also a one-person research project, so the response you get is a real person reading your
report between other things, not a triage queue. Both halves of that are honest below.

## Reporting

**Use GitHub's private vulnerability reporting**: the [Security
tab](https://github.com/calef/cricker-os/security) of this repository, "Report a vulnerability".
That opens a private advisory only you and the maintainer can see, with a place to attach a fix.

If that is unavailable, email **chris@crickertech.com** with `cricker-os security` in the subject.
There is no PGP key; if you need one before you will send details, say so in a first message and we
will sort out a channel.

**Please do not open a public issue for something you believe is exploitable.** Everything else
belongs in a public issue, and most things are everything else.

### What makes a report actionable

- The **commit sha** you were looking at. `main` moves.
- Which **architecture** (aarch64 or riscv64) and how it was run (`script/test`, `script/console`,
  `--hvf`). Several boundaries differ between the two ISAs, and one working and the other not is
  itself a finding (DECISIONS §19).
- Which **boundary** you believe is crossed, in the terms below. "A process read another process's
  memory" is a different claim from "a process panicked the kernel", and both are welcome.
- A **reproduction**, ideally as a test in the existing harness (a `#[test_case]` under
  `kernel/src/`, or a host-crate test). A failing test is the most useful thing you can send, and it
  is how a fix gets proven rather than asserted.

## In scope

The claim this project makes is confinement: a workload it did not write, running unverified, cannot
reach past the boundaries the kernel enforces. Anything that breaks one of these is in scope.

- **Capability forgery or widening.** Minting a capability that was never granted, widening rights
  across `derive`, `Send`, or delegation, naming another process's CSpace or endpoints, or reusing a
  generational name after revocation (DECISIONS §10, §13, §16; `crates/capability`).
- **MMU escape.** EL0 reading, writing, or executing kernel memory or another address space;
  breaking W^X; a stale TLB entry exposing a dead owner's data; anything reachable from userspace
  that maps physical memory outside the process's own untyped budget (`crates/paging`,
  notes/mmu.md).
- **DMA escape.** A device programmed by a userspace driver to read or write memory outside the
  grant it was given, through the software descriptor validator or past the IOMMU domain (DECISIONS
  §20, §23, §30; `crates/dma_validator`, notes/iommu.md).
- **IPC.** Anything that lets a message reach an endpoint the sender cannot name, a reply
  capability be used twice or by the wrong thread, or a server be confused about which client it is
  answering (DECISIONS §12, §26; `crates/ipc`).
- **The syscall surface.** Any `svc`/`ecall` argument, from EL0, that panics the kernel, corrupts
  kernel state, leaks kernel memory, or costs unbounded kernel time. The surface is deliberately
  narrow and every method is meant to validate its own inputs (DECISIONS §4 rule 3, §16).
- **Time-of-check to time-of-use on shared pages.** Every service contract moves bulk data through a
  page shared with the client, so a length or offset validated from a request word and then re-read
  from the page is a live double-fetch. This is a known lens rather than a known bug, and it is the
  one we would most like a second pair of eyes on (roadmap milestone 43).
- **The foreign-language and vendored seams.** The C component holds no capabilities and makes no
  syscalls; the vendored RedoxFS engine runs as a confined EL0 server. A way for either to reach
  authority it was not given is exactly the claim under test (DECISIONS §27, §31; notes/c-seam.md,
  notes/redoxfs-audit.md).
- **The boot trust root.** Anything that lets an unmeasured or altered init run as though it were
  measured (`crates/measured_boot`, notes/trusted-init.md).
- **The supply chain of this repository.** A dependency or vendored tree that is not what the
  manifest says it is. `script/supply-chain` is supposed to make that checkable; a way around it is
  a finding.

## Not in scope

None of these are dismissals of the underlying concern. They are things already known and written
down, which makes them roadmap items rather than reports.

- **"It is not production-ready."** Correct. There is no user authentication, no ASLR, no secure
  boot chain to firmware, no network security of any kind, and the shell is a demo. See
  notes/why-not-general-purpose.md.
- **A hardening feature that is on the roadmap.** design/roadmap/README.md is the list of what is missing
  and in what order. A missing feature that appears there is a roadmap item; a *defence that is
  claimed to exist and does not work* is a vulnerability, and that distinction is the whole test.
- **Anything that requires already being init.** init is privileged and unverified, and DECISIONS
  §14 says so in the thesis itself. The kernel confines it, and a compromised init cannot break the
  kernel or escape confinement, but init's authority over the processes it builds is by design.
- **QEMU or HVF escapes.** Report those to QEMU or to Apple. A guest breaking out of the emulator is
  not this kernel's boundary.
- **Denial of service by a process against itself**, or a process exhausting its own untyped budget.
  That budget is the mechanism, not a bug. A process exhausting a *kernel* resource, or another
  process's, is in scope.
- **Findings in upstream code we vendor**, unless our configuration is what makes them exploitable.
  Send those upstream too; we carry redoxfs at a pinned version (vendor/README.md) and would rather
  the fix land where everyone gets it. Tell us as well, so the pin can move.
- **Reports produced by a scanner with no analysis attached.** DECISIONS §35 is the standing policy:
  every finding gets a disposition and a dismissal is a written argument. That cuts both ways, and a
  list of tool output with no reasoning is not something either of us can disposition.

## What to expect

- **Acknowledgement within about a week.** This is a side project; there is no on-call.
- **No fixed remediation timeline, and no bounty.** What there is instead: a real answer about
  whether the boundary you found is one the project claims to hold, and if it is, a fix with a test
  that would have caught it, plus a `DECISIONS.md` or `notes/` entry recording what was wrong. That
  is how every previous security finding here was handled (notes/security.md, notes/arch-audit.md).
- **Credit however you want it**, in the fix commit and in the note. Say if you would rather not be
  named.
- **Coordinated disclosure, preferred and not demanded.** If you want to publish, publishing after
  90 days is fine by us whether or not there is a fix; tell us the date and we will not argue. If
  the finding is in vendored upstream code, upstream's timeline should win over ours.

## Supported versions

`main`, and only `main`. There are no releases and no version branches, so there is nothing to
backport to. If a fix matters to you, it is a commit on `main`.

## What has already been looked at

Two adversarial reviews are on the record, and reading them first will save you time:

- **notes/security.md**: a four-part review after milestone 11, with the threat model, what held
  up, and four real bugs (a crafted ELF that could panic the kernel, a spawn flood that could, a
  wasted-budget path, and documentation describing defences that had been deleted).
- **notes/arch-audit.md**: a pass over the assembly and architecture layer that found three:
  an `eret`/`sret` privilege-escalation staging race, a stale `tp` corrupting cross-hart per-CPU
  data on RISC-V, and a lock-free read-modify-write in the PLIC driver.

The machine-checked half is `script/verify` (Kani harnesses over the capability model, IPC, the MMU
invariants, the DMA validator). notes/verification.md states what each proof covers and, more
usefully here, what it does not: a proof holds inside chosen bounds, and DECISIONS §35 spells out
one gap the tools cannot close.

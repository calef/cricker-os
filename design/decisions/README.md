# nife: Architecture Decisions

*Name: ratified (§75 covers this directory). `decisions` names what the files are, one each, and the tree cites them as `§N` regardless of the directory's name; the plural is deliberate, since a singular would read as one decision about design rather than a place where decisions live.*

Decisions made 2026-07-12, before any code was written. Each entry records what we
chose, what we rejected, and why. Revisit these deliberately, not accidentally.

## How this directory works

One decision, one file, named `NN-slug.md`. The number is the identity: `§14` is
`14-project-direction.md` and nothing else, and the 2,000-odd `§N` citations spread across the
kernel, the crates, the notes and the roadmap all resolve here. GitHub renders this README as the
directory index, so browsing to `design/decisions/` shows the table below.

This was one 5,320-line file until milestone 114. Splitting it does the same three things the
roadmap split (milestone 76) did one directory over:

- **A number cannot be claimed twice by accident.** Two lanes both wrote `## 30.` into the single
  file on 2026-07-30 and it merged clean, because both sides' prose survived. As two files claiming
  one number it is a gate failure, which is what `script/decisions --check` now reports.
- **Text cannot land under the wrong heading.** §67 was appended into the middle of §61's BUGS
  section on 2026-08-03, because the insertion anchored on the string `## Reading` and the first
  occurrence of that string in the file was a wrapped prose line inside §61. Every gate reported
  clean for a day. `cat >>` into `67-second-stream.md` can only add text to §67.
- **A status flip stops being a conflict.** Marking one decision superseded used to edit the file
  every other lane was also editing.

Do not renumber. A number that moves breaks citations that no gate can see are wrong, because a
well-formed citation to the wrong section still resolves. Milestone 97 is where that check gets
built, and this directory is what makes it cheap: once a decision is a file with a title, a
citation's parenthetical name can be compared against that title.

## Status

Every decision carries a status on the line under its title, and the table below repeats it. The
two must agree, and `script/decisions --check` fails if they do not.

| Status | Means |
|---|---|
| `PROPOSED` | Raised, not yet decided. Options and a recommendation are in the file; nothing is built on it, and nothing should cite it as settled. Waiting on calef. |
| `DECIDED` | It holds as written. |
| `AMENDED` | It holds, but part of it was revised or overtaken by later work. The status line names what changed, and the amendment is in the file or in the decision it names. |
| `SUPERSEDED BY N` | A later decision replaces it. Kept, never deleted, because the reasoning is the record: §8's deferral was correct and §10 is what it deferred to. |

`AMENDED` is the token that pays for the vocabulary. Eleven decisions carry a revision that a reader
of the opening paragraph would otherwise miss, and §26 is the sharpest: its first line still says
"not yet built" while three blocks below it record milestone 22 building it. Nothing flagged that,
because a decision had no status to contradict.

**A decision waiting on calef is `PROPOSED`, and it lives here rather than in a queue of its own.**
`design/open-decisions.md` was that queue for one day, and it existed for a good reason: a decision
that lives only in a conversation's scrollback is in the medium milestone 94 was written to abolish.
But a proposal is the same object one lifecycle step before `DECIDED`, with the same shape (what is
being decided, the options, the recommendation and its reason, what is blocked until it is
answered). So it gets a file and a number when it is raised, and answering it flips a status in
place instead of moving text between two systems. Numbers 68 to 73 are the six that queue held.

## The decisions

| # | Status | Decision |
|---|---|---|
| 1 | DECIDED | [Target architecture: aarch64](01-target-architecture.md) |
| 2 | DECIDED | [Primary target: QEMU `virt`, Raspberry Pi as a later port](02-qemu-virt-primary.md) |
| 3 | DECIDED | [Use the crate ecosystem](03-crate-ecosystem.md) |
| 4 | DECIDED | [Kernel shape: monolithic, deferred, with two cheap rules](04-kernel-shape.md) |
| 5 | AMENDED | [Execution model: preemptive threads with real stacks](05-preemptive-threads.md) |
| 6 | SUPERSEDED BY 11 | [SMP: single-core, refactor when it hurts](06-single-core-first.md) |
| 7 | DECIDED | [Testing: QEMU harness + host-testable crates, from commit one](07-testing-harness.md) |
| 8 | SUPERSEDED BY 10 | [Process model / syscall ABI: DEFERRED to a hard decision point](08-process-model-deferred.md) |
| 9 | DECIDED | [Locking: IrqSafeMutex, plus a discipline](09-irq-safe-locking.md) |
| 10 | DECIDED | [Process model: capability-based, microkernel. Untyped memory deferred.](10-capability-microkernel.md) |
| 11 | AMENDED | [SMP: per-CPU run queues, message-based migration. §6, reopened.](11-per-cpu-run-queues.md) |
| 12 | DECIDED | [Call/Reply IPC: a one-shot reply capability](12-call-reply-ipc.md) |
| 13 | DECIDED | [Capability revocation and untyped reclamation (frames)](13-frame-revocation.md) |
| 14 | AMENDED | [The project's direction: a verified-Rust capability microkernel that runs real workloads](14-project-direction.md) |
| 15 | DECIDED | [The native ABI: formalize the convention, defer the BootInfo (milestone 19e)](15-native-abi.md) |
| 16 | AMENDED | [Object revocation: reclaim the objects a process built (extends §13)](16-object-revocation.md) |
| 17 | DECIDED | [The second architecture: RISC-V, and the page-table format trait](17-riscv-second-architecture.md) |
| 18 | DECIDED | [The PCIe transport: one driver, two buses, the seam in the kernel](18-pcie-transport.md) |
| 19 | DECIDED | [Architectural parity is a tenet; the targets are aarch64, riscv64, and x86_64](19-architectural-parity.md) |
| 20 | DECIDED | [IOMMU-backed DMA isolation: one seam, two arch drivers (milestone 16b)](20-iommu-dma-isolation.md) |
| 21 | AMENDED | [The terminal is a userspace component, and the kernel is out of the shell business (milestone 28)](21-terminal-in-userspace.md) |
| 22 | AMENDED | [Rust `std` on the native ABI, the Hermit way (milestone 27)](22-rust-std-on-the-native-abi.md) |
| 23 | DECIDED | [Multi-queue DMA confinement: the validator's second direction (milestone 30)](23-multi-queue-dma-confinement.md) |
| 24 | AMENDED | [Interrupting the foreground process: two-tier, shell-held, no new kernel surface](24-interrupting-the-foreground.md) |
| 25 | DECIDED | [Socket identity: a socket id in phase one, minted endpoints as the tracked later step](25-socket-identity.md) |
| 26 | AMENDED | [The fault endpoint: thread death becomes a message a supervisor holds](26-fault-endpoint.md) |
| 27 | AMENDED | [The filesystem service: a capability-shaped contract over a component we did not write (milestone 32 phase 2)](27-filesystem-service.md) |
| 28 | AMENDED | [SMP placement: two random choices at spawn, message-shaped stealing, local wakes](28-smp-placement.md) |
| 29 | DECIDED | [The framebuffer is a bigger grant, not an exemption (milestone 29, the display ladder's rung one)](29-framebuffer-grant.md) |
| 30 | DECIDED | [The DMA boundary is proved for descriptors, and the proof says where it stops (milestone 35)](30-dma-boundary-proof.md) |
| 31 | DECIDED | [The foreign-language seam: C holds no capabilities and makes no syscalls (milestone 36)](31-foreign-language-seam.md) |
| 32 | DECIDED | [A supervisor may collect a corpse without being able to build one](32-reap-without-build.md) |
| 33 | DECIDED | [The compositor's authority is memory, not messages (milestone 33, the display ladder's rung two)](33-compositor-authority.md) |
| 34 | AMENDED | [RedoxFS is the primary filesystem, on three conditions](34-redoxfs-primary.md) |
| 35 | DECIDED | [What a scanner is for here, and how its findings get dispositioned](35-scanner-findings.md) |
| 36 | DECIDED | [The repository is part of the TCB (milestones 44 and 42)](36-repository-in-the-tcb.md) |
| 37 | DECIDED | [Text is a value three witnesses compute, not a screenshot (milestone 29's remaining increment)](37-text-as-a-value.md) |
| 38 | DECIDED | [A suppression is scoped to an item and carries a reason, or it does not ship (milestone 41)](38-scoped-suppressions.md) |
| 39 | DECIDED | [A component is named for what it is, and nothing is named for a daemon](39-component-names.md) |
| 40 | DECIDED | [A supervisor's death is its subtree's death; there is no reaper of last resort](40-no-reaper-of-last-resort.md) |
| 41 | DECIDED | [The endpoint is the broker, and a device is revoked by taking it back (milestone 23)](41-endpoint-as-broker.md) |
| 42 | DECIDED | [A filesystem declares what it offers and must be truthful; it is not required to be capable](42-truthful-filesystem.md) |
| 43 | DECIDED | [Reading the clock is a page, setting it is a page you may write, proposing is an endpoint](43-clock-authority.md) |
| 44 | DECIDED | [Entropy is a capability, `std::random` improves transparently, and the refusal is loud](44-entropy-capability.md) |
| 45 | DECIDED | [A nife partition is `EC5CC08B-D749-4434-AC38-A274C50385BA`, and that never changes](45-partition-guid.md) |
| 46 | AMENDED | [Thin primitives or whole subsystems; we write everything in between](46-dependency-rule.md) |
| 47 | DECIDED | [A directory capability carries six rights, and a child can never exceed its parent](47-directory-rights.md) |
| 48 | DECIDED | [Navigation is the shell rebinding what it holds, and every shell has its own root](48-shell-navigation.md) |
| 49 | DECIDED | [Removal is a directory operation, and `-r` widens the grant rather than setting a flag](49-removal-and-recursion.md) |
| 50 | DECIDED | [Namespace composition (`bind`), not stored paths](50-namespace-composition.md) |
| 51 | DECIDED | [The sink protocol: a writer must not be able to tell what it is writing to](51-sink-protocol.md) |
| 52 | DECIDED | [A set of names is a namespace, and that is how a glob is granted](52-nameset-glob-grant.md) |
| 53 | DECIDED | [Parity is a matrix, not a pair](53-parity-matrix.md) |
| 54 | DECIDED | [Recovering a backup includes its metadata, and formatting a disk needs entropy](54-host-recovery.md) |
| 55 | DECIDED | [The file behind a `>` is the shell itself, because one page cannot serve two clients](55-shell-holds-the-redirect.md) |
| 56 | DECIDED | [The filesystem contract describes its own verbs, so a caretaker is written once](56-verb-table.md) |
| 57 | DECIDED | [Extended attributes forward through the caretakers, and the server enforces direction](57-xattr-forwarding.md) |
| 58 | DECIDED | [A wider archive name, and the one format change that had to bump the magic](58-wider-archive-name.md) |
| 59 | DECIDED | [Append is an open mode, so `>>` costs a character and a flag](59-append-mode.md) |
| 60 | DECIDED | [Fuzzing complements the proofs, and the parsers are exactly where it wins](60-fuzzing-the-parsers.md) |
| 61 | DECIDED | [A lint is adopted on evidence from this tree, not on its description](61-lints-on-evidence.md) |
| 62 | DECIDED | [Nothing of yours lives below the live stack pointer](62-below-the-stack-pointer.md) |
| 63 | DECIDED | [The line between a program and its crate is "does this need a capability"](63-program-versus-crate.md) |
| 64 | DECIDED | [A per-file coverage number counts where tests are written, not what they reach](64-per-file-coverage.md) |
| 65 | DECIDED | [A refusal that is not passive cannot be used as a question](65-active-refusal.md) |
| 66 | DECIDED | [A refusal is a non-zero status, and not the same one an error gets](66-refusal-status.md) |
| 67 | DECIDED | [A program's second stream is a declaration, not a number](67-second-stream.md) |
| 68 | DECIDED | [`BootEndowment::unused` wants a truer name: it is `for_test_roles`](68-boot-endowment-name.md) |
| 69 | DECIDED | [`Endow` is a verb, and names the same idea as `Endowment`: it is `ChildEndowment`](69-endow-versus-endowment.md) |
| 70 | DECIDED | [BUILT measures the end-state, and a false premise is rewritten in as the finding](70-built-when-nothing-to-fix.md) |
| 71 | DECIDED | [A limitation is promoted when it stops being a fact and becomes a plan](71-recorded-limitations.md) |
| 72 | DECIDED | [`time` needs no clock: duration is ambient, wall-clock identity is authority](72-time-command-clock.md) |
| 73 | DECIDED | [Milestone 44's ten admin minutes, which only calef can spend](73-repository-admin-steps.md) |
| 74 | DECIDED | [Audits run on change, not on the calendar: events first, then a count](74-audit-cadence.md) |
| 75 | DECIDED | [Directories under `design/` and `notes/` carry provenance in their own README](75-naming-covers-directories.md) |
| 76 | PROPOSED | [What catches a milestone status that is wrong in both places?](76-roadmap-status-versus-tree.md) |
| 77 | DECIDED | [The branch-prefix list now describes the tree](77-branch-prefixes.md) |
| 78 | PROPOSED | [Signed commits: worth doing, and not as a side effect](78-signed-commits.md) |
| 79 | DECIDED | [Holding password-equivalent material, and what a session key release means](79-password-equivalent-material.md) |
| 80 | DECIDED | [One build for the kernel and everything that runs on it](80-one-build-for-everything.md) |
| 81 | DECIDED | [A dependency stays upgradable; we suppress churn, never the upgrade](81-dependency-upgrades.md) |
| 82 | DECIDED | [Ambient authority is the problem; replacing the ecosystem, not confining it, is the end state](82-ambient-authority-and-the-rewrite.md) |
| 83 | DECIDED | [When the same thing exists in C and in Rust, take the Rust one](83-rust-over-c-implementations.md) |
| 84 | DECIDED | [How we port: prefer software that has already dropped ambient authority](84-how-we-port.md) |
| 85 | DECIDED | [What we port is evidence and must not be ours; what we ship is product and must be](85-evidence-and-product.md) |
| 86 | PROPOSED | [Whether an NVMe driver can leave the kernel, and what capability would let it](86-el0-nvme-driver.md) |
| 87 | DECIDED | [MIT OR Apache-2.0, and why the GPL's lesson does not transfer](87-permissive-license.md) |
| 88 | PROPOSED | [`needs-architect` as a required check, rather than as a script's restraint](88-needs-architect-as-a-check.md) |
| 89 | DECIDED | [`provisional` becomes the fourth provenance state](89-provisional-versus-unrecorded.md) |
| 90 | DECIDED | [The claim is a draft pull request; the status flip is a gate](90-claiming-and-closing.md) |
| 91 | DECIDED | [A region's endpoints are swept before its refusal, not after](91-endpoints-before-the-refusal.md) |
| 92 | DECIDED | [A caretaker is supervised by the client it serves](92-caretaker-lifetime.md) |
| 93 | DECIDED | [The filesystem wire protocol is ours, and 9P is an adapter at the edge](93-filesystem-wire-protocol.md) |
| 94 | DECIDED | [What may live in a userspace library, and what must be per-binary](94-what-may-live-in-a-library.md) |
| 95 | PROPOSED | [A hand-written IPC fastpath, and whether it can stay proven](95-a-proven-ipc-fastpath.md) |
| 96 | PROPOSED | [Process kernel or event kernel, and how to decide it](96-process-kernel-or-event-kernel.md) |
| 97 | PROPOSED | [Six gates run on every pull request and none of them can stop one](97-advisory-checks.md) |
| 98 | PROPOSED | [`OPENDIR` cannot be asked to attenuate, so a held directory probes for its own rights](98-opendir-cannot-attenuate.md) |
| 99 | DECIDED | [Where Apple's metadata lands: stream or sidecar](99-apple-metadata-at-rest.md) |
| 100 | AMENDED | [The terminal font](100-the-terminal-font.md) |
| 101 | DECIDED | [Notification objects: async multiplexing without wait-any](101-notification-objects.md) |
| 102 | DECIDED | [A Frame names a run of pages](102-frame-names-a-run.md) |
| 104 | DECIDED | [The rich-text font is DejaVu Sans Mono, and the palette is Solarized](104-the-font-and-the-palette.md) |
| 103 | SUPERSEDED BY 102 | [What a `Frame` names](103-what-a-frame-names.md) |
| 105 | DECIDED | [`std::thread::spawn` stays declined, until a customer needs it](105-thread-spawn-decline-for-now.md) |

Two blocks that lived among the decisions are not decisions and moved out with the split, the same
way milestone 76 moved four essays out of the roadmap: [the open design
ideas](../open-design-ideas.md), which are proposals parked against a trigger rather than choices
made, and [the original eleven-milestone plan](../original-milestone-plan.md), which the roadmap
backfilled into `design/roadmap/` and which is kept here as the record of what was planned before
anything was built.

## Reading

- **The seL4 manual**, and Klein et al., *seL4: Formal Verification of an OS Kernel* (SOSP'09)
- **Liedtke**, *On µ-Kernel Construction* (SOSP'95): why Mach was slow and why that was not a law
- **xv6 book** (MIT, ~100pp) for how a real Unix-shaped kernel is structured. Read it as the
  road not taken (§10), not as a template.
- `rust-raspberrypi-OS-tutorials` for the aarch64-specific mechanics
- OSDev wiki as a reference, not a tutorial
- *Operating Systems: Three Easy Pieces* for the theory

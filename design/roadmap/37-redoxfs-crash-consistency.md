# 37. Prove RedoxFS's crash consistency (DECISIONS §34, condition 1)

**Status: BUILT.**

**Built 2026-07-30, both ISAs. §34's condition 1 is met, and the claim it earns is narrower and
sharper than the one it replaces** (DECISIONS §34's amendment carries the full statement).

What is measured, on the host, exhaustively: 93 fault points across a seven-operation workload, each
one a power cut with the process gone and the recovery a fresh mount. Every one recovers a state that
**really existed**, the prefix never goes backwards as the cut advances, and at the last cut point
nothing is lost. The same sweep with the interrupted write **torn** at four offsets: 372 points, same
result. A separate sweep models a device that *lies* (acknowledges a write it never persists, then
carries on): 186 damages, 112 recovered, 74 refused at the mount or the read, and **zero silently
wrong**, which is the honest limit and the honest guarantee in one number.

The controls, which is what makes the rest mean anything: with the header ring's older generations
removed, **92 of the 93 fault points do not mount at all**; a commit torn at 2048 bytes fails
`Header::valid()` while the previous generation's slot stays valid and older. And the injector caught
a bug in the *harness* first, which is the best evidence it bites: nine fault points looked like
filesystems that never existed until it turned out `snapshot` was reading `EIO` as "the name is
absent".

On device, on its own disk on both ISAs: the FS server is killed one block write into its second
transaction, with that block torn in half by a real virtio write, and a **different FS-server
process** mounts what it left behind through the same block server and reads the file back whole.

**In brief.** Inject the failure a copy-on-write filesystem exists to survive, and measure whether it does: torn writes (a block partially written), dropped writes (a write the device acknowledged and did not persist), and a kill mid-transaction, then reopen with the same `cleanup: true` header-ring replay the FS server always mounts with, and assert the filesystem is consistent and every acknowledged write is either wholly present or wholly absent. The seam is `IpcDisk` and the block server, which sit between the engine and the device and can drop or truncate a write deliberately; the sans-IO core already runs on the host against a real image, so most of this is host-testable in milliseconds and only the device-level kill needs QEMU. Includes the negative control that makes the rest mean anything: the injector must be shown to actually corrupt something when the replay is disabled

**Why it matters.** **the condition that decides whether §34's label is earned.** Crash consistency is RedoxFS's central selling point and the reason it beat ext2, and we currently assert it on the strength of the upstream design description rather than any measurement. That is a claim of exactly the kind this project's rules forbid, and it is the first thing a skeptic asks a filesystem. Until it passes, the docs say "designed for crash consistency" and never "crash consistent". Note this is a gap in **our harness, not in RedoxFS**: no candidate engine's crash consistency is tested here, so switching engines would not address it

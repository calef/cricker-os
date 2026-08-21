# 144. QEMU's monitor screendump never lands in this sandbox

**Status: NOT-STARTED.**

**Gate: NONE.** Minted provisionally by calef on 2026-08-21, during milestone 16a's bench session;
the integrator should confirm the number at merge (143 was already claimed on an unmerged branch,
`roadmap/143-silicon-iommu`, so this may need renumbering; as of this writing 143 merged clean and
144 has no collision).

## What this is

The scanout-proving mechanism (notes/framebuffer-contract.md, "Proving the scanout, from the
host") drives QEMU's monitor over a unix socket (`NIFE_GPU_MON`) to `screendump` the guest's
framebuffer and compare it pixel-for-pixel against what the guest painted. It is not a display
gate: `-display none` is headless by design, and the monitor answers `screendump` even with no
display backend (verified against QEMU 11.0.2, the note says, and confirmed again on this
sandbox's own QEMU 11.0.2 by hand).

In this development sandbox, `cargo xtask test --arch riscv64` completes its full 279-test kernel
suite cleanly, but the referee (`ScanoutReferee` in `xtask/src/main.rs`) never gets a match:

```
scanout check (riscv64) FAILED: the compositor test passed, so the guest's witnesses agree about
the framebuffer, but QEMU's scanout never held the composed screen. Last mismatch: no screendump
was ever taken (did QEMU get a monitor?)
```

Same failure shape on all three GPU-referee checks (compositor, display-terminal, raw scanout
pattern) plus the two network-referee checks (`InboundProber`'s port-forward accept test, the
mDNS multicast test, the SMB session test): all six checks in this family fail the same way,
while every in-guest kernel test that doesn't depend on a host-side monitor or forwarded port
passes.

## What was ruled out at the bench (2026-08-21)

- **QEMU's monitor mechanism itself.** A hand-built `qemu-system-riscv64 -monitor
  unix:...,server,nowait -display none` bound the socket file and answered a connect; the
  banner (`QEMU 11.0.2 monitor - type 'help' for more information`) came back over the wire.
- **Unix socket creation in `/tmp` on this sandbox.** A plain Python `socket.bind()` to a
  `/tmp` path succeeded with no permission error.
- **The runner script wiring.** `scripts/qemu-runner-riscv64.sh` passes `-monitor
  unix:$NIFE_GPU_MON,server,nowait` exactly when `NIFE_GPU_MON` is set, which is the same
  mechanism `xtask`'s aarch64 leg and this riscv64 leg both use, and neither the socket path
  nor the flag differs from what the note describes as already proven.

None of that isolates the actual failure: the synthetic hand-test above used no real kernel and
no attached GPU device, so it does not reproduce the referee's actual conditions (a live guest,
under emulation, racing a 100ms poll loop against real boot and test time). The gap between
"the mechanism works in isolation" and "the mechanism connects during a real run" is exactly what
is unmeasured.

## What would settle it

1. **Isolate host load as a variable.** Re-run `cargo xtask test --arch riscv64` alone, with
   nothing else competing for CPU, and see whether the referee connects. The load-average
   instrumentation this tree already has (see `xtask/src/main.rs`'s host-load reporting, built
   for exactly this class of failure per notes/load-sensitive-assertions.md) should be read at
   the moment of failure rather than guessed.
2. **Add a diagnostic to the referee itself**: log every `UnixStream::connect` attempt's result
   (not just the aggregate "never taken"), so a failing run says whether QEMU never started the
   monitor at all, started it too late, or accepted connections that then went nowhere.
3. **Check whether this sandbox's QEMU build differs from a bare-metal one** in some way that
   only shows up under load or under whatever containment this sandbox applies to child
   processes (seccomp, network namespace, cgroup CPU throttling). `qemu-system-riscv64
   -display help` and a comparison against the CI runner's QEMU build would be the first checks.
4. **Confirm whether this is sandbox-specific or reproduces on the real dev machine** outside any
   containment. If it does not reproduce there, this milestone's scope narrows to "make the
   sandbox's CI-adjacent checks skip gracefully when the monitor is unreachable" rather than a
   fix to the mechanism itself.

## Why it matters

This is exactly the gap notes/framebuffer-contract.md's own opening line names: "the scanout" is
the one thing the in-guest test cannot prove, so the referee is the only witness that a wrong
pixel format or scanout rectangle would be caught at all. A CI environment where the referee
silently cannot connect is a green suite that has quietly lost that coverage, which is the kind
of gap this project's own BUGS-section discipline exists to name rather than hide.

## What this does NOT include

- **Fixing an actual scanout bug.** Nothing here suggests the pixels are wrong; the compositor,
  display-terminal, and pattern tests all pass on the guest side. This is purely about whether
  the host-side witness can reach the guest at all in this sandbox.
- **The network referees' underlying protocol correctness.** `InboundProber`, the multicast
  check, and the SMB check are proven elsewhere (this same tree's QEMU CI, presumably, since the
  notes describe them as already working); this milestone is about why they fail specifically
  in this bench sandbox.

# 34. GPU acceleration via virtio-gpu 3D (the display ladder's rung four)

**Status: NOT-STARTED.**

**Gate: DECISION.** The block prices it as a mountain and says it reopens the parked competitor
question, which the display ladder's governance note records as the architect's call. Rung four is
not a lane to launch before that answer.

**In brief.** The **Venus** path: Vulkan commands serialized over the virtio-gpu device, arriving on the §18 PCIe transport, so the guest gets real GPU acceleration without owning a hardware driver. Needs the 3D context and command-submission side of virtio-gpu that rung one deliberately left alone (rung one sets up no cursor queue and no 3D context, keeping the §23 two-queue ceiling untouched), the confinement story extended to command-carried backing addresses (DECISIONS §30's residual gap: those are the addresses the descriptor validator structurally cannot see, and today only an IOMMU stops them), and something to consume it, which is what would give `wgpu` a real target

**Why it matters.** **how every VM gets a GPU without a hardware driver**, and the honest ceiling on the display ladder: rung five (a bare-metal driver for the VisionFive 2's BXE-4-32 3D core) is struck as a Linux-scale multi-year effort that proves nothing this does not. A mountain, priced as such, and it reopens the parked competitor question the ladder's governance note names as the architect's call

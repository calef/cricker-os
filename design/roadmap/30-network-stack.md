# 30. The network stack as a confined component

**Status: BUILT.**

**In brief.** A userspace **virtio-net** driver behind the DMA confinement (extended to multi-queue: RX means the device writes INTO driver memory), and the TCP/IP stack itself (`smoltcp`) as a swappable userspace component with a capability-shaped socket contract; backs `std::net` for 27

**Why it matters.** **the canonical microkernel component**, the one people ask about first when a minimal kernel claims to stand next to Linux; and milestone 23's most convincing instance, hot-swapping a network stack under open connections. The reuse call is the plan's easiest: the thesis is the kernel confining the stack, not the stack

**Deliverable.** Two components and a contract. A userspace **virtio-net** driver, confined by
the same shadow-ring validator as the disk, which requires the one genuinely new kernel-adjacent
piece: **multi-queue transport support** (virtio-net needs RX and TX; the §18 seam and the
confinement are queue-0-only today, and RX is the direction where the *device writes into*
driver memory, so the validator grows a proved, tested second direction rather than an ad hoc
one). Above it, the TCP/IP stack itself as a swappable userspace component, `smoltcp` inside a
net server, speaking a capability-shaped socket contract: an endpoint plus shared frames per
connection, no ambient "the network"; a process holds a capability to a stack or it does not.
The contract is what `std::net`'s PAL (milestone 27) binds to, replacing its honest
`Unsupported`. Scope discipline: TCP, UDP, DHCP, done; no sockets-API mimicry beyond what the
PAL needs.

**Why.** A userspace network stack has been the defining microkernel component since Mach and
L4, and it is the first thing people ask about when a minimal kernel claims to stand next to
Linux. Milestone 23 gets its most convincing instance: live-replacing a network stack under
open connections is a far harder-nosed test of the component contract than a console swap. And
the multi-queue RX confinement is real DMA-isolation work that should land under the
validator's discipline, not be retrofitted when a NIC needs it on real hardware.

**Prior art and reuse.** The reuse call is the easiest in the plan: `smoltcp` (no_std,
kernel-agnostic, event-driven, proven across embedded Rust; Redox has shipped on it). Building
TCP by hand proves nothing thesis-relevant. Prior art to read before the contract is drawn:
seL4's net_stack componentization, Fuchsia's Netstack3 (Rust, capability-routed, the closest
cousin), and Plan 9's /net as the counter-design (per-connection filesystem, everything a
file). Testing is cheap: QEMU's user-mode networking NATs the guest with zero host setup.

**Sequencing.** After the PCIe transport (done); the multi-queue confinement is the
prerequisite piece and worth building first as its own tested step. Feeds 23 and 27.
**Effort: 3 lanes** (measured: multi-queue confinement, the driver and net_stack, then the socket contract).

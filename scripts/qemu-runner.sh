#!/bin/sh
#
# The QEMU runner. Cargo invokes this for `cargo run` and `cargo test`, appending the
# path to the ELF it just built.
#
# Why this exists rather than a plain `qemu-system-aarch64 ... -kernel` line in
# .cargo/config.toml: QEMU only follows the **Linux arm64 boot protocol** (and
# therefore only hands us a device tree pointer in x0) for a flat `Image`. Given an
# ELF, it takes a bare-metal path instead and populates no registers at all.
#
# So we strip the ELF down to a flat binary. The arm64 Image header lives at byte 0
# of it (kernel/src/arch/aarch64/image_header.s), which is what makes QEMU recognize
# the blob as a kernel.
#
# Tests boot through exactly the same path as `cargo xtask run` does, deliberately.
# A test harness that exercises a different boot path than the real thing is testing
# a fiction.
#
# See notes/boot-protocol.md.

set -e

ELF="$1"
shift

# llvm-objcopy ships with the `llvm-tools` rustup component, which rust-toolchain.toml
# pins. We resolve it out of the sysroot rather than expecting it on PATH, because
# `rust-objcopy` needs a separate `cargo install cargo-binutils` and we'd rather not
# add a setup step that fails confusingly six months from now.
SYSROOT="$(rustc --print sysroot)"
HOST="$(rustc -vV | awk '/^host:/{print $2}')"
OBJCOPY="$SYSROOT/lib/rustlib/$HOST/bin/llvm-objcopy"

if [ ! -x "$OBJCOPY" ]; then
    echo "qemu-runner: cannot find llvm-objcopy at $OBJCOPY" >&2
    echo "qemu-runner: is the llvm-tools component installed? (rust-toolchain.toml pins it)" >&2
    exit 1
fi

IMG="$ELF.img"
"$OBJCOPY" -O binary "$ELF" "$IMG"

# The userspace program rides in as an initrd, exactly the way Linux gets its initramfs: QEMU
# loads the file into RAM and writes the address into /chosen/linux,initrd-start in the device
# tree it generates. The kernel finds it there. Nothing about the binary is compiled into the
# kernel. See notes/elf.md and kernel/src/memory.rs.
INITRD=""
if [ -n "$CRICKER_INITRD" ] && [ -f "$CRICKER_INITRD" ]; then
    INITRD="-initrd $CRICKER_INITRD"
fi

# Attach the crickerfs image as a virtio-blk device. `if=none` + `-device virtio-blk-device`
# gives us a virtio-mmio block device on the `virt` machine, which is what the userspace driver
# probes for and reads. Without a disk, the kernel simply finds no block device and says so.
#
# A SET CRICKER_DISK naming a missing file is an error, not a silent no-op. The old behaviour
# (quietly booting diskless) had the kernel truthfully reporting "no block device", which reads
# like a machine fact when it is actually a build-order mistake; it very likely produced the
# false "riscv virt has no mmio disk" record in notes/riscv-parity-scope.md.
DISK=""
if [ -n "$CRICKER_DISK" ] && [ ! -f "$CRICKER_DISK" ]; then
    echo "qemu-runner: CRICKER_DISK=$CRICKER_DISK does not exist (run mkdisk first)" >&2
    exit 1
fi
if [ -n "$CRICKER_DISK" ]; then
    # force-legacy=false selects MODERN virtio-mmio (version 2), whose split register interface
    # (separate physical addresses for the descriptor table and the two rings) is the current
    # design and the one worth learning. Without it QEMU gives legacy (version 1), a different
    # and older queue layout.
    #
    # Both transports are attached WRITABLE (milestone 32: the write-capable block path), which
    # is why there are two image files rather than one attached twice: QEMU's image locking
    # refuses to open one file for two devices once either can write. mkdisk writes the sibling
    # alongside the main image with identical contents; missing sibling = stale build, fail loud
    # (the readonly-era silent-degradation lesson, see the CRICKER_DISK check above).
    #
    # iommu_platform=on is what puts the PCI disk BEHIND the SMMU (milestone 16b): the device then
    # emits IOVAs the SMMU translates through the domain the kernel built, and offers
    # VIRTIO_F_ACCESS_PLATFORM so the driver knows it. WITHOUT this flag QEMU's virtio device
    # bypasses the SMMU silently, and the confinement test (which asserts the hardware faults an
    # out-of-region DMA) fails loudly rather than passing on a fiction. The mmio disk (hd0) has no
    # IOMMU in front of it on this machine, so it takes no such flag.
    PCI_DISK="${CRICKER_DISK%.img}-pci.img"
    if [ ! -f "$PCI_DISK" ]; then
        echo "qemu-runner: $PCI_DISK does not exist (run mkdisk first; it writes both images)" >&2
        exit 1
    fi
    # The RedoxFS image (milestone 32 phase 2), the SECOND mmio block device. It is placed on the
    # command line BEFORE the crickerfs disk on purpose: QEMU's `virt` assigns virtio-mmio devices
    # to slots in REVERSE command-line order (the last -device gets the lowest-address slot), and
    # the kernel finds block devices by ascending slot. So the crickerfs disk must be the LAST mmio
    # device to keep slot 0 (find_block_device -> crickerfs, the phase-1 driver tests), which leaves
    # the RedoxFS disk at slot 1 (find_block_device_n(1) -> RedoxFS, the FS server's block server).
    # Getting this backwards silently hands the phase-1 tests the wrong disk; that is exactly the
    # bug this ordering fixes. Soft: present only when the test flow built it (cargo xtask test),
    # absent for a plain interactive boot, which just skips the FS-server test. Created host-side by
    # tools/redoxfs-host; the server only ever opens it.
    REDOXFS_DISK="${CRICKER_DISK%.img}-redoxfs.img"
    REDOXFS_MMIO=""
    if [ -f "$REDOXFS_DISK" ]; then
        REDOXFS_MMIO="-drive file=$REDOXFS_DISK,if=none,format=raw,id=hd2 -device virtio-blk-device,drive=hd2"
    fi
    DISK="-global virtio-mmio.force-legacy=false $REDOXFS_MMIO -drive file=$CRICKER_DISK,if=none,format=raw,id=hd0 -device virtio-blk-device,drive=hd0 -drive file=$PCI_DISK,if=none,format=raw,id=hd1 -device virtio-blk-pci,drive=hd1,disable-legacy=on,iommu_platform=on"
fi

# Attach a virtio-net NIC on QEMU user-mode (slirp) networking when CRICKER_NET is set (milestone
# 30). slirp NATs the guest with a built-in DHCP server (10.0.2.0/24, gateway 10.0.2.2) and DNS
# resolver (10.0.2.3), and needs no host setup, so the net tests run with zero privilege. Two NICs
# mirror the two disks: the mmio NIC (net0) has no IOMMU in front of it, the PCI NIC (net1) sits
# behind the SMMU (iommu_platform=on), the same hardware confinement the PCI disk gets. There is no
# image file to fail loud on here; the manufactured-fact hazard (CRICKER_NET set but no NIC
# enumerated) is caught by the net test, which asserts a NIC is present rather than skipping.
#
# guestfwd adds a deterministic TCP echo peer at 10.0.2.9:7777 inside slirp: a connection to it is
# piped to a fresh `/bin/cat`, so the TCP round-trip gate (connect, send, recv the echo, close) runs
# with zero host setup and nothing outlives QEMU. Verified against QEMU 11.0.2. Each slirp instance
# is its own network, so both NICs can use the same virtual address without conflict.
GUESTFWD="guestfwd=tcp:10.0.2.9:7777-cmd:/bin/cat"

# `tftp=` turns on slirp's OWN TFTP server, at the gateway (10.0.2.2:69), and that is what makes the
# gating UDP test deterministic and offline. The UDP test used to query 10.0.2.3:53, which is NOT a
# resolver: libslirp NATs anything sent there to the HOST's nameserver, so that test silently
# depended on the developer's DNS working at that instant and flaked whenever a query was dropped
# (measured ~2.5% per query against a real resolver). TFTP is served inside libslirp, so the request
# and its reply never leave the emulator. This is the UDP twin of the guestfwd echo peer above.
# The fixture's name and contents are fixed and must match user/src/netcli.rs (TFTP_NAME/TFTP_BODY);
# `printf` writes it with no trailing newline so the client can assert the bytes exactly.
TFTPDIR="$(dirname "$0")/../target/tftp"
mkdir -p "$TFTPDIR"
printf 'cricker-tftp!' > "$TFTPDIR/cricker"

NET=""
if [ -n "$CRICKER_NET" ]; then
    NET="-netdev user,id=net0,$GUESTFWD,tftp=$TFTPDIR -device virtio-net-device,netdev=net0 -netdev user,id=net1,$GUESTFWD,tftp=$TFTPDIR -device virtio-net-pci,netdev=net1,disable-legacy=on,iommu_platform=on"
fi

# Attach a virtio-gpu when CRICKER_GPU is set (milestone 29, the display ladder's rung one).
#
# PCIe only, and that is not a shortcut: there is no virtio-gpu on this machine's virtio-mmio bus in
# any configuration, so unlike the disk and the NIC there is no mmio twin to attach. The parity that
# matters is aarch64 virt and riscv virt, and both carry virtio-gpu-pci over the §18 transport.
#
# disable-legacy=on makes the function MODERN (device id 0x1050); iommu_platform=on puts it BEHIND
# the SMMU, so the pixel reads its RESOURCE_ATTACH_BACKING asks for are translated through the domain
# the kernel built. That flag is load-bearing here in a way it is not for the disk: a virtio-gpu's
# backing addresses ride in a device-level command payload, not in a descriptor, so the transport's
# shadow-ring validator never sees them and the IOMMU is the only thing that bounds them. Drop the
# flag and the GPU could name any physical address (see notes/framebuffer-contract.md).
#
# There is no image file to fail loud on, as with the NIC. The manufactured-fact hazard (CRICKER_GPU
# set but no GPU enumerated) is caught in the kernel test, which ASSERTS a GPU is present rather than
# skipping, and asserts the IOMMU is active while one is.
GPU=""
if [ -n "$CRICKER_GPU" ]; then
    GPU="-device virtio-gpu-pci,disable-legacy=on,iommu_platform=on"
fi

# Attach a virtio keyboard when CRICKER_KBD is set (milestone 29's input).
#
# PCIe here is a CHOICE, not a constraint: unlike the GPU, this machine does offer a
# virtio-keyboard-device on the virtio-mmio bus. The keyboard rides PCIe anyway so it lands in the
# same IOMMU domain the GPU does, and iommu_platform=on is what puts it there. A keyboard is the one
# device whose DMA you would least like unconfined: its buffers are where every keystroke lands.
#
# The keys themselves come from the HOST, over the QEMU monitor below (`sendkey`), because nothing in
# the guest can press one. QEMU drops key events until a driver sets DRIVER_OK, so xtask can send
# them from the start of the run with nothing to synchronize.
KBD=""
if [ -n "$CRICKER_KBD" ]; then
    KBD="-device virtio-keyboard-pci,disable-legacy=on,iommu_platform=on"
fi

# A QEMU monitor on a unix socket, when CRICKER_GPU_MON names one (milestone 29). This is how the
# **scanout** gets proven rather than only the framebuffer: `screendump` writes a PPM of the scanout
# and it works with -display none (verified against QEMU 11.0.2), so the host can see the pixels the
# guest cannot read back. xtask drives it while the suite runs (see gpu_shot); nothing else uses it,
# and without the variable QEMU gets no monitor at all, exactly as before.
#
# The path must stay under 104 bytes: that is the OS limit on a unix socket path, and a worktree
# checkout plus target/ gets close, which is why xtask puts the socket in /tmp and not in target/.
MON=""
if [ -n "$CRICKER_GPU_MON" ]; then
    MON="-monitor unix:$CRICKER_GPU_MON,server,nowait"
fi

# shellcheck disable=SC2086  # $INITRD, $DISK, $NET, $GPU and $KBD are deliberately word-split or empty
# CPU and accelerator.
#
# By default we run under TCG (QEMU translates every aarch64 instruction), with an emulated
# cortex-a72. That is deterministic and runs identically on any host, which is what the test
# harness wants.
#
# Set CRICKER_ACCEL=hvf to run under Apple's Hypervisor.framework instead: HVF puts the kernel on
# the real Apple Silicon core at guest EL1, using the hardware virtualization the chip already
# has. The coincidence that makes this a flag and not a port is that the host and the guest are the
# same ISA (aarch64). Two consequences:
#
#   - HVF runs the PHYSICAL core, so `-cpu host` is mandatory; you cannot ask for an emulated a72.
#   - gic-version is PINNED to 2, so a future QEMU default cannot swap in a GICv3 our driver does
#     not speak. QEMU emulates the GIC either way (Apple cores use their own AIC natively) and
#     injects interrupts through HVF, so the MMIO GICv2 driver keeps working.
if [ "$CRICKER_ACCEL" = "hvf" ]; then
    MACHINE="virt,accel=hvf,gic-version=2"
    CPU="host"
else
    # iommu=smmuv3 puts an SMMUv3 in front of the PCIe root complex (milestone 16b). It is on the
    # TCG path only, on purpose: the IOMMU is a correctness feature proven by the test suite (which
    # runs under TCG), not a benchmark axis, and smmuv3 emulation alongside HVF acceleration is the
    # fragile combination. The device tree then carries an `smmuv3@...` node (memory::smmu_region
    # finds it) and an identity iommu-map for the bus. A plain boot without a PCI disk still gets the
    # SMMU; it just has nothing to confine.
    MACHINE="virt,gic-version=2,iommu=smmuv3"
    CPU="cortex-a72"
fi

# Number of cores. Four by default, matching cpu::MAX_CPUS and the SMP tests (§11). QEMU brings
# up core 0 running; the kernel starts the rest itself via PSCI CPU_ON (see smp.rs).
SMP="${CRICKER_SMP:-4}"

exec qemu-system-aarch64 \
    -machine "$MACHINE" \
    -cpu "$CPU" \
    -smp "$SMP" \
    -display none \
    -serial stdio \
    -semihosting \
    -kernel "$IMG" \
    $INITRD \
    $DISK \
    $NET \
    $GPU \
    $KBD \
    $MON \
    "$@"

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
    echo "qemu-runner-aarch64: cannot find llvm-objcopy at $OBJCOPY" >&2
    echo "qemu-runner-aarch64: is the llvm-tools component installed? (rust-toolchain.toml pins it)" >&2
    exit 1
fi

IMG="$ELF.img"
"$OBJCOPY" -O binary "$ELF" "$IMG"

# The userspace program rides in as an initrd, exactly the way Linux gets its initramfs: QEMU
# loads the file into RAM and writes the address into /chosen/linux,initrd-start in the device
# tree it generates. The kernel finds it there. Nothing about the binary is compiled into the
# kernel. See notes/elf.md and kernel/src/memory.rs.
INITRD=""
if [ -n "$NIFE_INITRD" ] && [ -f "$NIFE_INITRD" ]; then
    INITRD="-initrd $NIFE_INITRD"
fi

# Attach the nifefs image as a virtio-blk device. `if=none` + `-device virtio-blk-device`
# gives us a virtio-mmio block device on the `virt` machine, which is what the userspace driver
# probes for and reads. Without a disk, the kernel simply finds no block device and says so.
#
# A SET NIFE_DISK naming a missing file is an error, not a silent no-op. The old behaviour
# (quietly booting diskless) had the kernel truthfully reporting "no block device", which reads
# like a machine fact when it is actually a build-order mistake; it very likely produced the
# false "riscv virt has no mmio disk" record in notes/riscv-parity-scope.md.
DISK=""
if [ -n "$NIFE_DISK" ] && [ ! -f "$NIFE_DISK" ]; then
    echo "qemu-runner-aarch64: NIFE_DISK=$NIFE_DISK does not exist (run mkdisk first)" >&2
    exit 1
fi
if [ -n "$NIFE_DISK" ]; then
    # force-legacy=false selects MODERN virtio-mmio (version 2), whose split register interface
    # (separate physical addresses for the descriptor table and the two rings) is the current
    # design and the one worth learning. Without it QEMU gives legacy (version 1), a different
    # and older queue layout.
    #
    # Both transports are attached WRITABLE (milestone 32: the write-capable block path), which
    # is why there are two image files rather than one attached twice: QEMU's image locking
    # refuses to open one file for two devices once either can write. mkdisk writes the sibling
    # alongside the main image with identical contents; missing sibling = stale build, fail loud
    # (the readonly-era silent-degradation lesson, see the NIFE_DISK check above).
    #
    # iommu_platform=on is what puts the PCI disk BEHIND the SMMU (milestone 16b): the device then
    # emits IOVAs the SMMU translates through the domain the kernel built, and offers
    # VIRTIO_F_ACCESS_PLATFORM so the driver knows it. WITHOUT this flag QEMU's virtio device
    # bypasses the SMMU silently, and the confinement test (which asserts the hardware faults an
    # out-of-region DMA) fails loudly rather than passing on a fiction. The mmio disk (hd0) has no
    # IOMMU in front of it on this machine, so it takes no such flag.
    PCI_DISK="${NIFE_DISK%.img}-pci.img"
    if [ ! -f "$PCI_DISK" ]; then
        echo "qemu-runner-aarch64: $PCI_DISK does not exist (run mkdisk first; it writes both images)" >&2
        exit 1
    fi
    # The RedoxFS image (milestone 32 phase 2), the SECOND mmio block device. It is placed on the
    # command line BEFORE the nifefs disk on purpose: QEMU's `virt` assigns virtio-mmio devices
    # to slots in REVERSE command-line order (the last -device gets the lowest-address slot), and
    # the kernel finds block devices by ascending slot. So the nifefs disk must be the LAST mmio
    # device to keep slot 0 (find_block_device -> nifefs, the phase-1 driver tests), which leaves
    # the RedoxFS disk at slot 1 (find_block_device_n(1) -> RedoxFS, the FS server's block server).
    # Getting this backwards silently hands the phase-1 tests the wrong disk; that is exactly the
    # bug this ordering fixes. Soft: present only when the test flow built it (cargo xtask test),
    # absent for a plain interactive boot, which just skips the FS-server test. Created host-side by
    # tools/redoxfs_host; the server only ever opens it.
    REDOXFS_DISK="${NIFE_DISK%.img}-redoxfs.img"
    REDOXFS_MMIO=""
    if [ -f "$REDOXFS_DISK" ]; then
        REDOXFS_MMIO="-drive file=$REDOXFS_DISK,if=none,format=raw,id=hd2 -device virtio-blk-device,drive=hd2"
    fi
    # The crash test's OWN RedoxFS image (milestone 37), the THIRD mmio block device, at slot 2. It
    # goes FIRST on the command line because the slot assignment is reverse of the command-line
    # order, so the three land at nifefs=0, redoxfs=1, crash=2, which is what
    # `find_block_device_n` counts. A dedicated disk is the point: the crash test deliberately leaves
    # a filesystem half-written, and doing that to the shared image would couple every other FS
    # test's result to whether this one ran first (DECISIONS §27's order-coupled gate). Soft, like
    # the others: present only when the test flow built it.
    CRASH_DISK="${NIFE_DISK%.img}-redoxfs-crash.img"
    CRASH_MMIO=""
    if [ -f "$CRASH_DISK" ]; then
        CRASH_MMIO="-drive file=$CRASH_DISK,if=none,format=raw,id=hd3 -device virtio-blk-device,drive=hd3"
    fi
    # The GPT-partitioned image (milestone 57), the FOURTH mmio block device, at slot 3. It goes
    # FIRST on the command line because the slot assignment is the reverse of command-line order, so
    # the four land at nifefs=0, redoxfs=1, crash=2, gpt=3, which is what `find_block_device_n`
    # counts and what `disk_service::GPT_DISK` asks for. This one carries no filesystem at all: the
    # bytes are the `sgdisk` fixture from crates/gpt/tests/fixtures, so the guest reads a partition
    # table written by gptfdisk rather than by us. Soft, like the others: present only when the test
    # flow built it (cargo xtask test).
    GPT_DISK_IMG="${NIFE_DISK%.img}-gpt.img"
    GPT_MMIO=""
    if [ -f "$GPT_DISK_IMG" ]; then
        GPT_MMIO="-drive file=$GPT_DISK_IMG,if=none,format=raw,id=hd4 -device virtio-blk-device,drive=hd4"
    fi
    # The blank image (milestone 57's write half), the FIFTH mmio block device, at slot 4. It goes
    # FIRST on the command line for the same reason as the others: slot assignment is the reverse of
    # command-line order, so the five land at nifefs=0, redoxfs=1, crash=2, gpt=3, blank=4, which
    # is what `find_block_device_n` counts and what `disk_service::BLANK_DISK` asks for. This one
    # arrives as 64 MiB of ZEROS: the guest writes the partition table and then the filesystem
    # inside it, and the post-run host check reads both back. Its own disk, regenerated every run,
    # because a test that partitions a disk must not touch an image another test reads. Soft, like
    # the others: present only when the test flow built it.
    BLANK_DISK_IMG="${NIFE_DISK%.img}-blank.img"
    BLANK_MMIO=""
    if [ -f "$BLANK_DISK_IMG" ]; then
        BLANK_MMIO="-drive file=$BLANK_DISK_IMG,if=none,format=raw,id=hd5 -device virtio-blk-device,drive=hd5"
    fi
    DISK="-global virtio-mmio.force-legacy=false $BLANK_MMIO $GPT_MMIO $CRASH_MMIO $REDOXFS_MMIO -drive file=$NIFE_DISK,if=none,format=raw,id=hd0 -device virtio-blk-device,drive=hd0 -drive file=$PCI_DISK,if=none,format=raw,id=hd1 -device virtio-blk-pci,drive=hd1,disable-legacy=on,iommu_platform=on"
fi

# Attach a virtio-net NIC on QEMU user-mode (slirp) networking when NIFE_NET is set (milestone
# 30). slirp NATs the guest with a built-in DHCP server (10.0.2.0/24, gateway 10.0.2.2) and DNS
# resolver (10.0.2.3), and needs no host setup, so the net tests run with zero privilege. Two NICs
# mirror the two disks: the mmio NIC (net0) has no IOMMU in front of it, the PCI NIC (net1) sits
# behind the SMMU (iommu_platform=on), the same hardware confinement the PCI disk gets. There is no
# image file to fail loud on here; the manufactured-fact hazard (NIFE_NET set but no NIC
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
# The fixture's name and contents are fixed and must match user/src/socket_test_client.rs (TFTP_NAME/TFTP_BODY);
# `printf` writes it with no trailing newline so the client can assert the bytes exactly.
TFTPDIR="$(dirname "$0")/../target/tftp"
mkdir -p "$TFTPDIR"
printf 'nife-tftp!' > "$TFTPDIR/nife"

# hostfwd is guestfwd's mirror and the inbound gate's whole mechanism (milestone 107): QEMU listens
# on a HOST port and forwards connections to the guest's 10.0.2.15:7778, so a host process can
# connect INTO the guest. Everything this project had proved over the network was the guest as a
# client; this is the other direction.
#
# Only on the mmio NIC, and only when NIFE_HOSTFWD_PORT names a port. Both restrictions are
# deliberate. This is the one QEMU flag here that **binds a port on the developer's machine**, so it
# does not belong on a plain `cargo xtask run` or on the benchmark boot, both of which share this
# runner; and the port is chosen by xtask (a free one, asked of the OS) rather than fixed here,
# because two lanes running the suite at once on one machine would otherwise collide and the loser
# would fail to start QEMU at all. The guest address is spelled out rather than defaulted so the
# line says which guest it means.
HOSTFWD=""
if [ -n "$NIFE_HOSTFWD_PORT" ]; then
    HOSTFWD=",hostfwd=tcp:127.0.0.1:$NIFE_HOSTFWD_PORT-10.0.2.15:7778"
fi

# The SMB adapter's forward (milestone 54), the same mechanism one port over: xtask's SMB prober
# (and a Mac attempting a real mount, notes/smb.md) reaches the guest's SMB listener through it.
# The guest port defaults to the test's 7779; the serve boot overrides it to SMB's own 445.
if [ -n "$NIFE_SMB_HOSTFWD_PORT" ]; then
    HOSTFWD="$HOSTFWD,hostfwd=tcp:127.0.0.1:$NIFE_SMB_HOSTFWD_PORT-10.0.2.15:${NIFE_SMB_GUEST_PORT:-7779}"
fi

NET=""
if [ -n "$NIFE_NET" ]; then
    NET="-netdev user,id=net0,$GUESTFWD,tftp=$TFTPDIR$HOSTFWD -device virtio-net-device,netdev=net0 -netdev user,id=net1,$GUESTFWD,tftp=$TFTPDIR -device virtio-net-pci,netdev=net1,disable-legacy=on,iommu_platform=on"
fi

# Attach a virtio-gpu when NIFE_GPU is set (milestone 29, the display ladder's rung one).
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
# There is no image file to fail loud on, as with the NIC. The manufactured-fact hazard (NIFE_GPU
# set but no GPU enumerated) is caught in the kernel test, which ASSERTS a GPU is present rather than
# skipping, and asserts the IOMMU is active while one is.
GPU=""
if [ -n "$NIFE_GPU" ]; then
    GPU="-device virtio-gpu-pci,disable-legacy=on,iommu_platform=on"
fi

# Attach a virtio keyboard when NIFE_KBD is set (milestone 29's input).
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
if [ -n "$NIFE_KBD" ]; then
    KBD="-device virtio-keyboard-pci,disable-legacy=on,iommu_platform=on"
fi

# Attach two virtio-rng devices when NIFE_RNG is set (milestone 56, the entropy half).
#
# BOTH transports, because the entropy service must be the same binary on either bus (DECISIONS §18)
# and a random source that works on one is not a random source. The mmio one goes on the mmio bus
# alongside the disks; it is a slot the block-device scan skips, since that scan matches on DeviceID
# and an RNG reports 4, so the nifefs=0 / redoxfs=1 / crash=2 ordering above is unaffected.
#
# The PCI one is behind the SMMU (iommu_platform=on) on the same terms as the GPU and the keyboard:
# the buffer this device writes into is where the system's key material comes from, so it is the last
# device you would want writing wherever it liked.
#
# QEMU backs virtio-rng with the host's /dev/urandom by default (`rng-random`, filename
# /dev/urandom), which is what makes these bytes real and not an emulated counter. That is a fact
# about the emulator, recorded rather than assumed: on hardware the source is the board's TRNG and
# notes/entropy.md carries the caveat.
#
# There is no image file to fail loud on, as with the NIC and the GPU. The manufactured-fact hazard
# (NIFE_RNG set but no device enumerated) is caught in the kernel test, which ASSERTS a device is
# present on each bus rather than skipping.
RNG=""
if [ -n "$NIFE_RNG" ]; then
    RNG="-device virtio-rng-device -device virtio-rng-pci,disable-legacy=on,iommu_platform=on"
fi

# Attach an NVMe controller when NIFE_NVME names an image (milestone 53's storage half): the
# first NON-virtio DMA device this project drives, which is the whole point of it. No
# iommu_platform flag, because that knob is virtio's opt-in; a real PCI device model's DMA always
# goes through the PCI address space, so with iommu=smmuv3 on the machine the controller sits
# behind the SMMU with no flag to forget, and the kernel must confine its requester id before the
# controller can fetch a single command (kernel/src/nvme.rs). serial= is mandatory (QEMU refuses
# the device without one); the value is arbitrary identity, not configuration.
#
# A set NIFE_NVME naming a missing file is an error, the NIFE_DISK lesson above: a silently
# absent controller would read as a machine fact when it is a build-order mistake. The kernel test
# ASSERTS the controller is present rather than skipping, like the GPU's.
NVME=""
if [ -n "$NIFE_NVME" ]; then
    if [ ! -f "$NIFE_NVME" ]; then
        echo "qemu-runner-aarch64: NIFE_NVME=$NIFE_NVME does not exist (xtask's mknvmedisk writes it)" >&2
        exit 1
    fi
    NVME="-drive file=$NIFE_NVME,if=none,format=raw,id=nvme0 -device nvme,serial=nife-nvme,drive=nvme0"
fi

# A QEMU monitor on a unix socket, when NIFE_GPU_MON names one (milestone 29). This is how the
# **scanout** gets proven rather than only the framebuffer: `screendump` writes a PPM of the scanout
# and it works with -display none (verified against QEMU 11.0.2), so the host can see the pixels the
# guest cannot read back. xtask drives it while the suite runs (see gpu_shot); nothing else uses it,
# and without the variable QEMU gets no monitor at all, exactly as before.
#
# The path must stay under 104 bytes: that is the OS limit on a unix socket path, and a worktree
# checkout plus target/ gets close, which is why xtask puts the socket in /tmp and not in target/.
MON=""
if [ -n "$NIFE_GPU_MON" ]; then
    MON="-monitor unix:$NIFE_GPU_MON,server,nowait"
fi

# shellcheck disable=SC2086  # $INITRD, $DISK, $NET, $GPU, $KBD, $RNG and $NVME are deliberately word-split or empty
# CPU and accelerator.
#
# By default we run under TCG (QEMU translates every aarch64 instruction), with an emulated
# cortex-a72. That is deterministic and runs identically on any host, which is what the test
# harness wants.
#
# Set NIFE_ACCEL=hvf to run under Apple's Hypervisor.framework instead: HVF puts the kernel on
# the real Apple Silicon core at guest EL1, using the hardware virtualization the chip already
# has. The coincidence that makes this a flag and not a port is that the host and the guest are the
# same ISA (aarch64). Two consequences:
#
#   - HVF runs the PHYSICAL core, so `-cpu host` is mandatory; you cannot ask for an emulated a72.
#   - gic-version is PINNED to 2, so a future QEMU default cannot swap in a GICv3 our driver does
#     not speak. QEMU emulates the GIC either way (Apple cores use their own AIC natively) and
#     injects interrupts through HVF, so the MMIO GICv2 driver keeps working.
#
# NIFE_CPU overrides the TCG model (milestone 59, the parity twin of the riscv runner's flag).
# Under HVF there is nothing to override: the guest runs on the physical Apple core, so `-cpu host`
# is the only answer and asking for anything else is a mistake worth failing on rather than
# silently ignoring.
if [ "$NIFE_ACCEL" = "hvf" ]; then
    # iommu=smmuv3 is on BOTH paths since milestone 81, and this is a correction: it used to be
    # TCG-only, on the recorded belief that "smmuv3 emulation alongside HVF acceleration is the
    # fragile combination". Nobody had run it. The suite on the physical core says otherwise, and
    # the belief cost a real gap while it stood: without an SMMU the display test fails outright
    # ("a virtio-gpu is present but the IOMMU is not active") and the DMA confinement tests, which
    # assert the HARDWARE faults an escaping DMA, would have had no hardware to assert about.
    #
    # It is also the right place for it on principle. The accelerator chooses how CPU instructions
    # execute; the SMMU is in front of the PCIe root complex and translates DEVICE traffic, which
    # QEMU emulates in the host process either way. The two are orthogonal, and the suite proves it.
    MACHINE="virt,accel=hvf,gic-version=2,iommu=smmuv3"
    if [ -n "$NIFE_CPU" ] && [ "$NIFE_CPU" != "host" ]; then
        echo "qemu-runner-aarch64: NIFE_CPU=$NIFE_CPU cannot apply under HVF (the guest runs the physical core; -cpu host is mandatory)" >&2
        exit 1
    fi
    CPU="host"
else
    # iommu=smmuv3 puts an SMMUv3 in front of the PCIe root complex (milestone 16b). The device tree
    # then carries an `smmuv3@...` node (memory::smmu_region finds it) and an identity iommu-map for
    # the bus. A plain boot without a PCI disk still gets the SMMU; it just has nothing to confine.
    # The HVF branch above takes the same flag, since milestone 81.
    MACHINE="virt,gic-version=2,iommu=smmuv3"
    CPU="${NIFE_CPU:-cortex-a72}"
fi

# Number of cores. Four by default, the SMP tests' shape (§11); NIFE_SMP moves it, and the
# ceiling is cpu::MAX_CPUS (the per-CPU statics), not this default: the suite asserts loudly if
# -smp exceeds what the kernel can seat. QEMU brings up core 0 running; the kernel starts the rest
# itself via PSCI CPU_ON (see smp.rs).
SMP="${NIFE_SMP:-4}"

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
    $RNG \
    $NVME \
    $MON \
    "$@"

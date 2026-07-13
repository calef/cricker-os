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

exec qemu-system-aarch64 \
    -machine virt \
    -cpu cortex-a72 \
    -nographic \
    -semihosting \
    -kernel "$IMG" \
    "$@"

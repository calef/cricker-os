#!/bin/sh
#
# The RISC-V QEMU runner (milestone 20). Cargo invokes this for `cargo run` and `cargo test` on the
# riscv64 target, appending the path to the ELF it just built.
#
# Simpler than the aarch64 runner: QEMU's `virt` machine boots a RISC-V ELF directly with `-kernel`,
# and `-bios default` runs OpenSBI, which initializes the machine in M-mode and hands our payload
# control in S-mode (hart id in a0, device-tree pointer in a1). There is no flat-Image / objcopy
# step, because RISC-V has no equivalent of the arm64 Image header that the aarch64 path needs.
#
# The kernel halts with `wfi` (arch::halt), so QEMU does not exit on its own. Bound any interactive
# run with scripts/qemu-bounded.sh, exactly as on aarch64 (see CLAUDE.md, "Never leave QEMU
# running"). See notes/riscv-port.md.

set -e

ELF="$1"
shift

# One hart for now: SMP bring-up (SBI HSM) is the traps/timer step. The NS16550 console is on the
# `virt` machine at 0x1000_0000; `-serial stdio` wires it to this terminal.
SMP="${CRICKER_SMP:-1}"

exec qemu-system-riscv64 \
    -machine virt \
    -cpu rv64 \
    -smp "$SMP" \
    -m 128M \
    -bios default \
    -display none \
    -serial stdio \
    -kernel "$ELF" \
    "$@"

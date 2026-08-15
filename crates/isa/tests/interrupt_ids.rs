//! The console UART's interrupt line, read from each machine's own tree, on the host.
//!
//! The number this replaces was the last QEMU constant on the interrupt path: `UART_IRQ = 10` in
//! `kernel/src/main.rs`, which on the JH7110 armed an unrelated PLIC source, and boot 13
//! (2026-08-15) proved it on silicon when a key press at the completed tour's prompt reached
//! nothing (notes/visionfive2.md, BUGS). These tests hold the fix's whole claim: the same read
//! that answers 10 on QEMU's tree answers 32 on both JH7110 fixtures, and 33 on the aarch64
//! `virt` tree whose GIC speaks three-cell entries, so the number the kernel arms is the
//! machine's rather than the emulator's.
//!
//! The three trees deliberately cover the three places `interrupt-parent` lives: on the serial
//! node itself (QEMU riscv64), once on the root (QEMU aarch64), and on `/soc` per the mainline
//! dtsi (the JH7110 upstream model; the vendor fixture spells it per-node).

use isa::interrupt_id;

const QEMU_RISCV_VIRT: &[u8] = include_bytes!("fixtures/qemu-riscv64-virt-smp4.dtb");
const JH7110: &[u8] = include_bytes!("fixtures/jh7110.dtb");
const JH7110_VENDOR: &[u8] = include_bytes!("fixtures/jh7110-vendor.dtb");
// The aarch64 machine's tree lives with the dtb crate's fixture tests; reuse it rather than
// committing a second copy that could drift from the one the parser is proven against.
const QEMU_AARCH64_VIRT: &[u8] = include_bytes!("../../dtb/tests/fixtures/qemu-aarch64-virt.dtb");

/// The console node's name, as `kernel/src/console.rs` pins it: both QEMU riscv64 `virt` and the
/// JH7110 spell UART0 exactly this way.
const RISCV_UART: &[u8] = b"serial@10000000";

fn tree(bytes: &[u8]) -> dtb::Dtb<'_> {
    dtb::Dtb::from_bytes(bytes).expect("fixture is a valid device tree")
}

/// **QEMU `virt` says 10**, the number the old constant hardcoded: on the machine the constant
/// was written for, the tree and the constant agree, which is why the bug stayed invisible until
/// a board with a different answer ran the same kernel.
#[test]
fn qemu_riscv_virt_uart_is_plic_source_10() {
    let id = interrupt_id::of_node(&tree(QEMU_RISCV_VIRT), RISCV_UART).unwrap();
    assert_eq!(id, Some(10));
}

/// **Both JH7110 trees say 32**, the line boot 13 proved the kernel was not arming. The upstream
/// model states `interrupt-parent` once on `/soc` (inheritance), the vendor fixture on the serial
/// node itself; the answer must not depend on which spelling a board's firmware chose.
#[test]
fn jh7110_uart_is_plic_source_32_in_both_trees() {
    assert_eq!(
        interrupt_id::of_node(&tree(JH7110), RISCV_UART).unwrap(),
        Some(32),
        "the mainline-modeled tree, parent inherited from /soc"
    );
    assert_eq!(
        interrupt_id::of_node(&tree(JH7110_VENDOR), RISCV_UART).unwrap(),
        Some(32),
        "the vendor-modeled tree, parent on the node itself"
    );
}

/// **QEMU aarch64 `virt` says 33**: the pl011's `interrupts = <0 1 4>` is SPI 1 in the GIC's
/// three-cell form, and the INTID the GIC delivers is bank base 32 plus 1, exactly the number
/// `user.rs` has always hardcoded as `UART_RX_INTID`. The parent here is inherited from the root,
/// the third spelling.
#[test]
fn qemu_aarch64_virt_pl011_is_intid_33() {
    let id = interrupt_id::of_node(&tree(QEMU_AARCH64_VIRT), b"pl011@9000000").unwrap();
    assert_eq!(id, Some(33));
}

/// **A tree that does not say answers `None`, not a guess.** A node with no `interrupts` at all
/// (the memory node), and a node that wires its line through `interrupts-extended` instead (the
/// CLINT), both send the caller to its documented fallback rather than arming a number pulled
/// from the wrong property.
#[test]
fn a_tree_that_does_not_say_answers_none() {
    let dt = tree(QEMU_RISCV_VIRT);
    assert_eq!(interrupt_id::of_node(&dt, b"memory@").unwrap(), None);
    assert_eq!(interrupt_id::of_node(&dt, b"clint@").unwrap(), None);
    assert_eq!(interrupt_id::of_node(&dt, b"no-such-node@").unwrap(), None);
}

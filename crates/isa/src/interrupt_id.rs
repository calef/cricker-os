//! **Which interrupt line does this device raise?** The `interrupts` property, decoded per the
//! interrupt parent's own `#interrupt-cells`.
//!
//! The VisionFive 2's first completed tour (boot 13, 2026-08-15, notes/visionfive2.md) is why this
//! exists. The tour's UART-driver step passed `UART_IRQ = 10` to the demo, which is QEMU `virt`'s
//! NS16550 PLIC line; on the JH7110, UART0 interrupts on line **32**, so the board build enabled an
//! unrelated PLIC source and a key press at the finished tour's prompt reached nothing. The number
//! was the last QEMU constant on the interrupt path, and like the PLIC context layout before it
//! ([`crate::plic`]), it was never a constant to begin with: it is a fact the device tree states.
//!
//! Decoding it is not one read, because the property's *shape* belongs to a different node. An
//! `interrupts` value is a list of entries each `#interrupt-cells` wide, and that count is declared
//! by the device's **interrupt parent**, named by phandle in an `interrupt-parent` property that
//! the spec makes inheritable (QEMU riscv64 writes it on the serial node, QEMU aarch64 once on the
//! root, the mainline JH7110 dtsi on `/soc`). So the walk is: the device's `interrupts`, the
//! nearest `interrupt-parent`, the controller that phandle names, its `#interrupt-cells`, and only
//! then the entry itself:
//!
//! - **1 cell** (RISC-V PLIC, `#interrupt-cells = <1>`): the cell is the source number, verbatim.
//! - **3 cells** (Arm GIC): `<type number flags>`, where the number is *bank-relative* and the
//!   INTID the GIC actually delivers adds the bank base: SPI (type 0) starts at 32, PPI (type 1)
//!   at 16. QEMU's PL011 says `<0 1 4>`, which is SPI 1, which is the INTID 33 the kernel has
//!   always hardcoded.
//!
//! # BUGS
//!
//! - **Only the first `interrupts` entry is decoded.** A device with several (a combined
//!   TX/RX/error split, say) answers with its first, which is the convention every UART binding
//!   this kernel meets follows. A caller needing entry *n* needs a wider read.
//! - **Cell counts other than 1 and 3 answer `None`**, honestly: a 2-cell parent (some PIC-style
//!   controllers) or a 4-cell `GICv3` extension carries information this decoder does not
//!   understand, and a number pulled from the wrong cell would arm the wrong source, which is the
//!   exact bug this module exists to end. `None` sends the caller to its documented fallback.
//! - **`interrupts-extended` (the property, on the device) is not read.** No tree this kernel
//!   meets puts it on a UART; the PLIC's own use of it is [`crate::plic`]'s.
//!
//! Name: provisional (`interrupt_id` for the module, following `cpu_list`'s pattern of a shared
//! question beside the two arch records; the GIC spelling "INTID" is where the noun comes from).

use dtb::{Dtb, Error};

/// The GIC's SPI bank base: `interrupts = <0 n ...>` means INTID `32 + n`.
const GIC_SPI_BASE: u32 = 32;
/// The GIC's PPI bank base: `interrupts = <1 n ...>` means INTID `16 + n`.
const GIC_PPI_BASE: u32 = 16;

/// **The interrupt id of the first node whose name starts with `node`**, decoded per its interrupt
/// parent's `#interrupt-cells`. `Ok(None)` when the tree does not say: no such node, no
/// `interrupts`, no resolvable `interrupt-parent`, or an entry shape this decoder does not
/// understand (see the module's BUGS). The caller owns the fallback, and should say which source
/// won: a transcript that names the number's origin is diagnosable at a bench, one that does not
/// already cost a boot (notes/visionfive2.md).
pub fn of_node(dt: &Dtb<'_>, node: &[u8]) -> Result<Option<u32>, Error> {
    let Some(interrupts) = dt.node_prop(node, b"interrupts")? else {
        return Ok(None);
    };
    let Some(parent) = dt.node_prop_inherited(node, b"interrupt-parent")? else {
        return Ok(None);
    };
    let Some(phandle) = be32_word(parent, 0) else {
        return Ok(None);
    };
    let Some(cells_prop) = dt.phandle_prop(phandle, b"#interrupt-cells")? else {
        return Ok(None);
    };
    let Some(cells) = be32_word(cells_prop, 0) else {
        return Ok(None);
    };

    Ok(match cells {
        // A PLIC (or any single-cell parent): the cell is the source number.
        1 => be32_word(interrupts, 0),
        // A GIC: <type number flags>, the number relative to its bank's base.
        3 => match (be32_word(interrupts, 0), be32_word(interrupts, 1)) {
            (Some(0), Some(n)) => GIC_SPI_BASE.checked_add(n),
            (Some(1), Some(n)) if n < 16 => Some(GIC_PPI_BASE + n),
            _ => None,
        },
        _ => None,
    })
}

/// Big-endian cell `i` of a property's bytes, `None` when the property is too short.
fn be32_word(bytes: &[u8], i: usize) -> Option<u32> {
    let at = i.checked_mul(4)?;
    bytes
        .get(at..at.checked_add(4)?)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

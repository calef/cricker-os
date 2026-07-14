//! A minimal flattened device tree (FDT) parser.
//!
//! The device tree is the machine describing itself: where RAM is, where the UART is,
//! where the interrupt controller lives, how many CPUs exist. QEMU hands us a pointer
//! to one in `x0` (see notes/boot-protocol.md), and this is how we read it.
//!
//! # Everything here is big-endian
//!
//! The FDT format predates the little-endian consensus and never changed. Every
//! integer in the blob is stored big-endian, on a machine that is little-endian. So
//! every read goes through [`be32`] or [`be64`], and forgetting one gives you a
//! plausible-looking number that is wrong by a factor of 16 million.
//!
//! # Why this is a separate crate
//!
//! It is **pure logic**: bytes in, structs out. No hardware, no `unsafe` beyond one
//! entry point, no kernel. So it compiles for the host and its tests run in
//! milliseconds against a real device tree dumped from QEMU, instead of booting an
//! emulator. See DECISIONS.md §7.

#![cfg_attr(not(test), no_std)]

/// A contiguous span of physical memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub start: u64,
    pub size: u64,
}

impl Region {
    pub fn end(&self) -> u64 {
        self.start + self.size
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The first four bytes weren't `0xd00dfeed`. Either the pointer is wrong or the
    /// bootloader didn't give us a device tree at all.
    BadMagic(u32),
    /// The blob claims a version we don't understand.
    UnsupportedVersion(u32),
    /// An offset in the header points outside the blob. Truncated or corrupt.
    Truncated,
    /// A token we don't recognize. The structure block is malformed.
    BadToken(u32),
    /// The caller's output slice was too small to hold every region found.
    TooManyRegions,
}

// Structure-block tokens.
const FDT_BEGIN_NODE: u32 = 0x1;
const FDT_END_NODE: u32 = 0x2;
const FDT_PROP: u32 = 0x3;
const FDT_NOP: u32 = 0x4;
const FDT_END: u32 = 0x9;

const MAGIC: u32 = 0xd00d_feed;
const HEADER_LEN: usize = 40;

fn be32(bytes: &[u8], at: usize) -> Result<u32, Error> {
    let slice = bytes.get(at..at + 4).ok_or(Error::Truncated)?;
    Ok(u32::from_be_bytes(slice.try_into().unwrap()))
}

fn be64(bytes: &[u8], at: usize) -> Result<u64, Error> {
    let slice = bytes.get(at..at + 8).ok_or(Error::Truncated)?;
    Ok(u64::from_be_bytes(slice.try_into().unwrap()))
}

/// A parsed, borrowed device tree blob.
#[derive(Debug)]
pub struct Dtb<'a> {
    bytes: &'a [u8],
    off_struct: usize,
    off_strings: usize,
    off_rsvmap: usize,
}

impl<'a> Dtb<'a> {
    /// # Safety
    ///
    /// `ptr` must point at a device tree blob that stays valid for `'a`. We read the
    /// header to learn the blob's own length, which means we trust the first 8 bytes
    /// before we have validated anything. The magic check immediately after is what
    /// makes that survivable: a wrong pointer almost certainly fails it.
    pub unsafe fn from_ptr(ptr: *const u8) -> Result<Self, Error> {
        // Read just enough to learn how long the thing claims to be.
        let header = unsafe { core::slice::from_raw_parts(ptr, HEADER_LEN) };
        let magic = be32(header, 0)?;
        if magic != MAGIC {
            return Err(Error::BadMagic(magic));
        }
        let total = be32(header, 4)? as usize;

        let bytes = unsafe { core::slice::from_raw_parts(ptr, total) };
        Self::from_bytes(bytes)
    }

    pub fn from_bytes(bytes: &'a [u8]) -> Result<Self, Error> {
        let magic = be32(bytes, 0)?;
        if magic != MAGIC {
            return Err(Error::BadMagic(magic));
        }

        let total = be32(bytes, 4)? as usize;
        if bytes.len() < total || total < HEADER_LEN {
            return Err(Error::Truncated);
        }

        // We only understand version 17 and later, which is everything made since about
        // 2005. The `last_comp_version` field is the blob telling us the oldest reader
        // it is still compatible with.
        let last_comp_version = be32(bytes, 24)?;
        if last_comp_version > 17 {
            return Err(Error::UnsupportedVersion(last_comp_version));
        }

        let dtb = Dtb {
            off_struct: be32(bytes, 8)? as usize,
            off_strings: be32(bytes, 12)? as usize,
            off_rsvmap: be32(bytes, 16)? as usize,
            bytes: &bytes[..total],
        };

        if dtb.off_struct >= total || dtb.off_strings >= total || dtb.off_rsvmap >= total {
            return Err(Error::Truncated);
        }

        Ok(dtb)
    }

    /// How many bytes the blob occupies. We need this to mark it as reserved: it is
    /// sitting in the very RAM we are about to start handing out.
    pub fn total_size(&self) -> usize {
        self.bytes.len()
    }

    /// The memory reservation block: regions the bootloader is telling us **not to
    /// touch**, before we've parsed a single node.
    ///
    /// This is a separate, deliberately dead-simple structure precisely so that a
    /// kernel can honour it without having to parse anything. QEMU's `virt` leaves it
    /// empty, but a real board's firmware often does not, and a kernel that skips it
    /// will happily allocate over the firmware's own tables.
    pub fn reserved_regions(&self, out: &mut [Region]) -> Result<usize, Error> {
        let mut at = self.off_rsvmap;
        let mut n = 0;

        loop {
            let start = be64(self.bytes, at)?;
            let size = be64(self.bytes, at + 8)?;
            at += 16;

            // The list is terminated by an all-zero entry.
            if start == 0 && size == 0 {
                return Ok(n);
            }

            *out.get_mut(n).ok_or(Error::TooManyRegions)? = Region { start, size };
            n += 1;
        }
    }

    /// Every `/memory` node's `reg` property: the actual RAM.
    ///
    /// This is the whole reason we bothered with the boot protocol. Milestone 1
    /// hardcoded `0x4000_0000` because we'd read it off a `dtc` dump by hand. Now the
    /// machine tells us, which means the same kernel binary works on a board with a
    /// different memory map.
    pub fn memory_regions(&self, out: &mut [Region]) -> Result<usize, Error> {
        // `reg` is a list of (address, size) pairs, but how many 32-bit cells each of
        // those takes is not fixed. It's declared by #address-cells and #size-cells on
        // the PARENT node. For a /memory node the parent is the root.
        //
        // The spec's defaults are 2 and 1. Nearly every 64-bit machine says 2 and 2
        // (i.e. 64-bit addresses, 64-bit sizes), but we read them rather than assume.
        let mut address_cells = 2u32;
        let mut size_cells = 1u32;

        let mut found = 0;
        let mut depth = 0i32;

        // The depth at which we entered a /memory node, or None.
        //
        // Tracking the *depth* rather than a bare "am I inside one" flag matters: a
        // node's properties are the ones seen while `depth` equals the node's own depth.
        // If a /memory node ever had a child, a bare flag would be cleared by the
        // child's END_NODE and we'd stop reading the parent's remaining properties. No
        // real device tree does this today, which is exactly why it would be a lurking
        // bug rather than an obvious one.
        let mut memory_at: Option<i32> = None;
        let mut at = self.off_struct;

        loop {
            let token = be32(self.bytes, at)?;
            at += 4;

            match token {
                FDT_BEGIN_NODE => {
                    let name = self.cstr(at)?;
                    at += align4(name.len() + 1);
                    depth += 1;

                    // A memory node is a child of the root named `memory` or
                    // `memory@<address>`. (There is also a `device_type = "memory"`
                    // property, which is the more correct check, but it arrives *after*
                    // the node name and the name is unambiguous in practice.)
                    if depth == 2 && (name == b"memory" || name.starts_with(b"memory@")) {
                        memory_at = Some(depth);
                    }
                }

                FDT_END_NODE => {
                    if memory_at == Some(depth) {
                        memory_at = None;
                    }
                    depth -= 1;
                }

                FDT_PROP => {
                    let len = be32(self.bytes, at)? as usize;
                    let name_off = be32(self.bytes, at + 4)? as usize;
                    let value_at = at + 8;
                    at = value_at + align4(len);

                    let name = self.cstr(self.off_strings + name_off)?;

                    // The root's cell counts, which we need before we can decode any
                    // `reg`. They appear on the root node, which we visit first, so by
                    // the time we reach a /memory node these are correct.
                    if depth == 1 {
                        match name {
                            b"#address-cells" => address_cells = be32(self.bytes, value_at)?,
                            b"#size-cells" => size_cells = be32(self.bytes, value_at)?,
                            _ => {}
                        }
                    }

                    if memory_at == Some(depth) && name == b"reg" {
                        found += self.decode_reg(
                            value_at,
                            len,
                            address_cells,
                            size_cells,
                            &mut out[found..],
                        )?;
                    }
                }

                FDT_NOP => {}
                FDT_END => return Ok(found),
                other => return Err(Error::BadToken(other)),
            }
        }
    }

    /// The `reg` regions of the first node whose name starts with `prefix`.
    ///
    /// Used to find the interrupt controller (`intc@8000000`) without hardcoding its address.
    /// The GIC has **two** register blocks (a distributor and a per-CPU interface), so `reg`
    /// here decodes to two regions, and the order is part of the binding: distributor first.
    ///
    /// Matching on a name prefix rather than the `compatible` string is a deliberate
    /// simplification. `compatible` is the *correct* way to identify a device (`intc@...` is
    /// just a conventional name), and a real driver would match `"arm,cortex-a15-gic"`. We
    /// look at names because it is ten lines instead of forty and we have exactly one board.
    /// Written down so the Pi port knows what to fix.
    pub fn node_reg(&self, prefix: &[u8], out: &mut [Region]) -> Result<usize, Error> {
        let mut address_cells = 2u32;
        let mut size_cells = 1u32;

        let mut depth = 0i32;
        let mut target_at: Option<i32> = None;
        let mut at = self.off_struct;

        loop {
            let token = be32(self.bytes, at)?;
            at += 4;

            match token {
                FDT_BEGIN_NODE => {
                    let name = self.cstr(at)?;
                    at += align4(name.len() + 1);
                    depth += 1;

                    if depth == 2 && name.starts_with(prefix) && target_at.is_none() {
                        target_at = Some(depth);
                    }
                }

                FDT_END_NODE => {
                    if target_at == Some(depth) {
                        // We have walked the whole node. If it had a `reg` we already decoded
                        // it; either way, stop looking.
                        target_at = None;
                    }
                    depth -= 1;
                }

                FDT_PROP => {
                    let len = be32(self.bytes, at)? as usize;
                    let name_off = be32(self.bytes, at + 4)? as usize;
                    let value_at = at + 8;
                    at = value_at + align4(len);

                    let name = self.cstr(self.off_strings + name_off)?;

                    // The ROOT's cell counts. A device node may declare its own #address-cells
                    // for its *children*, but its own `reg` is decoded with its PARENT's, and
                    // for a node at depth 2 the parent is the root.
                    if depth == 1 {
                        match name {
                            b"#address-cells" => address_cells = be32(self.bytes, value_at)?,
                            b"#size-cells" => size_cells = be32(self.bytes, value_at)?,
                            _ => {}
                        }
                    }

                    if target_at == Some(depth) && name == b"reg" {
                        return self.decode_reg(value_at, len, address_cells, size_cells, out);
                    }
                }

                FDT_NOP => {}
                FDT_END => return Ok(0),
                other => return Err(Error::BadToken(other)),
            }
        }
    }

    /// The initial ramdisk, if the bootloader placed one.
    ///
    /// Declared in `/chosen` as `linux,initrd-start` and `linux,initrd-end`.
    ///
    /// **This memory is ours to protect.** The bootloader loaded a file into RAM for us
    /// and told us where it put it. If we don't reserve it, the frame allocator hands it
    /// out to the first caller and the initrd is destroyed before we ever read a byte of
    /// it. Milestone 8 (a filesystem) and milestone 10 (a userspace shell to load) both
    /// want this, and by then the bug would be far away from its cause.
    pub fn initrd(&self) -> Result<Option<Region>, Error> {
        let mut start: Option<u64> = None;
        let mut end: Option<u64> = None;

        let mut depth = 0i32;
        let mut chosen_at: Option<i32> = None;
        let mut at = self.off_struct;

        loop {
            let token = be32(self.bytes, at)?;
            at += 4;

            match token {
                FDT_BEGIN_NODE => {
                    let name = self.cstr(at)?;
                    at += align4(name.len() + 1);
                    depth += 1;
                    if depth == 2 && name == b"chosen" {
                        chosen_at = Some(depth);
                    }
                }

                FDT_END_NODE => {
                    if chosen_at == Some(depth) {
                        chosen_at = None;
                    }
                    depth -= 1;
                }

                FDT_PROP => {
                    let len = be32(self.bytes, at)? as usize;
                    let name_off = be32(self.bytes, at + 4)? as usize;
                    let value_at = at + 8;
                    at = value_at + align4(len);

                    if chosen_at == Some(depth) {
                        match self.cstr(self.off_strings + name_off)? {
                            b"linux,initrd-start" => start = Some(self.int(value_at, len)?),
                            b"linux,initrd-end" => end = Some(self.int(value_at, len)?),
                            _ => {}
                        }
                    }
                }

                FDT_NOP => {}
                FDT_END => break,
                other => return Err(Error::BadToken(other)),
            }
        }

        Ok(match (start, end) {
            // `initrd-end` is exclusive, so an empty initrd (start == end) is a "no".
            (Some(s), Some(e)) if e > s => Some(Region {
                start: s,
                size: e - s,
            }),
            _ => None,
        })
    }

    /// An integer property.
    ///
    /// The device tree spec lets these be either 32 or 64 bits wide, and **the only way
    /// to tell is the property's length**. QEMU writes `linux,initrd-start` as 8 bytes;
    /// a 32-bit platform writes 4. Assume one and you silently misread the other.
    fn int(&self, at: usize, len: usize) -> Result<u64, Error> {
        match len {
            4 => Ok(be32(self.bytes, at)? as u64),
            8 => Ok(be64(self.bytes, at)?),
            _ => Err(Error::Truncated),
        }
    }

    /// Decode a `reg` property: a packed list of (address, size) pairs, where each
    /// value is `cells` 32-bit big-endian words concatenated.
    fn decode_reg(
        &self,
        at: usize,
        len: usize,
        address_cells: u32,
        size_cells: u32,
        out: &mut [Region],
    ) -> Result<usize, Error> {
        let pair_bytes = (address_cells as usize + size_cells as usize) * 4;
        if pair_bytes == 0 {
            return Ok(0);
        }

        let mut n = 0;
        let mut cursor = at;

        while cursor + pair_bytes <= at + len {
            let start = self.cells(cursor, address_cells)?;
            let size = self.cells(cursor + address_cells as usize * 4, size_cells)?;
            cursor += pair_bytes;

            // A zero-size region is legal and useless. Skip it rather than handing the
            // allocator an empty range to reason about.
            if size == 0 {
                continue;
            }

            *out.get_mut(n).ok_or(Error::TooManyRegions)? = Region { start, size };
            n += 1;
        }

        Ok(n)
    }

    /// Read `count` 32-bit big-endian cells and concatenate them into one u64.
    fn cells(&self, at: usize, count: u32) -> Result<u64, Error> {
        let mut value = 0u64;
        for i in 0..count as usize {
            value = (value << 32) | be32(self.bytes, at + i * 4)? as u64;
        }
        Ok(value)
    }

    /// A null-terminated string in the blob, returned without its terminator.
    fn cstr(&self, at: usize) -> Result<&'a [u8], Error> {
        let rest = self.bytes.get(at..).ok_or(Error::Truncated)?;
        let end = rest.iter().position(|&b| b == 0).ok_or(Error::Truncated)?;
        Ok(&rest[..end])
    }
}

/// Everything in the structure block is padded to a 4-byte boundary.
fn align4(n: usize) -> usize {
    n.div_ceil(4) * 4
}

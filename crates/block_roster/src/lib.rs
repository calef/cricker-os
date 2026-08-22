//! **What drives are attached** (milestone 57): the roster page, and the authority it is not.
//!
//! `lsblk` on Linux reads `/sys/block`, which every process on the machine can read, and the
//! listing is therefore ambient: knowing what disks exist is free, and so is naming one to a tool
//! that will write to it. Here it is a capability. The kernel writes this page once at wiring time
//! and maps it **read-only** into exactly the programs that were granted it; a program without the
//! mapping has nowhere to look, and the refusal is "you hold no such capability" rather than a
//! permission check.
//!
//! # The line this draws, which is the point of the milestone
//!
//! Listing the drives and holding one of them are **different authorities**:
//!
//! - the roster says a device exists, what bus it came off, and its ordinal;
//! - a block-service endpoint says you may read and write one particular device's blocks.
//!
//! So the roster deliberately carries **no capacity**, and that is a design decision rather than an
//! omission. A disk's size is a fact about its contents-bearing surface, and you learn it by asking
//! the device (`fs_proto::blk::SIZE`), which takes the endpoint. An enumerator that answered "how
//! big" would be answering a question only a holder should be able to ask, and it would also have
//! to bring a PCI function up to find out, which has side effects the kernel has no business
//! causing on behalf of a listing.
//!
//! Compare the compositor's window enumeration (DECISIONS §33), which is the same shape and the
//! precedent this follows: a **read-only mapping, not a verb**. There is no request to forge, no
//! server to confuse, and nothing to authorize at read time, because the authorization happened
//! when the mapping was made.
//!
//! # This is not a `*_proto`, and the difference is the whole design
//!
//! Ten crates in `crates/` end in `_proto` and describe **messages two programs exchange**:
//! `fs_proto`, `sink_proto`, `clock_proto` and the rest. This one is not among them, and a reader
//! scanning the directory should not expect a protocol here. It describes **bytes at an address**,
//! written once by the kernel at wiring time and thereafter only read.
//!
//! That is not a naming detail, it is the authority claim restated: a protocol has a server, and a
//! server is something that can be asked, confused, or persuaded. There is nothing here to ask. The
//! decision about who may see the machine's disks was made when the mapping was created, and a
//! program either holds the page or has nowhere to look.
//!
//! It also has a consequence the `_proto` crates carry and this one does not: **no seqlock.** The
//! clock page needs one because the clock service republishes it (`clock_proto::ClockPage`); this
//! page never changes after the mapping exists, so a reader needs no protocol for reading it
//! consistently. Hot plug would change that, and it would be a change to this crate rather than to
//! its readers.
//!
//! # The page
//!
//! ```text
//!   0..8     MAGIC
//!   8..12    count: how many entries follow
//!   12..16   capacity: how many entries this page has room for
//!   16..     entries, ENTRY_BYTES each
//!
//!   an entry:
//!   0..4     ordinal: which device of this transport, 0-based
//!   4..8     transport: TRANSPORT_MMIO or TRANSPORT_PCI
//! ```
//!
//! Little-endian, fixed width, no pointers: the reader is a `no_std` program with no allocator and
//! the writer is the kernel, and a self-describing format between two programs in one tree would be
//! ceremony. A zeroed frame reads as an empty roster with the wrong magic, which [`Roster::read`]
//! refuses, so a page nobody wrote is never mistaken for a machine with no disks.
//!
//! ```
//! # use block_roster::{Device, Roster, TRANSPORT_MMIO, TRANSPORT_PCI};
//! let mut page = [0u8; 4096];
//! let devices = [
//!     Device { ordinal: 0, transport: TRANSPORT_MMIO },
//!     Device { ordinal: 0, transport: TRANSPORT_PCI },
//! ];
//! block_roster::write(&mut page, &devices).unwrap();
//!
//! let roster = Roster::read(&page).unwrap();
//! assert_eq!(roster.len(), 2);
//! assert_eq!(roster.get(1).unwrap().transport, TRANSPORT_PCI);
//! ```
//!
//! Name: ratified 2026-08-03 (calef, milestone 57). The lane shipped it provisionally and calef
//! approved it by name. It deliberately does not take the `*_proto` suffix its ten neighbours
//! carry: a protocol has a server, and a server is something that can be asked, confused or
//! persuaded, where this is a read-only page whose authorization happened when the mapping was
//! made.

#![no_std]

/// `"blkrostr"` little-endian, at offset 0. A page that does not start with this is not a roster,
/// and in particular a zeroed frame is not an empty one.
pub const MAGIC: u64 = u64::from_le_bytes(*b"blkrostr");

/// Bytes per entry. Two `u32`s, and the fixed width is what lets a reader index without parsing.
pub const ENTRY_BYTES: usize = 8;

/// Where the first entry starts: magic, count, capacity.
pub const HEADER_BYTES: usize = 16;

/// A virtio-mmio device: the ordinal is its position among block devices on the mmio bus, counted
/// the way the kernel's own scan counts them.
pub const TRANSPORT_MMIO: u32 = 0;

/// A virtio-blk function on the PCIe bus (DECISIONS §18's transport).
pub const TRANSPORT_PCI: u32 = 1;

/// One attached block device: which bus, and which one of them.
///
/// Deliberately not a handle. Holding this tells you a device exists; it does not let you touch it,
/// and there is no method here that turns one into the other. The kernel decides which program gets
/// which device's endpoint, at wiring time, and nothing a program computes from a roster can change
/// that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Device {
    /// Which block device of this transport, 0-based, in the order the kernel's scan finds them.
    pub ordinal: u32,
    /// [`TRANSPORT_MMIO`] or [`TRANSPORT_PCI`].
    pub transport: u32,
}

impl Device {
    /// The transport's name, for a listing a person reads. `None` for a number this build does not
    /// know, which is what a reader of a page written by a newer kernel would see.
    pub const fn transport_name(&self) -> Option<&'static str> {
        match self.transport {
            TRANSPORT_MMIO => Some("virtio-mmio"),
            TRANSPORT_PCI => Some("virtio-pci"),
            _ => None,
        }
    }
}

/// How many entries a page of `len` bytes can hold.
pub const fn capacity_of(len: usize) -> usize {
    if len < HEADER_BYTES {
        0
    } else {
        (len - HEADER_BYTES) / ENTRY_BYTES
    }
}

/// Lay a roster into `page`. Fails if the page cannot hold `devices`.
///
/// The kernel is the only caller, and it writes the page before any program can see it. See the
/// crate header for what that buys the reader.
pub fn write(page: &mut [u8], devices: &[Device]) -> Result<(), Full> {
    let capacity = capacity_of(page.len());
    if devices.len() > capacity {
        return Err(Full);
    }
    page[..8].copy_from_slice(&MAGIC.to_le_bytes());
    page[8..12].copy_from_slice(&(devices.len() as u32).to_le_bytes());
    page[12..16].copy_from_slice(&(capacity as u32).to_le_bytes());
    for (i, d) in devices.iter().enumerate() {
        let at = HEADER_BYTES + i * ENTRY_BYTES;
        page[at..at + 4].copy_from_slice(&d.ordinal.to_le_bytes());
        page[at + 4..at + 8].copy_from_slice(&d.transport.to_le_bytes());
    }
    Ok(())
}

/// More devices than the page has room for. The kernel treats this as a wiring bug rather than a
/// runtime condition: a machine with more disks than a page can name is a machine this design has
/// to be told about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Full;

/// A roster page a program holds, borrowed rather than copied.
#[derive(Debug, Clone, Copy)]
pub struct Roster<'a> {
    entries: &'a [u8],
    count: usize,
}

impl<'a> Roster<'a> {
    /// Read a mapped page. `None` if the magic is wrong (which includes a frame nobody wrote) or
    /// the count runs past what the page holds.
    ///
    /// Both refusals matter for the same reason: this page arrives from outside the program, and a
    /// reader that trusted a count it did not check would index past its own mapping.
    pub fn read(page: &'a [u8]) -> Option<Roster<'a>> {
        if page.len() < HEADER_BYTES {
            return None;
        }
        if u64::from_le_bytes(page[..8].try_into().ok()?) != MAGIC {
            return None;
        }
        let count = u32::from_le_bytes(page[8..12].try_into().ok()?) as usize;
        if count > capacity_of(page.len()) {
            return None;
        }
        Some(Roster {
            entries: &page[HEADER_BYTES..],
            count,
        })
    }

    /// How many devices are attached.
    pub const fn len(&self) -> usize {
        self.count
    }

    /// Whether the machine has no block devices at all. A real answer, not an error: a diskless
    /// boot is a thing this OS does.
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// The `i`-th device, or `None` past the end.
    pub fn get(&self, i: usize) -> Option<Device> {
        if i >= self.count {
            return None;
        }
        let at = i * ENTRY_BYTES;
        Some(Device {
            ordinal: u32::from_le_bytes(self.entries[at..at + 4].try_into().ok()?),
            transport: u32::from_le_bytes(self.entries[at + 4..at + 8].try_into().ok()?),
        })
    }

    /// Every device, in the order the kernel found them.
    pub fn iter(&self) -> impl Iterator<Item = Device> + '_ {
        (0..self.count).filter_map(|i| self.get(i))
    }

    /// How many of the devices came off `transport`.
    pub fn count_on(&self, transport: u32) -> usize {
        self.iter().filter(|d| d.transport == transport).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> [Device; 4] {
        [
            Device {
                ordinal: 0,
                transport: TRANSPORT_MMIO,
            },
            Device {
                ordinal: 1,
                transport: TRANSPORT_MMIO,
            },
            Device {
                ordinal: 2,
                transport: TRANSPORT_MMIO,
            },
            Device {
                ordinal: 0,
                transport: TRANSPORT_PCI,
            },
        ]
    }

    #[test]
    fn a_roster_survives_the_round_trip() {
        let mut page = [0u8; 4096];
        write(&mut page, &sample()).unwrap();
        let r = Roster::read(&page).unwrap();
        assert_eq!(r.len(), 4);
        assert!(!r.is_empty());
        for (i, want) in sample().iter().enumerate() {
            assert_eq!(r.get(i).unwrap(), *want);
        }
        assert_eq!(r.get(4), None, "past the end is None, not the next entry");
        assert_eq!(r.count_on(TRANSPORT_MMIO), 3);
        assert_eq!(r.count_on(TRANSPORT_PCI), 1);
        assert_eq!(r.iter().count(), 4);
    }

    /// The refusal that costs nothing and buys the most: a frame nobody wrote is not a machine
    /// with no disks. Reading it as "zero devices" would be a lie that looks like a fact, and it
    /// is exactly what a mis-wired mapping produces.
    #[test]
    fn a_zeroed_page_is_not_an_empty_roster() {
        let page = [0u8; 4096];
        assert!(Roster::read(&page).is_none());

        // Whereas a roster the kernel deliberately wrote empty reads as empty.
        let mut page = [0u8; 4096];
        write(&mut page, &[]).unwrap();
        let r = Roster::read(&page).unwrap();
        assert!(r.is_empty());
        assert_eq!(r.get(0), None);
    }

    /// A count larger than the page can hold is refused rather than trusted. The reader is a
    /// `no_std` program indexing a mapping; believing this number is how it walks off the end.
    #[test]
    fn a_lying_count_is_refused() {
        let mut page = [0u8; 64]; // room for six entries
        write(&mut page, &sample()).unwrap();
        assert!(Roster::read(&page).is_some());
        page[8..12].copy_from_slice(&7u32.to_le_bytes());
        assert!(Roster::read(&page).is_none());
        // And exactly at capacity is fine, because the boundary is the interesting one.
        page[8..12].copy_from_slice(&6u32.to_le_bytes());
        assert!(Roster::read(&page).is_some());
    }

    #[test]
    fn a_page_too_small_for_its_devices_is_refused_at_write() {
        let mut page = [0u8; HEADER_BYTES + ENTRY_BYTES]; // room for one
        assert_eq!(write(&mut page, &sample()), Err(Full));
        assert_eq!(write(&mut page, &sample()[..1]), Ok(()));
        assert_eq!(capacity_of(0), 0, "a page with no header holds nothing");
        assert_eq!(capacity_of(HEADER_BYTES), 0);
    }

    #[test]
    fn transports_are_named_and_an_unknown_one_is_not_guessed() {
        assert_eq!(
            Device {
                ordinal: 0,
                transport: TRANSPORT_MMIO
            }
            .transport_name(),
            Some("virtio-mmio"),
        );
        assert_eq!(
            Device {
                ordinal: 0,
                transport: TRANSPORT_PCI
            }
            .transport_name(),
            Some("virtio-pci"),
        );
        assert_eq!(
            Device {
                ordinal: 0,
                transport: 99
            }
            .transport_name(),
            None,
        );
    }

    /// The magic is a fixed byte string on disk, not whatever `u64::from_le_bytes` happens to
    /// produce today. Spelling it out is what makes a page written by one build readable by
    /// another built on a different endianness.
    #[test]
    fn the_magic_is_the_bytes_it_says_it_is() {
        let mut page = [0u8; 4096];
        write(&mut page, &[]).unwrap();
        assert_eq!(&page[..8], b"blkrostr");
    }

    /// A page of exactly `HEADER_BYTES` is the smallest roster there is: room for the header,
    /// zero devices, and it must read back as that rather than being refused as short. Milestone
    /// 85's mutation run showed `read`'s length guard could rot from `<` to `<=` unnoticed
    /// because every test used a whole 4096-byte page.
    #[test]
    fn a_header_only_page_is_an_empty_roster_not_a_short_one() {
        let mut page = [0u8; HEADER_BYTES];
        write(&mut page, &[]).unwrap();
        let roster = Roster::read(&page).expect("header-only is a valid roster");
        assert_eq!(roster.iter().count(), 0);
        assert_eq!(capacity_of(HEADER_BYTES), 0);
        // One byte shorter really is short.
        assert_eq!(Roster::read(&page[..HEADER_BYTES - 1]).map(|_| ()), None);
    }
}

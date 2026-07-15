//! An ELF64 loader's front half: parse, validate, and hand back the segments to map.
//!
//! **Pure logic, so it compiles for the host and its tests run in milliseconds** with no
//! emulator (DECISIONS.md §7). Nothing in here knows what a page table is. It answers one
//! question: *what does this file want me to put where, and with what permissions?*
//!
//! Deliberately narrow. We parse **static, little-endian, aarch64, ET_EXEC** binaries and
//! nothing else. No dynamic linking, no relocations, no interpreter, no PIE. Every one of those
//! is a real feature and every one of them is also a way for a file to ask us to do something
//! surprising, and we would rather say "no" in eleven lines than "maybe" in a thousand.
//!
//! See notes/elf.md.

#![no_std]

/// `\x7fELF`.
const MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];

const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const EV_CURRENT: u8 = 1;

/// `e_type`. We accept only this one: a **static executable**, loaded where it says.
const ET_EXEC: u16 = 2;
/// `e_type` for a PIE / shared object. Needs relocation, which we do not do.
const ET_DYN: u16 = 3;

/// `e_machine`.
const EM_AARCH64: u16 = 183;

/// `p_type`: a segment the loader must actually put in memory. The only one we care about.
const PT_LOAD: u32 = 1;

/// `p_flags`.
pub const PF_X: u32 = 1;
pub const PF_W: u32 = 2;
pub const PF_R: u32 = 4;

/// 64 bytes of ELF64 header, then program headers of 56 bytes each.
const EHDR_SIZE: usize = 64;
const PHDR_SIZE: usize = 56;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Smaller than an ELF header. Not even worth looking at.
    TooSmall,
    /// No `\x7fELF`.
    BadMagic,
    /// 32-bit. We do not have a 32-bit anything.
    Not64Bit,
    /// Big-endian. aarch64 can be, and ours is not.
    NotLittleEndian,
    /// `e_version` is not 1.
    BadVersion,
    /// Compiled for some other machine. **This is the one that catches an x86 binary**, and it
    /// catches it *here* rather than as a mystery illegal-instruction fault at EL0.
    NotAarch64,
    /// A PIE or shared object. It expects a dynamic linker to relocate it. We are not one.
    NeedsRelocation,
    /// Not an executable at all (a relocatable object, a core dump).
    NotExecutable,
    /// The program header table runs off the end of the file.
    BadProgramHeaders,
    /// A segment's file contents run off the end of the file.
    ///
    /// **The bounds check that matters.** `p_offset + p_filesz` is attacker-controlled, and a
    /// loader that trusts it reads whatever happens to be after the buffer, and then maps it
    /// into a process.
    SegmentOutOfBounds,
    /// `p_memsz < p_filesz`: the segment claims to occupy less memory than it has bytes.
    SegmentTruncated,
    /// **A segment that is both writable and executable.**
    ///
    /// Refused, and this is the same W^X rule that `paging::Flags` enforces by having no
    /// `writable_and_executable()` constructor. A page that is both is how a buffer overflow
    /// becomes code execution, and an ELF is perfectly capable of *asking* for one.
    WritableAndExecutable,
    /// A segment that is neither readable nor executable. Nothing can ever touch it.
    SegmentUnreachable,
    /// Two segments want the same page.
    ///
    /// A real loader handles this (it is legal, and common when `.text` and `.rodata` share a
    /// page). Ours refuses, because our own linker script page-aligns every segment, so if we
    /// ever see one it means something we did not expect. See the TODO in the kernel's loader.
    SegmentsOverlap,
    /// The entry point is not inside any executable segment. The program cannot start.
    EntryNotExecutable,
    /// **`p_vaddr + p_memsz` overflows.** A crafted segment can name a near-`u64::MAX` memsz to
    /// wrap the address arithmetic; caught here so the entry check and page math cannot overflow.
    AddressOverflow,
    /// More program headers than we will look at. A real static executable has a handful; a huge
    /// count is only good for making the O(n^2) overlap check stall.
    TooManyProgramHeaders,
}

/// One `PT_LOAD` segment: what to map, where, and how.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment<'a> {
    /// The virtual address the program wants this at. Its choice, and we honour it or refuse.
    pub vaddr: u64,

    /// How much **memory** it occupies. May exceed `data.len()`.
    pub memsz: u64,

    /// `PF_R | PF_W | PF_X`.
    pub flags: u32,

    /// The bytes from the file. **`data.len()` is `p_filesz`, which can be less than `memsz`.**
    ///
    /// The difference is `.bss`, and **the loader must zero it**. This is the classic ELF loader
    /// bug: copy `filesz` bytes, forget the tail, and hand the program a `.bss` full of whoever
    /// used that frame last. Our loader zeroes every page before copying, so the tail is free,
    /// but only because we thought about it.
    pub data: &'a [u8],
}

impl Segment<'_> {
    pub fn is_readable(&self) -> bool {
        self.flags & PF_R != 0
    }
    pub fn is_writable(&self) -> bool {
        self.flags & PF_W != 0
    }
    pub fn is_executable(&self) -> bool {
        self.flags & PF_X != 0
    }

    /// The page-aligned range this segment touches: `[start, end)`.
    pub fn page_range(&self, page_size: u64) -> (u64, u64) {
        let start = self.vaddr & !(page_size - 1);
        // Saturating, so a hostile `memsz` cannot overflow this even though `Elf::parse` already
        // rejects `vaddr + memsz` overflow (this type is `pub`, so it must be panic-free alone).
        let end = self
            .vaddr
            .saturating_add(self.memsz)
            .div_ceil(page_size)
            .saturating_mul(page_size);
        (start, end)
    }
}

/// A parsed, **fully validated** ELF64 executable.
///
/// Everything is checked in [`Elf::parse`], not lazily while iterating. A loader that validates
/// as it maps has already mapped half a bad program by the time it finds out.
pub struct Elf<'a> {
    bytes: &'a [u8],
    entry: u64,
    phoff: usize,
    phnum: usize,
    phentsize: usize,
}

impl<'a> Elf<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        if bytes.len() < EHDR_SIZE {
            return Err(Error::TooSmall);
        }

        if bytes[0..4] != MAGIC {
            return Err(Error::BadMagic);
        }
        if bytes[4] != ELFCLASS64 {
            return Err(Error::Not64Bit);
        }
        if bytes[5] != ELFDATA2LSB {
            return Err(Error::NotLittleEndian);
        }
        if bytes[6] != EV_CURRENT {
            return Err(Error::BadVersion);
        }

        let e_type = u16le(bytes, 16);
        let e_machine = u16le(bytes, 18);

        if e_machine != EM_AARCH64 {
            return Err(Error::NotAarch64);
        }
        match e_type {
            ET_EXEC => {}
            ET_DYN => return Err(Error::NeedsRelocation),
            _ => return Err(Error::NotExecutable),
        }

        let entry = u64le(bytes, 24);
        let phoff = u64le(bytes, 32) as usize;
        let phentsize = u16le(bytes, 54) as usize;
        let phnum = u16le(bytes, 56) as usize;

        if phentsize < PHDR_SIZE {
            return Err(Error::BadProgramHeaders);
        }
        // Bound the header count before the O(n^2) overlap check. A legitimate static executable
        // has a few PT_LOAD segments; 65535 headers exist only to make validation stall.
        const MAX_PHNUM: usize = 64;
        if phnum > MAX_PHNUM {
            return Err(Error::TooManyProgramHeaders);
        }

        // The bounds check on the program header table itself. `phoff` and `phnum` come out of
        // the file, so they are hostile input, and `phoff + phnum * phentsize` is exactly the
        // kind of arithmetic that wraps.
        let table_len = phnum.checked_mul(phentsize).ok_or(Error::BadProgramHeaders)?;
        let table_end = phoff.checked_add(table_len).ok_or(Error::BadProgramHeaders)?;
        if table_end > bytes.len() {
            return Err(Error::BadProgramHeaders);
        }

        let elf = Elf {
            bytes,
            entry,
            phoff,
            phnum,
            phentsize,
        };

        elf.validate()?;
        Ok(elf)
    }

    /// Every check, before the caller maps a single page.
    fn validate(&self) -> Result<(), Error> {
        let mut entry_ok = false;

        for i in 0..self.phnum {
            let Some(seg) = self.segment_at(i)? else {
                continue;
            };

            if seg.is_writable() && seg.is_executable() {
                return Err(Error::WritableAndExecutable);
            }
            if !seg.is_readable() && !seg.is_executable() {
                return Err(Error::SegmentUnreachable);
            }

            if seg.is_executable()
                && (self.entry >= seg.vaddr && self.entry < seg.vaddr + seg.memsz)
            {
                entry_ok = true;
            }

            // No two segments may claim the same page. See `Error::SegmentsOverlap`.
            for j in 0..i {
                if let Some(other) = self.segment_at(j)? {
                    let (a0, a1) = seg.page_range(4096);
                    let (b0, b1) = other.page_range(4096);
                    if a0 < b1 && b0 < a1 {
                        return Err(Error::SegmentsOverlap);
                    }
                }
            }
        }

        if !entry_ok {
            return Err(Error::EntryNotExecutable);
        }
        Ok(())
    }

    /// The `i`th program header, if it is a `PT_LOAD`.
    fn segment_at(&self, i: usize) -> Result<Option<Segment<'a>>, Error> {
        let off = self.phoff + i * self.phentsize;
        let ph = &self.bytes[off..off + PHDR_SIZE];

        if u32le(ph, 0) != PT_LOAD {
            return Ok(None);
        }

        let flags = u32le(ph, 4);
        let p_offset = u64le(ph, 8) as usize;
        let vaddr = u64le(ph, 16);
        let filesz = u64le(ph, 32) as usize;
        let memsz = u64le(ph, 40);

        if (memsz as usize) < filesz {
            return Err(Error::SegmentTruncated);
        }

        // **The bounds check.** `p_offset` and `p_filesz` are hostile input.
        let end = p_offset
            .checked_add(filesz)
            .ok_or(Error::SegmentOutOfBounds)?;
        if end > self.bytes.len() {
            return Err(Error::SegmentOutOfBounds);
        }

        // And the VIRTUAL range must not overflow. `p_memsz` is hostile too, and `vaddr + memsz`
        // feeds the entry-in-segment check and `page_range`; a near-u64::MAX memsz would wrap
        // them. With overflow-checks on (the shipping dev profile) that wrap is a PANIC, i.e. a
        // crafted binary halting the kernel, which is exactly what this crate exists to prevent.
        vaddr.checked_add(memsz).ok_or(Error::AddressOverflow)?;

        Ok(Some(Segment {
            vaddr,
            memsz,
            flags,
            data: &self.bytes[p_offset..end],
        }))
    }

    /// Where execution begins. Validated to be inside an executable segment.
    pub fn entry(&self) -> u64 {
        self.entry
    }

    /// The segments to map, in file order.
    pub fn segments(&self) -> impl Iterator<Item = Segment<'a>> + '_ {
        (0..self.phnum).filter_map(|i| self.segment_at(i).ok().flatten())
    }
}

fn u16le(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn u64le(b: &[u8], at: usize) -> u64 {
    let mut v = [0u8; 8];
    v.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::vec;
    use std::vec::Vec;

    /// Build an ELF64 image by hand, so the tests can lie about anything they like.
    ///
    /// **This is the whole reason the parser is a host crate.** Forging a malicious binary is
    /// eleven lines here. Producing one from a real toolchain, getting it into an initrd, and
    /// booting QEMU to watch it be rejected would be a day's work and a slower test.
    struct Builder {
        e_type: u16,
        e_machine: u16,
        class: u8,
        data: u8,
        version: u8,
        magic: [u8; 4],
        entry: u64,
        segments: Vec<(u32, u64, Vec<u8>, u64)>, // flags, vaddr, bytes, memsz
        lie_about_filesz: Option<u64>,
        lie_about_offset: Option<u64>,
    }

    impl Builder {
        fn new() -> Self {
            Builder {
                e_type: ET_EXEC,
                e_machine: EM_AARCH64,
                class: ELFCLASS64,
                data: ELFDATA2LSB,
                version: EV_CURRENT,
                magic: MAGIC,
                entry: 0x40_0000,
                segments: vec![],
                lie_about_filesz: None,
                lie_about_offset: None,
            }
        }

        fn seg(mut self, flags: u32, vaddr: u64, bytes: &[u8], memsz: u64) -> Self {
            self.segments.push((flags, vaddr, bytes.to_vec(), memsz));
            self
        }

        fn build(self) -> Vec<u8> {
            let phnum = self.segments.len();
            let phoff = EHDR_SIZE;
            let mut body_off = EHDR_SIZE + phnum * PHDR_SIZE;

            let mut ehdr = vec![0u8; EHDR_SIZE];
            ehdr[0..4].copy_from_slice(&self.magic);
            ehdr[4] = self.class;
            ehdr[5] = self.data;
            ehdr[6] = self.version;
            ehdr[16..18].copy_from_slice(&self.e_type.to_le_bytes());
            ehdr[18..20].copy_from_slice(&self.e_machine.to_le_bytes());
            ehdr[24..32].copy_from_slice(&self.entry.to_le_bytes());
            ehdr[32..40].copy_from_slice(&(phoff as u64).to_le_bytes());
            ehdr[54..56].copy_from_slice(&(PHDR_SIZE as u16).to_le_bytes());
            ehdr[56..58].copy_from_slice(&(phnum as u16).to_le_bytes());

            let mut phdrs = vec![];
            let mut body = vec![];
            for (flags, vaddr, bytes, memsz) in &self.segments {
                let mut ph = vec![0u8; PHDR_SIZE];
                ph[0..4].copy_from_slice(&PT_LOAD.to_le_bytes());
                ph[4..8].copy_from_slice(&flags.to_le_bytes());
                let off = self.lie_about_offset.unwrap_or(body_off as u64);
                ph[8..16].copy_from_slice(&off.to_le_bytes());
                ph[16..24].copy_from_slice(&vaddr.to_le_bytes());
                let fsz = self.lie_about_filesz.unwrap_or(bytes.len() as u64);
                ph[32..40].copy_from_slice(&fsz.to_le_bytes());
                ph[40..48].copy_from_slice(&memsz.to_le_bytes());
                phdrs.extend_from_slice(&ph);
                body.extend_from_slice(bytes);
                body_off += bytes.len();
            }

            let mut out = ehdr;
            out.extend_from_slice(&phdrs);
            out.extend_from_slice(&body);
            out
        }
    }

    /// The happy path: two segments, code and data, and the entry point lands in the code.
    fn good() -> Vec<u8> {
        Builder::new()
            .seg(PF_R | PF_X, 0x40_0000, &[0xaa; 16], 16)
            .seg(PF_R | PF_W, 0x41_0000, &[0xbb; 8], 4096) // memsz > filesz: .bss
            .build()
    }

    #[test]
    fn a_good_binary_parses() {
        let bytes = good();
        let elf = Elf::parse(&bytes).expect("should parse");

        assert_eq!(elf.entry(), 0x40_0000);

        let segs: Vec<_> = elf.segments().collect();
        assert_eq!(segs.len(), 2);

        assert_eq!(segs[0].vaddr, 0x40_0000);
        assert!(segs[0].is_executable() && !segs[0].is_writable());
        assert_eq!(segs[0].data, &[0xaa; 16]);

        assert!(segs[1].is_writable() && !segs[1].is_executable());
    }

    /// **`memsz > filesz` is `.bss`, and forgetting it is the classic ELF loader bug.**
    ///
    /// The file carries 8 bytes; the program expects 4096, with the rest zeroed. A loader that
    /// copies `filesz` and stops hands the program 4088 bytes of whoever used that frame last.
    #[test]
    fn bss_is_the_difference_between_memsz_and_filesz() {
        let bytes = good();
        let elf = Elf::parse(&bytes).unwrap();
        let data = elf.segments().nth(1).unwrap();

        assert_eq!(data.data.len(), 8, "filesz");
        assert_eq!(data.memsz, 4096, "memsz");
        assert!(
            data.memsz as usize > data.data.len(),
            "the loader must zero {} bytes the file does not contain",
            data.memsz as usize - data.data.len(),
        );
    }

    /// **W^X, refused at the door.**
    ///
    /// An ELF can simply *ask* for a page that is both writable and executable, and a loader
    /// that grants it has handed the program the thing every exploit wants. `paging::Flags` has
    /// no `writable_and_executable()` constructor for the same reason; this is the check that
    /// stops a file talking us into building one.
    #[test]
    fn a_writable_executable_segment_is_refused() {
        let bytes = Builder::new()
            .seg(PF_R | PF_W | PF_X, 0x40_0000, &[0xaa; 16], 16)
            .build();

        assert_eq!(
            Elf::parse(&bytes).err(),
            Some(Error::WritableAndExecutable),
        );
    }

    /// A segment whose contents run off the end of the file.
    ///
    /// `p_offset` and `p_filesz` are attacker-controlled. A loader that trusts them reads
    /// whatever is after the buffer and then **maps it into a process**.
    #[test]
    fn a_segment_that_runs_off_the_end_is_refused() {
        // memsz is a match for the lie, so `SegmentTruncated` does not fire first and we
        // genuinely exercise the bounds check on the FILE.
        let mut b = Builder::new().seg(PF_R | PF_X, 0x40_0000, &[0xaa; 16], 0x1000_0000);
        b.lie_about_filesz = Some(0x1000_0000);
        assert_eq!(Elf::parse(&b.build()).err(), Some(Error::SegmentOutOfBounds));
    }

    #[test]
    fn an_offset_that_overflows_is_refused() {
        let mut b = Builder::new().seg(PF_R | PF_X, 0x40_0000, &[0xaa; 16], 16);
        b.lie_about_offset = Some(u64::MAX - 3); // p_offset + p_filesz wraps
        assert_eq!(Elf::parse(&b.build()).err(), Some(Error::SegmentOutOfBounds));
    }

    #[test]
    fn memsz_smaller_than_filesz_is_refused() {
        let bytes = Builder::new()
            .seg(PF_R | PF_X, 0x40_0000, &[0xaa; 16], 4) // memsz 4 < filesz 16
            .build();
        assert_eq!(Elf::parse(&bytes).err(), Some(Error::SegmentTruncated));
    }

    /// **An x86 binary, caught here rather than as an illegal instruction at EL0.**
    #[test]
    fn a_binary_for_another_machine_is_refused() {
        let mut b = Builder::new().seg(PF_R | PF_X, 0x40_0000, &[0xaa; 16], 16);
        b.e_machine = 62; // EM_X86_64
        assert_eq!(Elf::parse(&b.build()).err(), Some(Error::NotAarch64));
    }

    /// A PIE expects a dynamic linker to relocate it. We are not one, and loading it as if we
    /// were means jumping to an address that means nothing.
    #[test]
    fn a_position_independent_executable_is_refused() {
        let mut b = Builder::new().seg(PF_R | PF_X, 0x40_0000, &[0xaa; 16], 16);
        b.e_type = ET_DYN;
        assert_eq!(Elf::parse(&b.build()).err(), Some(Error::NeedsRelocation));
    }

    /// The entry point must be somewhere we can actually execute.
    #[test]
    fn an_entry_point_outside_every_executable_segment_is_refused() {
        let mut b = Builder::new().seg(PF_R | PF_X, 0x40_0000, &[0xaa; 16], 16);
        b.entry = 0x41_0000; // not in the code segment
        assert_eq!(Elf::parse(&b.build()).err(), Some(Error::EntryNotExecutable));
    }

    /// An entry point inside a segment that is readable but NOT executable.
    #[test]
    fn an_entry_point_in_a_data_segment_is_refused() {
        let mut b = Builder::new().seg(PF_R | PF_W, 0x40_0000, &[0xaa; 16], 16);
        b.entry = 0x40_0000;
        assert_eq!(Elf::parse(&b.build()).err(), Some(Error::EntryNotExecutable));
    }

    #[test]
    fn two_segments_in_the_same_page_are_refused() {
        let bytes = Builder::new()
            .seg(PF_R | PF_X, 0x40_0000, &[0xaa; 16], 16)
            .seg(PF_R | PF_W, 0x40_0800, &[0xbb; 16], 16) // same 4 KiB page
            .build();
        assert_eq!(Elf::parse(&bytes).err(), Some(Error::SegmentsOverlap));
    }

    #[test]
    fn a_segment_whose_address_range_overflows_is_refused() {
        // filesz small (passes the file-bounds check), memsz enormous (passes memsz >= filesz), so
        // only the vaddr+memsz overflow guard stands between this and a kernel panic.
        let mut b = Builder::new().seg(PF_R | PF_X, 0x40_0000, &[0xaa; 16], u64::MAX);
        b.entry = 0x40_0000;
        // Must return an Err, and crucially must NOT panic on the overflow.
        assert_eq!(Elf::parse(&b.build()).err(), Some(Error::AddressOverflow));
    }

    #[test]
    fn too_many_program_headers_are_refused() {
        let mut b = Builder::new();
        for _ in 0..65 {
            b = b.seg(PF_R | PF_X, 0x40_0000, &[0xaa; 8], 8);
        }
        assert_eq!(Elf::parse(&b.build()).err(), Some(Error::TooManyProgramHeaders));
    }

    #[test]
    fn junk_is_refused() {
        assert_eq!(Elf::parse(&[]).err(), Some(Error::TooSmall));
        assert_eq!(Elf::parse(&[0u8; 64]).err(), Some(Error::BadMagic));

        let mut b = Builder::new().seg(PF_R | PF_X, 0x40_0000, &[0xaa; 16], 16);
        b.class = 1; // ELFCLASS32
        assert_eq!(Elf::parse(&b.build()).err(), Some(Error::Not64Bit));

        let mut b = Builder::new().seg(PF_R | PF_X, 0x40_0000, &[0xaa; 16], 16);
        b.data = 2; // ELFDATA2MSB
        assert_eq!(Elf::parse(&b.build()).err(), Some(Error::NotLittleEndian));
    }

    /// A shell script, a JPEG, and the kernel's own flat image are all not-an-ELF.
    #[test]
    fn a_file_that_is_not_an_elf_at_all_is_refused() {
        assert_eq!(
            Elf::parse(b"#!/bin/sh\necho hello\n#####################################################").err(),
            Some(Error::BadMagic),
        );
    }

    #[test]
    fn page_range_covers_the_whole_segment() {
        let seg = Segment {
            vaddr: 0x40_0800,
            memsz: 0x900,
            flags: PF_R,
            data: &[],
        };
        // 0x400800..0x401100 spans two pages: 0x400000 and 0x401000.
        assert_eq!(seg.page_range(4096), (0x40_0000, 0x40_2000));
    }
}

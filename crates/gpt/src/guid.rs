//! GUIDs, the "G" in GPT, and the partition types worth recognising by name.
//!
//! # The mixed-endian trap
//!
//! A GUID is 16 bytes and everybody writes it as `C12A7328-F81F-11D2-BA4B-00A0C93EC93B`. The trap is
//! that **the five groups are not stored the same way**. The first three are integers and go on disk
//! little-endian; the last two are byte strings and go on disk in the order written. So the bytes of
//! that GUID are:
//!
//! ```text
//!   28 73 2A C1  1F F8  D2 11  BA 4B  00 A0 C9 3E C9 3B
//!   \--------/   \---/  \---/  \---/  \-----------------/
//!    u32 LE       u16 LE u16 LE  as written
//! ```
//!
//! This is not a GPT quirk, it is how Microsoft's `GUID` struct is laid out and GPT inherited it.
//! Getting it wrong gives you a GUID that looks plausible, matches nothing, and is byte-reversed in
//! three places out of five. [`Guid`] stores the on-disk bytes and does the swapping in exactly two
//! functions, so there is one place for the mistake to live and it is covered by a proof.
//!
//! # Type GUIDs versus unique GUIDs
//!
//! Every entry carries two. The **type** GUID says what the partition is for and is a shared
//! constant ([`types`]); the **unique** GUID names this particular partition and is generated once,
//! never reused. Confusing them is how you get two partitions the OS thinks are the same one.

/// A 128-bit GUID, stored in the byte order it has on disk.
///
/// Construct one from its printed fields with [`Guid::from_fields`] (const, so it works for the
/// constants in [`types`]) or from disk bytes with [`Guid::from_bytes`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Guid([u8; 16]);

impl Guid {
    /// The all-zero GUID, which in a partition entry's type field means "this entry is unused".
    /// The one GUID with a meaning rather than an identity.
    pub const ZERO: Guid = Guid([0; 16]);

    /// Build a GUID from the five printed groups, doing the mixed-endian encoding.
    ///
    /// `C12A7328-F81F-11D2-BA4B-00A0C93EC93B` is
    /// `from_fields(0xC12A7328, 0xF81F, 0x11D2, [0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B])`.
    pub const fn from_fields(a: u32, b: u16, c: u16, rest: [u8; 8]) -> Guid {
        let a = a.to_le_bytes();
        let b = b.to_le_bytes();
        let c = c.to_le_bytes();
        Guid([
            a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], rest[0], rest[1], rest[2], rest[3],
            rest[4], rest[5], rest[6], rest[7],
        ])
    }

    /// Wrap 16 bytes read from a disk. Every bit pattern is a GUID, so this cannot fail.
    pub const fn from_bytes(bytes: [u8; 16]) -> Guid {
        Guid(bytes)
    }

    /// The 16 bytes as they appear on disk.
    pub const fn to_bytes(self) -> [u8; 16] {
        self.0
    }

    /// Stamp 16 random bytes into an RFC 9562 version-4 GUID.
    ///
    /// **This crate has no randomness and this function does not invent any**: the caller brings the
    /// bytes, from a source that is genuinely unpredictable, and all this does is set the six bits
    /// the format reserves. On nife the caller is `disk_partitioner`, which holds an entropy
    /// endpoint; a GUID built from a counter would be unique on one disk and collide with every
    /// other machine's, which is the failure the format's uniqueness rule exists to prevent.
    ///
    /// Six bits are spent: four for the version (`4`) and two for the variant (`0b10`). They land in
    /// **printed** positions, so this is the mixed-endian rule again: the version nibble is the high
    /// nibble of the third group, which is stored little-endian and therefore lives in on-disk byte
    /// 7; the variant bits are the top of the fourth group, which is stored as written, so on-disk
    /// byte 8. Setting them at the wrong offsets produces a GUID that is still unique and reads as
    /// some other version, which nothing would ever catch.
    ///
    /// The remaining 122 bits are the caller's bytes, unmodified. A partition GUID does not have to
    /// be a v4 UUID (the spec asks only that it be unique), but every real tool writes one, and a
    /// disk this OS partitions should not be the odd one out under `sgdisk -i`.
    pub const fn v4_from_random(mut bytes: [u8; 16]) -> Guid {
        bytes[7] = (bytes[7] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        Guid(bytes)
    }

    /// True for the all-zero GUID. See [`Guid::ZERO`].
    pub const fn is_zero(self) -> bool {
        let mut i = 0;
        while i < 16 {
            if self.0[i] != 0 {
                return false;
            }
            i += 1;
        }
        true
    }

    /// The canonical `XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX` form, uppercase, as ASCII.
    ///
    /// Returns a fixed-size buffer rather than a `String` because this crate allocates nothing; the
    /// `Display` impl below is a thin wrapper for anywhere a formatter is available.
    pub fn to_ascii(self) -> [u8; 36] {
        const HEX: [u8; 16] = *b"0123456789ABCDEF";
        // The printed order, group by group: the first three groups are byte-reversed relative to
        // the storage order, the last two are not. This table is the mixed-endian rule made
        // explicit, so the loop below has no special cases.
        const ORDER: [usize; 16] = [3, 2, 1, 0, 5, 4, 7, 6, 8, 9, 10, 11, 12, 13, 14, 15];

        let mut out = [b'-'; 36];
        let mut at = 0;
        for (n, &src) in ORDER.iter().enumerate() {
            if n == 4 || n == 6 || n == 8 || n == 10 {
                at += 1; // leave the dash this position was initialised with
            }
            out[at] = HEX[(self.0[src] >> 4) as usize];
            out[at + 1] = HEX[(self.0[src] & 0xf) as usize];
            at += 2;
        }
        out
    }

    /// Parse the canonical form, accepting either case. `None` if it is not 36 characters with
    /// dashes in the four right places and hex everywhere else.
    ///
    /// Here for a future `mkpart` that takes a `--typecode` on a command line. Nothing on the parse
    /// path uses it: disks carry bytes, not text.
    pub fn try_from_ascii(text: &[u8]) -> Option<Guid> {
        const ORDER: [usize; 16] = [3, 2, 1, 0, 5, 4, 7, 6, 8, 9, 10, 11, 12, 13, 14, 15];
        if text.len() != 36 {
            return None;
        }
        for &dash in &[8usize, 13, 18, 23] {
            if text[dash] != b'-' {
                return None;
            }
        }
        let mut bytes = [0u8; 16];
        let mut at = 0;
        for (n, &dst) in ORDER.iter().enumerate() {
            if n == 4 || n == 6 || n == 8 || n == 10 {
                at += 1;
            }
            bytes[dst] = (nibble(text[at])? << 4) | nibble(text[at + 1])?;
            at += 2;
        }
        Some(Guid(bytes))
    }
}

fn nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

impl core::fmt::Display for Guid {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let ascii = self.to_ascii();
        // to_ascii emits only hex digits and dashes, so this is always valid UTF-8.
        f.write_str(core::str::from_utf8(&ascii).unwrap_or("<guid>"))
    }
}

/// `Debug` prints the same canonical form as `Display`. The default derive would print sixteen
/// decimal bytes in storage order, which is the one representation nobody can match against a
/// `sgdisk -i` listing.
impl core::fmt::Debug for Guid {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Display::fmt(self, f)
    }
}

/// Partition **type** GUIDs, and a name for each.
///
/// Not an attempt at the full registry, which runs to several hundred entries and is a data problem
/// rather than a kernel problem. These are the ones that answer a question this OS actually asks:
/// which partition do I boot from, which one holds a Linux filesystem, which one is ours, and (for
/// the recovery story in milestone 57) what are the two things a Mac will have put on a disk.
///
/// Every value here was read back out of `sgdisk` on the machine rather than typed from memory, and
/// four of them are pinned by the committed fixtures.
pub mod types {
    use super::Guid;

    /// An unused entry. The type GUID is what marks an entry live, not the LBA fields.
    pub const UNUSED: Guid = Guid::ZERO;

    /// EFI System Partition: the FAT volume firmware loads a bootloader from.
    pub const EFI_SYSTEM: Guid = Guid::from_fields(
        0xC12A_7328,
        0xF81F,
        0x11D2,
        [0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B],
    );

    /// BIOS boot partition, where GRUB puts its core image on a legacy-booted GPT disk. The GUID
    /// spells `Hah!IdontNeedEFI` in ASCII, which is the only joke in the UEFI ecosystem.
    pub const BIOS_BOOT: Guid = Guid::from_fields(
        0x2168_6148,
        0x6449,
        0x6E6F,
        [0x74, 0x4E, 0x65, 0x65, 0x64, 0x45, 0x46, 0x49],
    );

    /// Microsoft basic data. Also what macOS's `diskutil` labels a FAT partition it creates, which
    /// is why the Apple fixture carries it.
    pub const MICROSOFT_BASIC_DATA: Guid = Guid::from_fields(
        0xEBD0_A0A2,
        0xB9E5,
        0x4433,
        [0x87, 0xC0, 0x68, 0xB6, 0xB7, 0x26, 0x99, 0xC7],
    );

    /// Linux filesystem data, `sgdisk` type code `8300`. The generic "there is a Linux filesystem
    /// here" marker and by far the most common type on a Linux disk.
    pub const LINUX_FILESYSTEM: Guid = Guid::from_fields(
        0x0FC6_3DAF,
        0x8483,
        0x4772,
        [0x8E, 0x79, 0x3D, 0x69, 0xD8, 0x47, 0x7D, 0xE4],
    );

    /// Linux swap.
    pub const LINUX_SWAP: Guid = Guid::from_fields(
        0x0657_FD6D,
        0xA4AB,
        0x43C4,
        [0x84, 0xE5, 0x09, 0x33, 0xC8, 0x4B, 0x4F, 0x4F],
    );

    /// Linux root filesystem, arm64. From the Discoverable Partitions Specification, which is how a
    /// systemd machine finds its root without a kernel command line. Here because arm64 is the
    /// architecture this OS runs on and a disk shared with Linux may carry one.
    pub const LINUX_ROOT_ARM64: Guid = Guid::from_fields(
        0xB921_B045,
        0x1DF0,
        0x41C3,
        [0xAF, 0x44, 0x4C, 0x6F, 0x28, 0x0D, 0x3F, 0xAE],
    );

    /// LUKS, Linux's encrypted-volume container. Recognised so that a tool can say "encrypted, and
    /// this crate does not open it" instead of reporting an unreadable filesystem.
    pub const LINUX_LUKS: Guid = Guid::from_fields(
        0xCA7D_7CCB,
        0x63ED,
        0x4C53,
        [0x86, 0x1C, 0x17, 0x42, 0x53, 0x60, 0x59, 0xCC],
    );

    /// Apple HFS+.
    pub const APPLE_HFS_PLUS: Guid = Guid::from_fields(
        0x4846_5300,
        0x0000,
        0x11AA,
        [0xAA, 0x11, 0x00, 0x30, 0x65, 0x43, 0xEC, 0xAC],
    );

    /// Apple APFS, which is what any Mac disk made since 2017 is.
    pub const APPLE_APFS: Guid = Guid::from_fields(
        0x7C34_57EF,
        0x0000,
        0x11AA,
        [0xAA, 0x11, 0x00, 0x30, 0x65, 0x43, 0xEC, 0xAC],
    );

    /// **A nife data partition**, holding a RedoxFS volume (DECISIONS §34).
    ///
    /// A random version-4 GUID, generated once on 2026-07-30 and fixed forever. It has to be random:
    /// a type GUID's whole job is to not collide with anybody else's, and there is no registry to
    /// ask. **Never change this value.** A disk written by one release and read by another has only
    /// this number to agree on, and the recovery story (milestone 57's "the board is dead, can I get
    /// my data") depends on a `sgdisk -p` five years from now still showing it.
    pub const NIFE_DATA: Guid = Guid::from_fields(
        0xEC5C_C08B,
        0xD749,
        0x4434,
        [0xAC, 0x38, 0xA2, 0x74, 0xC5, 0x03, 0x85, 0xBA],
    );

    /// A short name for a type GUID, or `None` for one this crate does not recognise.
    ///
    /// Deliberately returns `None` rather than a placeholder: a partition tool should print the raw
    /// GUID for an unknown type, because that is the string a person can look up.
    pub fn name(guid: Guid) -> Option<&'static str> {
        Some(match guid {
            UNUSED => "unused",
            EFI_SYSTEM => "EFI system",
            BIOS_BOOT => "BIOS boot",
            MICROSOFT_BASIC_DATA => "Microsoft basic data",
            LINUX_FILESYSTEM => "Linux filesystem",
            LINUX_SWAP => "Linux swap",
            LINUX_ROOT_ARM64 => "Linux root (arm64)",
            LINUX_LUKS => "Linux LUKS",
            APPLE_HFS_PLUS => "Apple HFS+",
            APPLE_APFS => "Apple APFS",
            NIFE_DATA => "nife data",
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The EFI System Partition GUID, spelled out byte by byte, because this is the one place the
    /// mixed-endian rule can be wrong without anything else noticing. Cross-checked against the
    /// committed `sgdisk` fixture, where these sixteen bytes sit at offset 0 of entry 0.
    #[test]
    fn the_mixed_endian_layout_is_the_one_on_disk() {
        assert_eq!(
            types::EFI_SYSTEM.to_bytes(),
            [
                0x28, 0x73, 0x2A, 0xC1, // C12A7328, little-endian
                0x1F, 0xF8, // F81F, little-endian
                0xD2, 0x11, // 11D2, little-endian
                0xBA, 0x4B, // BA4B, as written
                0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B, // node, as written
            ]
        );
    }

    #[test]
    fn printing_undoes_the_swapping() {
        assert_eq!(
            &types::EFI_SYSTEM.to_ascii(),
            b"C12A7328-F81F-11D2-BA4B-00A0C93EC93B"
        );
        assert_eq!(
            &types::NIFE_DATA.to_ascii(),
            b"EC5CC08B-D749-4434-AC38-A274C50385BA"
        );
        assert_eq!(
            &Guid::ZERO.to_ascii(),
            b"00000000-0000-0000-0000-000000000000"
        );
    }

    #[test]
    fn text_round_trips_and_junk_is_refused() {
        for g in [types::EFI_SYSTEM, types::NIFE_DATA, Guid::ZERO] {
            assert_eq!(Guid::try_from_ascii(&g.to_ascii()), Some(g));
        }
        // Lowercase in, canonical uppercase out.
        let lower = b"c12a7328-f81f-11d2-ba4b-00a0c93ec93b";
        assert_eq!(Guid::try_from_ascii(lower), Some(types::EFI_SYSTEM));

        assert_eq!(Guid::try_from_ascii(b"too short"), None);
        assert_eq!(
            Guid::try_from_ascii(b"C12A7328+F81F-11D2-BA4B-00A0C93EC93B"),
            None,
            "a dash in the wrong place"
        );
        assert_eq!(
            Guid::try_from_ascii(b"G12A7328-F81F-11D2-BA4B-00A0C93EC93B"),
            None,
            "G is not hex"
        );
    }

    /// The six reserved bits land where a **reader** looks for them, which is the only thing about
    /// `v4_from_random` that can be wrong: at any other offsets the GUID is still unique, still
    /// unpredictable, and reads as some other UUID version forever.
    ///
    /// So the check is the printed form, not the bytes: position 14 is the version nibble and
    /// position 19 is the variant, and both are where `sgdisk -i` and `uuidgen` read them.
    #[test]
    fn stamping_random_bytes_gives_a_version_4_guid() {
        for fill in [0x00u8, 0xff, 0x5a, 0xa5] {
            let g = Guid::v4_from_random([fill; 16]);
            let text = g.to_ascii();
            assert_eq!(text[14], b'4', "version nibble, from a {fill:#04x} fill");
            assert!(
                matches!(text[19], b'8' | b'9' | b'A' | b'B'),
                "variant bits, from a {fill:#04x} fill: got {}",
                text[19] as char,
            );
        }
    }

    /// The other 122 bits are the caller's, untouched. A "stamp" that quietly normalised more than
    /// the format reserves would be throwing away entropy the caller paid a round trip for.
    #[test]
    fn stamping_keeps_every_bit_it_does_not_reserve() {
        let raw = [
            0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0xfe, 0xdc, 0xba, 0x98, 0x76, 0x54,
            0x32, 0x10,
        ];
        let out = Guid::v4_from_random(raw).to_bytes();
        for (i, (&before, &after)) in raw.iter().zip(out.iter()).enumerate() {
            match i {
                7 => assert_eq!(after, (before & 0x0f) | 0x40),
                8 => assert_eq!(after, (before & 0x3f) | 0x80),
                _ => assert_eq!(after, before, "byte {i} was modified"),
            }
        }
    }

    #[test]
    fn only_the_zero_guid_is_zero() {
        assert!(Guid::ZERO.is_zero());
        assert!(!types::LINUX_FILESYSTEM.is_zero());
        // One bit set, in the last group, which the naive "is the first word zero" check misses.
        assert!(!Guid::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]).is_zero());
    }

    #[test]
    fn every_named_type_is_distinct() {
        let all = [
            types::EFI_SYSTEM,
            types::BIOS_BOOT,
            types::MICROSOFT_BASIC_DATA,
            types::LINUX_FILESYSTEM,
            types::LINUX_SWAP,
            types::LINUX_ROOT_ARM64,
            types::LINUX_LUKS,
            types::APPLE_HFS_PLUS,
            types::APPLE_APFS,
            types::NIFE_DATA,
        ];
        for (i, a) in all.iter().enumerate() {
            assert!(types::name(*a).is_some(), "{a} has no name");
            for b in &all[i + 1..] {
                assert_ne!(a, b, "two type constants collide");
            }
        }
        assert_eq!(types::name(Guid::from_bytes([0xAB; 16])), None);
    }
}

#![no_std]
//! **What this machine advertises, as a document a person edits** (milestone 55's responder).
//!
//! `mdns_proto` takes the whole advertisement as data and hard-codes none of it, deliberately: the
//! disk names are a family's, the flag values are whatever a Mac was measured to accept, and the
//! model string is a knob the reference implementation sets independently of its Samba config. That
//! decision has a consequence somebody has to carry, which is that the values have to come from
//! *somewhere*, and a `const DISKS` inside the responder would put them back where the crate
//! refused to keep them. This is the somewhere: one text document, one parser, host-tested.
//!
//! The format is deliberately dull, because a configuration format is a thing a person edits at
//! two in the morning when their backups have stopped:
//!
//! ```text
//! # comments run to the end of the line
//! host      = GL-BE9300
//! smb-port  = 445
//! model     = MacSamba
//! sys-flags = 0x100
//! disk      = corinne 0x82
//! disk      = chris   0x82
//! ```
//!
//! Every key is required except `disk`, which repeats: one line per Time Machine share, in the
//! order they become `dk0=`, `dk1=` and so on inside the single `_adisk._tcp` TXT record. **One
//! advertisement carries every disk**; a file with two `disk` lines describes one server offering
//! two backup targets, which is what the reference router does, and never two servers.
//!
//! **The IP address is not in the document**, and that omission is the design rather than an
//! oversight. Which address this machine holds is a fact the DHCP lease establishes at run time,
//! so [`Config::advertisement`] takes it as an argument and whoever knows it supplies it. A
//! configuration file that named an address would be a file that goes stale the first time the
//! network changes.
//!
//! # EXAMPLES
//!
//! The reference document (the measured values from calef's router, 2026-08-15) parsed into the
//! advertisement a responder puts on the wire:
//!
//! ```
//! # let doc = "host = GL-BE9300\nsmb-port = 445\nmodel = MacSamba\nsys-flags = 0x100\ndisk = corinne 0x82\ndisk = chris 0x82\n";
//! let config = mdns_config::Config::parse(doc).expect("the document parses");
//! let adv = config.advertisement(Some([192, 168, 8, 1]));
//! assert_eq!(adv.host, "GL-BE9300");
//! assert_eq!(adv.smb_port, 445);
//! assert_eq!(adv.disks.len(), 2);
//! assert_eq!(adv.disks[0].volume, "corinne");
//! assert_eq!(adv.disks[1].flags, "0x82");
//! ```
//!
//! # BUGS
//!
//! - **The document is compiled into the responder rather than read from disk.** The program
//!   `include_str!`s it, because a program that read its configuration from a file would need a
//!   file capability, and wiring one into the QEMU gate needs a fixture the FS-side crates do not
//!   yet carry. So editing the advertisement today means a rebuild. The format, the parser and
//!   every error in it are unaffected by that: moving the document onto the filesystem is a change
//!   of delivery, not of shape, and the responder's `BUGS` section names it as the next step.
//! - **Values are checked for length and emptiness, not for meaning.** A `disk` line whose volume
//!   contains a comma or an `=` will produce a TXT entry a client parses differently than intended
//!   (DNS-SD's `key=value` convention gives both characters a job), and nothing here refuses it.
//!   The reference values contain neither. Refusing them would need a rule about what a share may
//!   be called, which is a question for whoever names shares.
//! - **`sys-flags` and a disk's flags are opaque strings**, copied to the wire as written. Their
//!   bits are not decoded anywhere in this tree (notes/mdns.md), so a typo in one is a typo a Mac
//!   sees.
//! - **No IPv6.** [`Config::advertisement`] takes an optional IPv4 address because
//!   `mdns_proto::Advertisement` carries one; the reference emits AAAA as well.
//!
//! Name: unrecorded, and explicitly **provisional**, minted by milestone 55's responder lane on
//! 2026-08-16. `mdns_advertisement` was the alternative and reads better beside
//! `mdns_proto::Advertisement`; `mdns_config` was taken because what the crate holds is the whole
//! of the responder's configuration and the type it produces is only part of it. Not put to calef.

use mdns_proto::{Advertisement, Disk};

/// How many `disk` lines one document may carry. Eight, against a measured two: the count is a
/// household's shares, and the array is fixed because this crate allocates nothing.
pub const MAX_DISKS: usize = 8;

/// The longest a host label or a volume name may be. A DNS label's own limit
/// ([`mdns_proto::MAX_LABEL_LEN`]), applied at parse time so an over-long name is refused with a
/// line number rather than as an `Overflow` from the emitter with nothing to point at.
pub const MAX_LABEL: usize = mdns_proto::MAX_LABEL_LEN;

/// What can be wrong with a document. Every variant carries the **1-based line number**, because
/// the whole reason a configuration file is a file is that a person is going to have to find the
/// mistake in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// A line that is neither blank, nor a comment, nor `key = value`.
    Malformed(usize),
    /// A `key = value` line whose key is not one this format defines.
    UnknownKey(usize),
    /// A key that appears twice (except `disk`, which is meant to).
    Repeated(usize),
    /// A `disk` line that is not `<volume> <flags>`.
    MalformedDisk(usize),
    /// More `disk` lines than [`MAX_DISKS`].
    TooManyDisks(usize),
    /// `smb-port` was not a number in `1..=65535`.
    BadPort(usize),
    /// A value that is empty, or longer than [`MAX_LABEL`].
    BadValue(usize),
    /// A required key that no line supplied. Carries the key rather than a line, since the whole
    /// document is where it is missing from.
    Missing(&'static str),
}

/// A parsed document. Borrows every string from the document itself, so parsing allocates nothing
/// and a `no_std` responder can hold one on its stack.
#[derive(Debug, Clone, Copy)]
pub struct Config<'a> {
    host: &'a str,
    smb_port: u16,
    model: &'a str,
    sys_flags: &'a str,
    disks: [Disk<'a>; MAX_DISKS],
    n_disks: usize,
}

impl<'a> Config<'a> {
    /// Parse a document. Fails on the first thing wrong with it, with the line number.
    pub fn parse(doc: &'a str) -> Result<Config<'a>, Error> {
        let mut host = None;
        let mut smb_port = None;
        let mut model = None;
        let mut sys_flags = None;
        let mut disks = [Disk {
            volume: "",
            flags: "",
        }; MAX_DISKS];
        let mut n_disks = 0;

        for (i, raw) in doc.lines().enumerate() {
            let no = i + 1;
            let line = strip_comment(raw).trim();
            if line.is_empty() {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                return Err(Error::Malformed(no));
            };
            let key = key.trim();
            let value = value.trim();
            match key {
                "host" => set_once(&mut host, checked(value, no)?, no)?,
                "model" => set_once(&mut model, checked(value, no)?, no)?,
                "sys-flags" => set_once(&mut sys_flags, checked(value, no)?, no)?,
                "smb-port" => {
                    let port: u16 = value.parse().map_err(|_| Error::BadPort(no))?;
                    if port == 0 {
                        return Err(Error::BadPort(no));
                    }
                    set_once(&mut smb_port, port, no)?;
                }
                "disk" => {
                    if n_disks == MAX_DISKS {
                        return Err(Error::TooManyDisks(no));
                    }
                    // `<volume> <flags>`, split on the first run of whitespace. A volume name with a
                    // space in it is out of reach of this format, which is recorded rather than
                    // solved: the reference names are user names.
                    let mut parts = value.split_whitespace();
                    let (Some(volume), Some(flags)) = (parts.next(), parts.next()) else {
                        return Err(Error::MalformedDisk(no));
                    };
                    if parts.next().is_some() {
                        return Err(Error::MalformedDisk(no));
                    }
                    disks[n_disks] = Disk {
                        volume: checked(volume, no)?,
                        flags: checked(flags, no)?,
                    };
                    n_disks += 1;
                }
                _ => return Err(Error::UnknownKey(no)),
            }
        }

        Ok(Config {
            host: host.ok_or(Error::Missing("host"))?,
            smb_port: smb_port.ok_or(Error::Missing("smb-port"))?,
            model: model.ok_or(Error::Missing("model"))?,
            sys_flags: sys_flags.ok_or(Error::Missing("sys-flags"))?,
            disks,
            n_disks,
        })
    }

    /// The advertisement this document describes, at the address this machine turned out to hold.
    ///
    /// `ipv4` is the run-time half: `None` emits no A record, which is what a responder that does
    /// not know its own address must do rather than guessing one.
    pub fn advertisement(&self, ipv4: Option<[u8; 4]>) -> Advertisement<'_> {
        Advertisement {
            host: self.host,
            smb_port: self.smb_port,
            disks: &self.disks[..self.n_disks],
            sys_flags: self.sys_flags,
            model: self.model,
            ipv4,
        }
    }

    /// How many disks the document declared. The responder prints it, because "two disks" is the
    /// one fact about this configuration a person can check against the Time Machine UI.
    pub fn disk_count(&self) -> usize {
        self.n_disks
    }
}

/// Everything from `#` on is a comment. There is no escape for a literal `#`, and none is needed:
/// no value in this format contains one.
fn strip_comment(line: &str) -> &str {
    match line.split_once('#') {
        Some((before, _)) => before,
        None => line,
    }
}

fn checked(value: &str, line: usize) -> Result<&str, Error> {
    if value.is_empty() || value.len() > MAX_LABEL {
        return Err(Error::BadValue(line));
    }
    Ok(value)
}

fn set_once<T>(slot: &mut Option<T>, value: T, line: usize) -> Result<(), Error> {
    if slot.is_some() {
        return Err(Error::Repeated(line));
    }
    *slot = Some(value);
    Ok(())
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::string::String;

    use super::*;

    /// The document the responder ships with, so the tests below and the program read literally the
    /// same bytes. If this file moves, both move together.
    const REFERENCE: &str = include_str!("../../../user/mdns_responder.conf");

    /// **The shipped document parses, and says what the router said.** This is the test that makes
    /// the file a specification rather than a comment: the values were measured off calef's working
    /// Time Machine server (notes/mdns.md), and a typo in the document is a Mac that does not list
    /// the share, which nothing else in the tree would catch until somebody's backup failed.
    #[test]
    fn the_shipped_document_is_the_measured_reference() {
        let c = Config::parse(REFERENCE).expect("the shipped document must parse");
        let adv = c.advertisement(None);
        assert_eq!(adv.host, "GL-BE9300");
        assert_eq!(adv.smb_port, 445);
        assert_eq!(adv.model, "MacSamba");
        assert_eq!(adv.sys_flags, "0x100");
        assert_eq!(adv.disks.len(), 2, "two disks is the measured truth");
        assert_eq!(adv.disks[0].volume, "corinne");
        assert_eq!(adv.disks[0].flags, "0x82");
        assert_eq!(adv.disks[1].volume, "chris");
        assert_eq!(adv.disks[1].flags, "0x82");
    }

    /// **The document survives the round trip to the wire**: parsed, handed to `mdns_proto`, and
    /// the bytes that come out carry the disk names in `dkN=` order. The two crates agree about
    /// what a `Disk` means, which is the only thing joining them.
    #[test]
    fn the_disks_reach_the_adisk_txt_record_in_order() {
        let c = Config::parse(REFERENCE).unwrap();
        let adv = c.advertisement(None);
        let mut out = [0u8; 512];
        let n = mdns_proto::adisk_txt_rdata(&adv, &mut out).unwrap();
        let txt = &out[..n];
        // Length-prefixed strings; check each entry rather than searching the blob, so an entry
        // that ran into its neighbour would fail.
        let mut at = 0;
        let mut entries: [&[u8]; 4] = [b""; 4];
        let mut k = 0;
        while at < n {
            let len = txt[at] as usize;
            entries[k] = &txt[at + 1..at + 1 + len];
            k += 1;
            at += 1 + len;
        }
        assert_eq!(k, 3, "two disks and the sys entry");
        assert_eq!(entries[0], b"dk0=adVN=corinne,adVF=0x82");
        assert_eq!(entries[1], b"dk1=adVN=chris,adVF=0x82");
        assert_eq!(entries[2], b"sys=adVF=0x100");
    }

    #[test]
    fn comments_and_blank_lines_and_spacing_are_ignored() {
        let doc = "\n# a comment\n\n  host   =   name  # trailing\nsmb-port=445\nmodel=m\nsys-flags=0x1\n";
        let c = Config::parse(doc).unwrap();
        assert_eq!(c.advertisement(None).host, "name");
        assert_eq!(c.advertisement(None).smb_port, 445);
    }

    /// A document with no `disk` line is legal and advertises no backup disk: a server that offers
    /// `_smb._tcp` and no Time Machine target is a real configuration, and refusing it here would
    /// be this crate inventing a policy.
    #[test]
    fn a_document_with_no_disks_parses() {
        let doc = "host=h\nsmb-port=445\nmodel=m\nsys-flags=0x100\n";
        assert_eq!(Config::parse(doc).unwrap().disk_count(), 0);
    }

    /// **Every failure names its line.** A configuration parser that says only "invalid" makes the
    /// person edit by bisection.
    #[test]
    fn each_error_points_at_the_line_that_is_wrong() {
        let base = "host=h\nsmb-port=445\nmodel=m\nsys-flags=0x100\n";
        let case = |extra: &str| {
            let mut doc = String::from(base);
            doc.push_str(extra);
            Config::parse(&doc).unwrap_err()
        };
        assert_eq!(case("nonsense\n"), Error::Malformed(5));
        assert_eq!(case("colour = blue\n"), Error::UnknownKey(5));
        assert_eq!(case("host = again\n"), Error::Repeated(5));
        assert_eq!(case("disk = onlyvolume\n"), Error::MalformedDisk(5));
        assert_eq!(case("disk = a b c\n"), Error::MalformedDisk(5));
        assert_eq!(case("smb-port = zero\n"), Error::BadPort(5));
        assert_eq!(case("smb-port = 0\n"), Error::BadPort(5));
        assert_eq!(case("disk = v \n"), Error::MalformedDisk(5));
    }

    /// A missing key is the document's fault, not a line's, and says which key.
    #[test]
    fn a_missing_key_names_itself() {
        assert_eq!(
            Config::parse("smb-port=445\nmodel=m\nsys-flags=0x1\n").unwrap_err(),
            Error::Missing("host")
        );
        assert_eq!(
            Config::parse("host=h\nmodel=m\nsys-flags=0x1\n").unwrap_err(),
            Error::Missing("smb-port")
        );
        assert_eq!(
            Config::parse("host=h\nsmb-port=1\nsys-flags=0x1\n").unwrap_err(),
            Error::Missing("model")
        );
        assert_eq!(
            Config::parse("host=h\nsmb-port=1\nmodel=m\n").unwrap_err(),
            Error::Missing("sys-flags")
        );
    }

    /// **A name too long is refused here**, where the message can point at the line, rather than
    /// arriving later as an emitter overflow with no idea which value caused it.
    #[test]
    fn an_over_long_or_empty_value_is_refused() {
        let long = "host=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\nsmb-port=1\nmodel=m\nsys-flags=0x1\n";
        assert_eq!(Config::parse(long).unwrap_err(), Error::BadValue(1));
        let empty = "host=\nsmb-port=1\nmodel=m\nsys-flags=0x1\n";
        assert_eq!(Config::parse(empty).unwrap_err(), Error::BadValue(1));
    }

    #[test]
    fn more_disks_than_the_array_holds_is_refused_rather_than_dropped() {
        let mut doc = String::from("host=h\nsmb-port=1\nmodel=m\nsys-flags=0x1\n");
        for _ in 0..=MAX_DISKS {
            doc.push_str("disk = v 0x82\n");
        }
        assert!(matches!(
            Config::parse(&doc).unwrap_err(),
            Error::TooManyDisks(_)
        ));
    }
}

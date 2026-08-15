//! Host tests. The primary vectors are **real bytes from the reference router** (GL.iNet
//! GL-BE9300, the working family Time Machine server), captured 2026-08-15 by querying it from the
//! dev machine; the capture method and decoded contents are in notes/mdns.md. Everything the
//! roadmap block said had to be measured is asserted here from the measurement, not from the RFCs:
//! one `_adisk` instance with the disks inside its TXT record, SRV port 0 on `_adisk` and
//! `_device-info`, `model=MacSamba`, and legacy-response shape (ID echoed, question included,
//! TTL 10, no cache-flush bits).

use super::*;

/// The router's response to a PTR query for `_adisk._tcp.local`, verbatim. Legacy unicast (our
/// query came from an ephemeral port), so: question echoed, all five records in the answer
/// section, TTL 10 throughout, compressed names.
const CAPTURE_ADISK: &str = "000084000001000500000000065f616469736b045f746370056c6f63616c00000c0001c00c000c00010000000a000c09474c2d424539333030c00cc02f001000010000000a00431a646b303d6164564e3d636f72696e6e652c616456463d3078383218646b313d6164564e3d63687269732c616456463d307838320e7379733d616456463d3078313030c02f002100010000000a001200000000000009474c2d424539333030c018c09c001c00010000000a0010fd2ce3042cd900000000000000000001c09c000100010000000a0004c0a80801";

/// Same router, `_smb._tcp.local`: SRV port 445, TXT a single empty string.
const CAPTURE_SMB: &str = "000084000001000500000000045f736d62045f746370056c6f63616c00000c0001c00c000c00010000000a000c09474c2d424539333030c00cc02d001000010000000a000100c02d002100010000000a00120000000001bd09474c2d424539333030c016c058001c00010000000a0010fd2ce3042cd900000000000000000001c058000100010000000a0004c0a80801";

/// Same router, `_device-info._tcp.local`: TXT `model=MacSamba`, SRV port 0.
const CAPTURE_DEVICE_INFO: &str = "0000840000010005000000000c5f6465766963652d696e666f045f746370056c6f63616c00000c0001c00c000c00010000000a000c09474c2d424539333030c00cc035001000010000000a000f0e6d6f64656c3d4d616353616d6261c035002100010000000a001200000000000009474c2d424539333030c01ec06e001c00010000000a0010fd2ce3042cd900000000000000000001c06e000100010000000a0004c0a80801";

/// The advertisement that describes the reference router, from the capture. This is test data in
/// the exact sense the crate requires: the flags and model are values, not constants the crate
/// knows.
const REFERENCE: Advertisement<'static> = Advertisement {
    host: "GL-BE9300",
    smb_port: 445,
    disks: &[
        Disk {
            volume: "corinne",
            flags: "0x82",
        },
        Disk {
            volume: "chris",
            flags: "0x82",
        },
    ],
    sys_flags: "0x100",
    model: "MacSamba",
    ipv4: Some([192, 168, 8, 1]),
};

fn unhex(s: &str, out: &mut [u8]) -> usize {
    fn v(c: u8) -> u8 {
        match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            _ => panic!("bad hex digit"),
        }
    }
    let b = s.as_bytes();
    assert!(b.len().is_multiple_of(2), "odd hex length");
    for i in 0..b.len() / 2 {
        out[i] = v(b[2 * i]) * 16 + v(b[2 * i + 1]);
    }
    b.len() / 2
}

/// Walk a whole message into fixed arrays, failing the test on any decode error.
fn parse_all<'a>(msg: &'a [u8]) -> (Header, [Option<Question>; 8], [Option<Record<'a>>; 16]) {
    let mut r = Reader::new(msg).expect("header");
    let h = r.header;
    let mut qs = [None; 8];
    for slot in qs.iter_mut().take(h.qdcount as usize) {
        *slot = Some(r.question().expect("question"));
    }
    let mut rs = [None; 16];
    let n = h.ancount as usize + h.nscount as usize + h.arcount as usize;
    for slot in rs.iter_mut().take(n) {
        *slot = Some(r.record().expect("record"));
    }
    (h, qs, rs)
}

fn find<'a, 'b>(
    records: &'b [Option<Record<'a>>; 16],
    name: &NameBuf,
    t: u16,
) -> Option<&'b Record<'a>> {
    records
        .iter()
        .flatten()
        .find(|r| r.rrtype == t && names_equal(r.name.as_bytes(), name.as_bytes()))
}

// ---------------------------------------------------------------------------
// The wire layer against hand-built bytes.
// ---------------------------------------------------------------------------

/// The header codec round-trips. Twelve bytes, six fields; a swap here misdirects every count.
#[test]
fn the_header_round_trips() {
    let h = Header {
        id: 0x1234,
        flags: FLAG_QR | FLAG_AA,
        qdcount: 1,
        ancount: 5,
        nscount: 2,
        arcount: 3,
    };
    let mut buf = [0u8; HEADER_LEN];
    h.write(&mut buf).unwrap();
    assert_eq!(Header::parse(&buf).unwrap(), h);
}

/// A dotted service name encodes to the exact bytes the reference router's question section
/// carries, checked byte for byte so the encoder and the capture agree on the ground truth.
#[test]
fn a_dotted_name_encodes_to_the_captured_bytes() {
    let n = name_from_dotted(SERVICE_ADISK).unwrap();
    assert_eq!(
        n.as_bytes(),
        b"\x06_adisk\x04_tcp\x05local\x00",
        "the question-section encoding of _adisk._tcp.local"
    );
    // Trailing dot is the same name.
    let dotted = name_from_dotted("_adisk._tcp.local.").unwrap();
    assert_eq!(dotted.as_bytes(), n.as_bytes());
}

/// The encoder refuses what the wire cannot carry: empty labels, labels over 63 bytes, names over
/// 255. Refusal, not truncation, because a truncated name is a different name.
#[test]
fn the_name_encoder_refuses_what_the_wire_cannot_carry() {
    assert_eq!(name_from_dotted("a..b"), Err(Error::BadLabel));
    assert_eq!(name_from_dotted(""), Err(Error::BadLabel));
    let long = core::str::from_utf8(&[b'x'; 64]).unwrap();
    assert_eq!(name_from_dotted(long), Err(Error::LabelTooLong));
    // Five 63-byte labels is 320 wire bytes, over the 255 cap.
    let mut label = [b'y'; 63];
    label[0] = b'y';
    let l = core::str::from_utf8(&label).unwrap();
    let mut parent = name_from_dotted(l).unwrap();
    for _ in 0..4 {
        match name_under(l.as_bytes(), &parent) {
            Ok(n) => parent = n,
            Err(e) => {
                assert_eq!(e, Error::NameTooLong);
                return;
            }
        }
    }
    panic!("a 320-byte name was accepted");
}

/// Name comparison is ASCII case-insensitive (RFC 6762 §9.2): a Mac may spell the query any way
/// it likes and the responder must still recognise its own records.
#[test]
fn names_compare_case_insensitively() {
    let a = name_from_dotted("_ADISK._TCP.LOCAL").unwrap();
    let b = name_from_dotted("_adisk._tcp.local").unwrap();
    assert!(names_equal(a.as_bytes(), b.as_bytes()));
    let c = name_from_dotted("_smb._tcp.local").unwrap();
    assert!(!names_equal(a.as_bytes(), c.as_bytes()));
}

/// The decoder rejects the malformed shapes an attacker (or a bug) can put on a multicast wire:
/// truncation, reserved label tags, and above all pointer loops, which are the classic DNS-parser
/// hang. A self-pointer must be an error, not an infinite loop.
#[test]
fn the_name_decoder_rejects_malformed_input() {
    // Truncated mid-label.
    assert_eq!(decode_name(b"\x05ab", 0), Err(Error::Truncated));
    // Reserved tag bits 0x40.
    assert_eq!(decode_name(b"\x41ab\x00", 0), Err(Error::BadLabel));
    // A pointer to itself: offset 2 points at offset 2.
    let mut msg = [0u8; 4];
    msg[2] = 0xc0;
    msg[3] = 0x02;
    assert_eq!(decode_name(&msg, 2), Err(Error::PointerForward));
    // A forward pointer.
    let fwd = [0xc0, 0x03, 0x00, 0x01, b'a', 0x00];
    assert_eq!(decode_name(&fwd, 0), Err(Error::PointerForward));
    // Two pointers that do not strictly descend: 4 -> 2, 2 -> 2.
    let cyc = [0x00, 0x00, 0xc0, 0x02, 0xc0, 0x02];
    assert_eq!(decode_name(&cyc, 4), Err(Error::PointerForward));
    // A pointer whose second byte is missing.
    assert_eq!(decode_name(&[0xc0], 0), Err(Error::Truncated));
}

/// TXT RDATA: no entries is the single empty string RFC 6763 §6.1 requires (and the reference
/// `_smb` record carries), an oversized entry is refused, and a full buffer reports rather than
/// panics.
#[test]
fn txt_rdata_keeps_the_at_least_one_string_rule() {
    let mut buf = [0u8; 8];
    assert_eq!(txt_rdata(&[], &mut buf).unwrap(), 1);
    assert_eq!(buf[0], 0);

    let big = [b'a'; 256];
    assert_eq!(
        txt_rdata(&[&big], &mut [0u8; 512]),
        Err(Error::EntryTooLong)
    );

    assert_eq!(txt_rdata(&[b"abcdefgh"], &mut buf), Err(Error::Overflow));
}

/// The builder enforces section order, because the header's four counts encode position and a
/// record appended to the wrong section is silently claimed by the wrong count.
#[test]
fn the_builder_enforces_section_order() {
    let name = name_from_dotted("a.local").unwrap();
    let mut buf = [0u8; 256];
    let mut b = Builder::response(&mut buf).unwrap();
    b.answer(name.as_bytes(), rrtype::TXT, CLASS_IN, 10, &[0])
        .unwrap();
    assert_eq!(
        b.question(name.as_bytes(), rrtype::PTR, CLASS_IN),
        Err(Error::SectionOrder)
    );
    b.additional(name.as_bytes(), rrtype::TXT, CLASS_IN, 10, &[0])
        .unwrap();
    assert_eq!(
        b.authority(name.as_bytes(), rrtype::TXT, CLASS_IN, 10, &[0]),
        Err(Error::SectionOrder)
    );
}

/// A buffer too small for the message is an error at the append that overflows, never a panic:
/// the responder must not be crashable by a response that grew past its buffer.
#[test]
fn the_builder_reports_overflow_instead_of_panicking() {
    let name = name_from_dotted("a-rather-long-instance-name.local").unwrap();
    let mut buf = [0u8; 24];
    let mut b = Builder::response(&mut buf).unwrap();
    assert_eq!(
        b.answer(name.as_bytes(), rrtype::TXT, CLASS_IN, 10, &[0]),
        Err(Error::Overflow)
    );
}

/// SRV RDATA round-trips through the encoder and the field decoder.
#[test]
fn srv_rdata_round_trips() {
    let target = name_from_dotted("GL-BE9300.local").unwrap();
    let mut rd = [0u8; 64];
    let n = srv_rdata(0, 0, 445, &target, &mut rd).unwrap();
    // Wrap it in a minimal record so srv_fields can decode it.
    let rec = Record {
        name: target,
        rrtype: rrtype::SRV,
        class: CLASS_IN,
        ttl: 10,
        rdata: &rd[..n],
        rdata_off: 0,
    };
    let srv = srv_fields(&rd[..n], &rec).unwrap();
    assert_eq!(srv.port, 445);
    assert_eq!(srv.priority, 0);
    assert!(names_equal(srv.target.as_bytes(), target.as_bytes()));
}

// ---------------------------------------------------------------------------
// The captured reference responses: the decoder against real, compressed packets.
// ---------------------------------------------------------------------------

/// The `_adisk` capture decodes completely: the compressed instance and target names resolve, the
/// TXT record carries exactly the two disks and the `sys` entry the router advertises, the SRV
/// port is 0, and the A record is the router. This is the crate's whole read side against the
/// packet that specifies milestone 55's requirement.
#[test]
fn the_captured_adisk_response_decodes_completely() {
    let mut buf = [0u8; 512];
    let n = unhex(CAPTURE_ADISK, &mut buf);
    let msg = &buf[..n];
    let (h, qs, rs) = parse_all(msg);

    assert_eq!(h.flags, FLAG_QR | FLAG_AA, "a plain authoritative response");
    assert_eq!(
        (h.qdcount, h.ancount),
        (1, 5),
        "legacy shape: question echoed"
    );

    let service = name_from_dotted(SERVICE_ADISK).unwrap();
    let q = qs[0].unwrap();
    assert!(names_equal(q.name.as_bytes(), service.as_bytes()));
    assert_eq!((q.qtype, q.qclass), (rrtype::PTR, CLASS_IN));

    let instance = name_under(b"GL-BE9300", &service).unwrap();
    let hostname = name_from_dotted("GL-BE9300.local").unwrap();

    let ptr = find(&rs, &service, rrtype::PTR).expect("the PTR answer");
    assert_eq!(ptr.ttl, 10, "legacy TTL cap, as the router applies it");
    assert!(!ptr.cache_flush(), "no cache-flush in a legacy response");
    let target = ptr_target(msg, ptr).unwrap();
    assert!(names_equal(target.as_bytes(), instance.as_bytes()));

    let txt = find(&rs, &instance, rrtype::TXT).expect("the TXT answer");
    let mut strings = txt_strings(txt.rdata);
    assert_eq!(
        strings.next().unwrap().unwrap(),
        b"dk0=adVN=corinne,adVF=0x82"
    );
    assert_eq!(
        strings.next().unwrap().unwrap(),
        b"dk1=adVN=chris,adVF=0x82"
    );
    assert_eq!(strings.next().unwrap().unwrap(), b"sys=adVF=0x100");
    assert!(strings.next().is_none(), "exactly three entries");

    let srv = find(&rs, &instance, rrtype::SRV).expect("the SRV answer");
    let srv = srv_fields(msg, srv).unwrap();
    assert_eq!(
        srv.port, 0,
        "_adisk advertises port 0: flags, not a service"
    );
    assert!(names_equal(srv.target.as_bytes(), hostname.as_bytes()));

    let a = find(&rs, &hostname, rrtype::A).expect("the A answer");
    assert_eq!(a.rdata, &[192, 168, 8, 1]);
    let aaaa = find(&rs, &hostname, rrtype::AAAA).expect("the AAAA answer");
    assert_eq!(aaaa.rdata.len(), 16);
}

/// The `_smb` capture: the connectable half. SRV port 445, and the TXT record is the single empty
/// string (RFC 6763 §6.1's minimum, which is what a service with no attributes emits).
#[test]
fn the_captured_smb_response_decodes_completely() {
    let mut buf = [0u8; 512];
    let n = unhex(CAPTURE_SMB, &mut buf);
    let msg = &buf[..n];
    let (_, _, rs) = parse_all(msg);

    let service = name_from_dotted(SERVICE_SMB).unwrap();
    let instance = name_under(b"GL-BE9300", &service).unwrap();

    let srv = find(&rs, &instance, rrtype::SRV).expect("SRV");
    assert_eq!(srv_fields(msg, srv).unwrap().port, 445);

    let txt = find(&rs, &instance, rrtype::TXT).expect("TXT");
    let mut strings = txt_strings(txt.rdata);
    assert_eq!(strings.next().unwrap().unwrap(), b"", "one empty string");
    assert!(strings.next().is_none());
}

/// The `_device-info` capture: `model=MacSamba`, measured, against a Samba config that says
/// `fruit:model = TimeCapsule`. The two knobs are independent and the working reference proves a
/// Mac accepts this one; the crate treats the model as data because of exactly this packet.
#[test]
fn the_captured_device_info_response_carries_model_macsamba() {
    let mut buf = [0u8; 512];
    let n = unhex(CAPTURE_DEVICE_INFO, &mut buf);
    let msg = &buf[..n];
    let (_, _, rs) = parse_all(msg);

    let service = name_from_dotted(SERVICE_DEVICE_INFO).unwrap();
    let instance = name_under(b"GL-BE9300", &service).unwrap();

    let txt = find(&rs, &instance, rrtype::TXT).expect("TXT");
    let mut strings = txt_strings(txt.rdata);
    assert_eq!(strings.next().unwrap().unwrap(), b"model=MacSamba");
    assert!(strings.next().is_none());

    let srv = find(&rs, &instance, rrtype::SRV).expect("SRV");
    assert_eq!(srv_fields(msg, srv).unwrap().port, 0);
}

/// Our `_adisk` TXT builder reproduces the captured RDATA **byte for byte** from the reference
/// advertisement data. This is the strongest claim the crate makes: the bytes a working family
/// backup accepts are the bytes this function emits.
#[test]
fn adisk_txt_rdata_matches_the_captured_bytes_exactly() {
    let mut buf = [0u8; 512];
    let n = unhex(CAPTURE_ADISK, &mut buf);
    let msg = &buf[..n];
    let (_, _, rs) = parse_all(msg);
    let service = name_from_dotted(SERVICE_ADISK).unwrap();
    let instance = name_under(b"GL-BE9300", &service).unwrap();
    let captured = find(&rs, &instance, rrtype::TXT).unwrap().rdata;

    let mut ours = [0u8; MAX_TXT_RDATA];
    let len = adisk_txt_rdata(&REFERENCE, &mut ours).unwrap();
    assert_eq!(&ours[..len], captured);
}

/// `dk_entry` formats the index in decimal, including multi-digit indices, and the key/value
/// framing the capture shows.
#[test]
fn dk_entry_formats_the_captured_shape() {
    let mut buf = [0u8; 255];
    let d = Disk {
        volume: "corinne",
        flags: "0x82",
    };
    let n = dk_entry(0, &d, &mut buf).unwrap();
    assert_eq!(&buf[..n], b"dk0=adVN=corinne,adVF=0x82");
    let n = dk_entry(12, &d, &mut buf).unwrap();
    assert_eq!(&buf[..n], b"dk12=adVN=corinne,adVF=0x82");
}

// ---------------------------------------------------------------------------
// respond(): the responder's decision function.
// ---------------------------------------------------------------------------

fn build_query(id: u16, name: &NameBuf, qtype: u16, qclass: u16, out: &mut [u8]) -> usize {
    let mut b = Builder::new(out, id, 0).unwrap();
    b.question(name.as_bytes(), qtype, qclass).unwrap();
    b.finish()
}

/// A legacy `_adisk` browse (what `dns-sd`-style one-shot tools and this capture's own query do)
/// produces a response equivalent to the router's: ID echoed, question included, PTR/SRV/TXT/A in
/// the answer section, TTL capped at 10, no cache-flush. Compared record by record rather than
/// byte by byte, because the router compresses names and we do not.
#[test]
fn a_legacy_adisk_browse_matches_the_reference_router() {
    let service = name_from_dotted(SERVICE_ADISK).unwrap();
    let mut qbuf = [0u8; 128];
    let qn = build_query(0x1234, &service, rrtype::PTR, CLASS_IN, &mut qbuf);

    let mut rbuf = [0u8; 1024];
    let n = respond(
        &REFERENCE,
        &qbuf[..qn],
        &mut rbuf,
        QuerySource::LegacyUnicast,
    )
    .unwrap()
    .expect("a browse for our service must be answered");
    let msg = &rbuf[..n];
    let (h, qs, rs) = parse_all(msg);

    assert_eq!(h.id, 0x1234, "legacy responses echo the querier's ID");
    assert_eq!(h.flags, FLAG_QR | FLAG_AA);
    assert_eq!(h.qdcount, 1, "legacy responses include the question");
    assert!(names_equal(
        qs[0].unwrap().name.as_bytes(),
        service.as_bytes()
    ));
    assert_eq!(h.arcount, 0, "legacy: everything in the answer section");

    let instance = name_under(b"GL-BE9300", &service).unwrap();
    let hostname = name_from_dotted("GL-BE9300.local").unwrap();

    let ptr = find(&rs, &service, rrtype::PTR).expect("PTR");
    assert_eq!(ptr.ttl, TTL_LEGACY_CAP);
    let target = ptr_target(msg, ptr).unwrap();
    assert!(names_equal(target.as_bytes(), instance.as_bytes()));

    let txt = find(&rs, &instance, rrtype::TXT).expect("TXT");
    assert!(
        !txt.cache_flush(),
        "no cache-flush bits in a legacy response"
    );
    let mut strings = txt_strings(txt.rdata);
    assert_eq!(
        strings.next().unwrap().unwrap(),
        b"dk0=adVN=corinne,adVF=0x82"
    );
    assert_eq!(
        strings.next().unwrap().unwrap(),
        b"dk1=adVN=chris,adVF=0x82"
    );
    assert_eq!(strings.next().unwrap().unwrap(), b"sys=adVF=0x100");

    let srv = find(&rs, &instance, rrtype::SRV).expect("SRV");
    assert_eq!(srv_fields(msg, srv).unwrap().port, 0);

    let a = find(&rs, &hostname, rrtype::A).expect("A");
    assert_eq!(a.rdata, &[192, 168, 8, 1]);
    assert_eq!(a.ttl, TTL_LEGACY_CAP);
}

/// A real multicast browse gets the multicast shape: ID 0, the PTR answer alone in the answer
/// section, the instance records in additionals with cache-flush set (they are unique to us; the
/// PTR is shared and never flushed), and the full RFC TTLs.
#[test]
fn a_multicast_adisk_browse_gets_the_multicast_shape() {
    let service = name_from_dotted(SERVICE_ADISK).unwrap();
    let mut qbuf = [0u8; 128];
    let qn = build_query(0, &service, rrtype::PTR, CLASS_IN, &mut qbuf);

    let mut rbuf = [0u8; 1024];
    let n = respond(&REFERENCE, &qbuf[..qn], &mut rbuf, QuerySource::Multicast)
        .unwrap()
        .expect("answered");
    let msg = &rbuf[..n];
    let (h, _, rs) = parse_all(msg);

    assert_eq!(h.id, 0, "multicast responses carry ID 0");
    assert_eq!(h.qdcount, 0, "multicast responses repeat no questions");
    assert_eq!(h.ancount, 1, "the PTR answer");
    assert_eq!(h.arcount, 3, "SRV, TXT and A ride in additionals");

    let instance = name_under(b"GL-BE9300", &service).unwrap();
    let ptr = find(&rs, &service, rrtype::PTR).expect("PTR");
    assert_eq!(ptr.ttl, TTL_SERVICE);
    assert!(!ptr.cache_flush(), "a PTR is shared, never cache-flushed");

    let srv = find(&rs, &instance, rrtype::SRV).expect("SRV");
    assert!(srv.cache_flush(), "unique records carry cache-flush");
    assert_eq!(srv.ttl, TTL_SERVICE);
    let hostname = name_from_dotted("GL-BE9300.local").unwrap();
    let a = find(&rs, &hostname, rrtype::A).expect("A");
    assert!(a.cache_flush());
    assert_eq!(a.ttl, TTL_HOST, "address records live 120 s, not 75 min");
}

/// The `_smb` browse advertises the real port, and its empty TXT still goes out (RFC 6763 requires
/// the record to exist).
#[test]
fn an_smb_browse_advertises_port_445() {
    let service = name_from_dotted(SERVICE_SMB).unwrap();
    let mut qbuf = [0u8; 128];
    let qn = build_query(0, &service, rrtype::PTR, CLASS_IN, &mut qbuf);
    let mut rbuf = [0u8; 1024];
    let n = respond(&REFERENCE, &qbuf[..qn], &mut rbuf, QuerySource::Multicast)
        .unwrap()
        .expect("answered");
    let msg = &rbuf[..n];
    let (_, _, rs) = parse_all(msg);
    let instance = name_under(b"GL-BE9300", &service).unwrap();
    let srv = find(&rs, &instance, rrtype::SRV).expect("SRV");
    assert_eq!(srv_fields(msg, srv).unwrap().port, 445);
    let txt = find(&rs, &instance, rrtype::TXT).expect("TXT");
    let mut strings = txt_strings(txt.rdata);
    assert_eq!(strings.next().unwrap().unwrap(), b"");
}

/// A direct SRV query on the instance name is answered with the SRV in the answer section: the
/// Mac does this after a browse when its cache holds the PTR but not the details.
#[test]
fn a_direct_srv_query_on_the_instance_is_answered() {
    let service = name_from_dotted(SERVICE_ADISK).unwrap();
    let instance = name_under(b"GL-BE9300", &service).unwrap();
    let mut qbuf = [0u8; 128];
    let qn = build_query(0, &instance, rrtype::SRV, CLASS_IN, &mut qbuf);
    let mut rbuf = [0u8; 1024];
    let n = respond(&REFERENCE, &qbuf[..qn], &mut rbuf, QuerySource::Multicast)
        .unwrap()
        .expect("answered");
    let (h, _, rs) = parse_all(&rbuf[..n]);
    assert_eq!(h.ancount, 1);
    let srv = find(&rs, &instance, rrtype::SRV).expect("SRV in answers");
    assert_eq!(srv_fields(&rbuf[..n], srv).unwrap().port, 0);
}

/// Service enumeration (RFC 6763 §9) lists all three types, which is how a network scanner or a
/// curious admin discovers what this responder offers.
#[test]
fn service_enumeration_lists_all_three_types() {
    let enumeration = name_from_dotted(SERVICE_ENUMERATION).unwrap();
    let mut qbuf = [0u8; 128];
    let qn = build_query(0, &enumeration, rrtype::PTR, CLASS_IN, &mut qbuf);
    let mut rbuf = [0u8; 1024];
    let n = respond(&REFERENCE, &qbuf[..qn], &mut rbuf, QuerySource::Multicast)
        .unwrap()
        .expect("answered");
    let msg = &rbuf[..n];
    let (h, _, rs) = parse_all(msg);
    assert_eq!(h.ancount, 3);
    let mut seen = [false; 3];
    for (i, svc) in [SERVICE_SMB, SERVICE_ADISK, SERVICE_DEVICE_INFO]
        .iter()
        .enumerate()
    {
        let svc = name_from_dotted(svc).unwrap();
        for r in rs.iter().flatten() {
            if r.rrtype == rrtype::PTR {
                let t = ptr_target(msg, r).unwrap();
                if names_equal(t.as_bytes(), svc.as_bytes()) {
                    seen[i] = true;
                }
            }
        }
    }
    assert_eq!(seen, [true; 3], "every service type is enumerated");
}

/// Known-answer suppression (RFC 6762 §7.1): a querier that already holds our PTR with more than
/// half its TTL left gets silence; one whose copy is stale gets the answer again. This is what
/// keeps a quiet network quiet between a Mac's periodic browses.
#[test]
fn known_answer_suppression_silences_a_fresh_querier() {
    let service = name_from_dotted(SERVICE_ADISK).unwrap();
    let instance = name_under(b"GL-BE9300", &service).unwrap();

    let build = |ttl: u32, out: &mut [u8]| -> usize {
        let mut b = Builder::query(out).unwrap();
        b.question(service.as_bytes(), rrtype::PTR, CLASS_IN)
            .unwrap();
        b.answer(
            service.as_bytes(),
            rrtype::PTR,
            CLASS_IN,
            ttl,
            instance.as_bytes(),
        )
        .unwrap();
        b.finish()
    };

    let mut qbuf = [0u8; 256];
    let mut rbuf = [0u8; 1024];

    let qn = build(TTL_SERVICE, &mut qbuf);
    let fresh = respond(&REFERENCE, &qbuf[..qn], &mut rbuf, QuerySource::Multicast).unwrap();
    assert!(
        fresh.is_none(),
        "a fresh known answer suppresses the response"
    );

    let qn = build(TTL_SERVICE / 2 - 1, &mut qbuf);
    let stale = respond(&REFERENCE, &qbuf[..qn], &mut rbuf, QuerySource::Multicast).unwrap();
    assert!(stale.is_some(), "a stale known answer is refreshed");
}

/// Silence in every case that is not ours: a response (looped back from ourselves or another
/// responder), a query for somebody else's service, and a query for a different instance.
#[test]
fn everything_that_is_not_ours_gets_silence() {
    let mut rbuf = [0u8; 1024];

    // A response, not a query.
    let mut buf = [0u8; 256];
    let service = name_from_dotted(SERVICE_ADISK).unwrap();
    let mut b = Builder::response(&mut buf).unwrap();
    b.answer(service.as_bytes(), rrtype::PTR, CLASS_IN, 10, b"\x01x\x00")
        .unwrap();
    let n = b.finish();
    assert_eq!(
        respond(&REFERENCE, &buf[..n], &mut rbuf, QuerySource::Multicast).unwrap(),
        None
    );

    // Somebody else's service type.
    let other = name_from_dotted("_ipp._tcp.local").unwrap();
    let mut qbuf = [0u8; 128];
    let qn = build_query(0, &other, rrtype::PTR, CLASS_IN, &mut qbuf);
    assert_eq!(
        respond(&REFERENCE, &qbuf[..qn], &mut rbuf, QuerySource::Multicast).unwrap(),
        None
    );

    // Our service type, somebody else's instance.
    let foreign = name_under(b"NotOurServer", &service).unwrap();
    let qn = build_query(0, &foreign, rrtype::SRV, CLASS_IN, &mut qbuf);
    assert_eq!(
        respond(&REFERENCE, &qbuf[..qn], &mut rbuf, QuerySource::Multicast).unwrap(),
        None
    );
}

/// The QU bit (RFC 6762 §5.4) parses out of the question class and does not confuse the class
/// itself. The responder does not yet *act* on it (it answers multicast either way, which is
/// always legal); this pins the parse so the acting version has the bit to act on.
#[test]
fn the_qu_bit_parses_out_of_the_question_class() {
    let service = name_from_dotted(SERVICE_ADISK).unwrap();
    let mut qbuf = [0u8; 128];
    let qn = build_query(0, &service, rrtype::PTR, CLASS_IN | QU, &mut qbuf);
    let mut r = Reader::new(&qbuf[..qn]).unwrap();
    let q = r.question().unwrap();
    assert!(q.unicast_requested());
    assert_eq!(q.class(), CLASS_IN);
    // And a QU question is still answered.
    let mut rbuf = [0u8; 1024];
    assert!(
        respond(&REFERENCE, &qbuf[..qn], &mut rbuf, QuerySource::Multicast)
            .unwrap()
            .is_some()
    );
}

// ---------------------------------------------------------------------------
// Probing: the wire halves of probe-before-claim.
// ---------------------------------------------------------------------------

/// A probe is an ANY question with the proposed records in the authority section, which is the
/// shape RFC 6762 §8.1 mandates so a simultaneous prober can tiebreak instead of deadlocking.
#[test]
fn a_probe_query_has_the_mandated_shape() {
    let service = name_from_dotted(SERVICE_ADISK).unwrap();
    let claim = name_under(b"GL-BE9300", &service).unwrap();
    let hostname = name_from_dotted("GL-BE9300.local").unwrap();
    let mut rd = [0u8; 64];
    let n = srv_rdata(0, 0, 0, &hostname, &mut rd).unwrap();
    let proposed = [ProposedRecord {
        rrtype: rrtype::SRV,
        ttl: TTL_SERVICE,
        rdata: &rd[..n],
    }];
    let mut buf = [0u8; 512];
    let len = probe_query(&claim, &proposed, &mut buf).unwrap();
    let (h, qs, rs) = parse_all(&buf[..len]);
    assert_eq!(h.id, 0);
    assert_eq!(h.flags, 0, "a probe is a query");
    assert_eq!((h.qdcount, h.nscount), (1, 1));
    let q = qs[0].unwrap();
    assert_eq!(q.qtype, rrtype::ANY, "probes ask ANY");
    assert!(
        q.unicast_requested(),
        "first probe asks for unicast replies"
    );
    let rec = rs[0].unwrap();
    assert!(names_equal(rec.name.as_bytes(), claim.as_bytes()));
    assert_eq!(rec.rrtype, rrtype::SRV);
}

/// Conflict detection reads real packets: the captured router response defeats a probe for the
/// name the router holds, and leaves a differently named probe alone.
#[test]
fn the_captured_response_defeats_a_probe_for_its_name() {
    let mut buf = [0u8; 512];
    let n = unhex(CAPTURE_ADISK, &mut buf);
    let service = name_from_dotted(SERVICE_ADISK).unwrap();
    let taken = name_under(b"GL-BE9300", &service).unwrap();
    let free = name_under(b"nife-backups", &service).unwrap();
    assert!(conflicts(&buf[..n], &taken).unwrap());
    assert!(!conflicts(&buf[..n], &free).unwrap());
}

/// The simultaneous-probe tiebreak is lexicographic over (class, type, rdata), rdata decided
/// bytewise: RFC 6762 §8.2.1's exact comparison, which both parties must compute identically or
/// both back off forever.
#[test]
fn the_probe_tiebreak_is_lexicographic() {
    use core::cmp::Ordering;
    // rdata decides when class and type tie.
    assert_eq!(
        tiebreak(
            (CLASS_IN, rrtype::SRV, b"\x00\x01"),
            (CLASS_IN, rrtype::SRV, b"\x00\x02")
        ),
        Ordering::Less
    );
    // A longer rdata that is a prefix-extension is greater.
    assert_eq!(
        tiebreak(
            (CLASS_IN, rrtype::SRV, b"\x00\x01\x00"),
            (CLASS_IN, rrtype::SRV, b"\x00\x01")
        ),
        Ordering::Greater
    );
    // Type outranks rdata: TXT (16) sorts below SRV (33) whatever the rdata says.
    assert_eq!(
        tiebreak(
            (CLASS_IN, rrtype::TXT, b"\xff"),
            (CLASS_IN, rrtype::SRV, b"\x00")
        ),
        Ordering::Less
    );
    // The cache-flush bit is not part of the comparison.
    assert_eq!(
        tiebreak(
            (CLASS_IN | CACHE_FLUSH, rrtype::SRV, b"a"),
            (CLASS_IN, rrtype::SRV, b"a")
        ),
        Ordering::Equal
    );
}

# mDNS/DNS-SD: the Time Machine advertisement

Milestone 55's second protocol. A Mac's Time Machine UI lists only servers it discovered over
multicast DNS, and the reference router advertises exactly three service types: `_smb._tcp` (the
file service and its port), `_adisk._tcp` (the Time Machine flags, which are what populate the
backup-disk list), and `_device-info._tcp` (a model string, which picks the icon). The requirement
was measured, not assumed: `dns-sd -B _adisk._tcp` on the family network returns the router, so the
working reference does it, and proving it unnecessary would mean disabling it on a working family
backup system (design/roadmap/55-time-machine.md, "mDNS is required after all").

Two pieces exist or are pending:

- **`crates/mdns_proto`** (built): the DNS wire format, compression handling, the DNS-SD PTR/SRV/TXT
  structuring, the probe-before-claim wire halves, and `respond()`, the responder's entire decision
  as a pure function. Host-tested against real captured router packets; Kani harnesses cover the
  parser's termination and bounds.
- **The responder program** (not built, deliberately): it needs multicast reception, and the stack
  does not offer it yet. The verdict and the missing pieces are below; building them is a lane of
  its own.

## The measured reference, captured 2026-08-15

All three service types were captured from calef's router (GL.iNet GL-BE9300 running OpenWrt,
Samba with `vfs_fruit`, the working family Time Machine server) by sending one-shot PTR queries
from the dev machine. The router answered from `192.168.8.1:5353`; the full hex is in
`crates/mdns_proto/src/tests.rs` as the primary test vectors. Decoded:

| Record | `_smb._tcp` | `_adisk._tcp` | `_device-info._tcp` |
|---|---|---|---|
| PTR target | `GL-BE9300.<type>.local` | same | same |
| SRV port | **445** | **0** | **0** |
| SRV target | `GL-BE9300.local` | same | same |
| TXT | one empty string | `dk0=adVN=corinne,adVF=0x82`, `dk1=adVN=chris,adVF=0x82`, `sys=adVF=0x100` | `model=MacSamba` |

Plus an A record (`192.168.8.1`) and an AAAA in every response. Findings that beat the documentation:

- **One `_adisk` instance for all shares.** The disks are `dkN=` entries inside a single TXT record.
  Emitting one announcement per share would be wrong.
- **Two disks is correct.** The roadmap block described three users; graeme migrated from macOS to
  Windows and his share was dropped, so the reference advertises corinne and chris (calef confirmed,
  2026-08-15).
- **`model=MacSamba`, not `TimeCapsule`.** The router's own Samba config sets
  `fruit:model = TimeCapsule`, and its mDNS advertisement says `MacSamba` anyway: the SMB-side AAPL
  model and the `_device-info` TXT are separate knobs, and the working reference runs with them
  disagreeing. Whatever `fruit:model` buys, it is not this record. The crate therefore takes the
  model as data.
- **`_adisk` and `_device-info` advertise SRV port 0.** They carry flags, not a connectable service.
- **Legacy unicast shape confirmed** (RFC 6762 §6.7): our queries came from an ephemeral port, and
  the router echoed the ID, included the question, put all five records in the answer section, set
  no cache-flush bits, and capped every TTL at 10.

The flag values are copied as measured; **the meaning of the `adVF` bits is not decoded here**, and
does not need to be until something wants to emit different ones.

## The smoltcp multicast answer (the question the roadmap block asks first)

**The tree's smoltcp is 0.13.1** (`Cargo.lock`; pinned in `user/Cargo.toml` with
`default-features = false` and features `alloc`, `medium-ethernet`, `proto-ipv4`, `proto-dhcpv4`,
`socket-udp`, `socket-tcp`, `socket-dhcpv4`).

**smoltcp 0.13.1 supports what mDNS needs, and the tree has it switched off.** The `multicast`
cargo feature (in smoltcp's own default set, which `default-features = false` discards) provides:

- `Interface::join_multicast_group` / `leave_multicast_group` (`iface/interface/multicast.rs`),
  with IGMP membership reports sent and IGMP queries answered for IPv4 groups (MLD for IPv6).
- Receive-path acceptance: `process_ipv4` drops any packet whose destination is not us, broadcast,
  or a joined group (`has_multicast_group`). **Without the feature, the only IPv4 multicast group
  accepted is `224.0.0.1`** (all-systems, hardcoded), so datagrams to `224.0.0.251` are discarded
  before UDP ever sees them. The ethernet layer already accepts multicast MAC frames either way;
  the filter that matters is the IP one.

So the responder is *nearly* ordinary socket code, and the distance is measured in small pieces,
none of them in this lane:

1. **The feature flag**: add `"multicast"` to the smoltcp features in `user/Cargo.toml`. One line.
   Not a new dependency, but it is a change to a vendored-engine pin's configuration, so it should
   land visibly, not ride along.
2. **The join**: `net_stack` calls `join_multicast_group(Ipv4Address::new(224, 0, 0, 251))` at
   startup and polls so the IGMP report goes out. A few lines.
3. **Socket surface, the real work.** The `socket_proto` contract cannot express an mDNS responder
   today, three ways:
   - `OP_OPEN_UDP` binds an **ephemeral** local port only. mDNS must bind 5353. A fixed UDP port is
     a claim on a shared namespace, the same authority question `LISTEN` answered for TCP with the
     listen grant, and it should be granted the same way rather than opened to any client.
   - `OP_RECV` **discards the datagram's source endpoint** (`net_stack.rs`, `sock_recv`:
     `.map(|(n, _)| n)`). A responder must see the source, both to reply unicast and because RFC
     6762 §6.7 makes the semantics turn on whether the source port is 5353. The shared frame's
     `dst_ip`/`dst_port` header fields are dead space on a RECV reply and could carry the source
     without a format change; that is a proposal, not a decision.
   - `OP_SENDTO` to a multicast destination should work as-is (smoltcp maps IPv4 multicast to the
     multicast MAC without ARP), but nothing in the tree has exercised it. Measure before trusting.

The honest size: the stack half of the responder is a small milestone (a feature line, a join call,
one granted-port mechanism, source addressing on RECV), and the protocol half is already built and
tested. What was *not* needed is any change to smoltcp itself.

## Testing the eventual responder under QEMU

The QEMU nets in `xtask` are slirp (user-mode networking) with `hostfwd` for inbound TCP. Slirp
does not deliver external multicast into the guest, so the existing harness cannot inject an mDNS
query from the host the way the TCP tests connect in. The responder milestone should measure its
options before designing the gate; the candidates are a `-netdev socket,mcast=` pair (two guests,
or host code speaking the socket protocol), a tap backend, or driving `respond()`'s decision purely
on-device with hand-fed frames. The protocol logic itself does not need QEMU at all; that is what
the crate split buys.

## EXAMPLES

Re-deriving the reference vectors needs no special tooling. From any machine on the router's
network, a one-shot legacy query (Python, ephemeral port, so the answer arrives unicast):

```python
import socket, struct
def qname(n):
    return b"".join(bytes([len(l)]) + l.encode() for l in n.split(".") if l) + b"\x00"
q = struct.pack(">HHHHHH", 0, 0, 1, 0, 0, 0) + qname("_adisk._tcp.local") + struct.pack(">HH", 12, 1)
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.settimeout(3)
s.sendto(q, ("224.0.0.251", 5353))
print(s.recvfrom(4096)[0].hex())
```

Or, on macOS, the resolver's own view (which exercises the multicast path a Mac actually uses):

```sh
dns-sd -B _adisk._tcp             # browse: who advertises Time Machine disks
dns-sd -L GL-BE9300 _adisk._tcp local   # resolve: the TXT keys and SRV
```

The `-L` output presents decoded TXT entries; the Python capture gives the wire bytes, which is
what a test vector needs.

## BUGS

- **The multicast-response shape is asserted from the RFC, not from a capture.** All three captured
  packets are legacy unicast responses (the capture tool cannot bind 5353 while mDNSResponder holds
  it). The crate's multicast responses (ID 0, additionals, cache-flush, TTL 4500/120) follow RFC
  6762 and are pinned by tests, but no working implementation's multicast bytes have been compared.
  Capturing the router's *multicast* answer to a Mac's real browse (tcpdump on port 5353) would
  close that, and is cheap for whoever next holds a root shell on the network.
- **No AAAA emission.** `Advertisement` carries an optional IPv4 address only. The reference emits
  AAAA; a Mac on an IPv6-only network would not find us. Add the field when the responder exists to
  use it.
- **`respond()` does not act on the QU bit** (it parses; the responder answering multicast either
  way is always legal, just occasionally chattier).
- The crate's own BUGS section (`crates/mdns_proto/src/lib.rs`) records the wire-level limits:
  uncompressed emission, PTR-only known-answer suppression, probe timing left to the caller, no
  TC-bit delay.
- **smoltcp's `socket-mdns` feature was not evaluated.** It exists in 0.13.1 for *client-side*
  DNS-over-multicast lookups (a `socket-dns` variant), not for a responder, so it does not change
  the verdict above; recorded so nobody re-derives that.

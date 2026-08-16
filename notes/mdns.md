# mDNS/DNS-SD: the Time Machine advertisement

Milestone 55's second protocol. A Mac's Time Machine UI lists only servers it discovered over
multicast DNS, and the reference router advertises exactly three service types: `_smb._tcp` (the
file service and its port), `_adisk._tcp` (the Time Machine flags, which are what populate the
backup-disk list), and `_device-info._tcp` (a model string, which picks the icon). The requirement
was measured, not assumed: `dns-sd -B _adisk._tcp` on the family network returns the router, so the
working reference does it, and proving it unnecessary would mean disabling it on a working family
backup system (design/roadmap/55-time-machine.md, "mDNS is required after all").

Four pieces, all built:

- **`crates/mdns_proto`**: the DNS wire format, compression handling, the DNS-SD PTR/SRV/TXT
  structuring, the probe-before-claim wire halves, `respond()` (the responder's entire decision as a
  pure function) and `announcement()` (RFC 6762 §8.3's unsolicited response). Host-tested against
  real captured router packets; Kani harnesses cover the parser's termination and bounds.
- **The stack half** (both ISAs): smoltcp's `multicast` feature is on and `net_stack` joins
  224.0.0.251 at startup; `BIND_UDP` claims a fixed port against a granted range
  (`socket_proto::udp_bind_grant`, the UDP twin of milestone 107's listen grant, riding the high
  half of the same spawn word); a UDP `RECV` reply carries the datagram's source endpoint in the
  frame's dst fields.
- **`crates/mdns_config` and `user/mdns_responder.conf`**: what this machine advertises, as a
  document a person edits rather than constants in a program. See "The configuration" below.
- **`user/src/mdns_responder.rs`**: the program. Binds 5353 through the grant, announces, then
  answers queries with `respond()` until it has served its rounds. **One authority and nothing
  else**: it holds no share, no file, no TCP port, so the process that tells a Mac a backup target
  exists cannot serve a byte of it, and the process that serves the bytes (`smb_server`) cannot be
  found. On the reference implementation those are one daemon with one configuration file.

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

So the responder is *nearly* ordinary socket code, and the distance was measured in small pieces.
**All three landed with the stack half** (milestone 55, the lane after the one that wrote this
note), the shapes the sizing proposed:

1. **The feature flag**: `"multicast"` in `user/Cargo.toml`'s smoltcp features. Landed as its own
   commit, being a change to a vendored-engine pin's configuration.
2. **The join**: `net_stack` joins 224.0.0.251 right after DHCP configures, then polls so the IGMP
   membership report carries a real source address. Membership is interface state, not socket
   state, so the join is unconditional; what is granted per client is the port.
3. **Socket surface.** The three gaps, closed:
   - `OP_BIND_UDP` (name provisional) binds a **fixed** UDP port, checked against a **UDP bind
     grant** the spawn site packs with `socket_proto::udp_bind_grant` into the high half of the
     same spawn word milestone 107's listen grant occupies. The halves are independent
     authorities; the zero word still grants nothing anywhere. The reply vocabulary is `LISTEN`'s
     three outcomes, which are properties of claiming a port, not of TCP.
   - A UDP `RECV` reply now writes the datagram's **source endpoint** into the shared frame's
     `dst_ip`/`dst_port` fields, the dead-space proposal above, taken. TCP `RECV` leaves them
     untouched; the peer is fixed by the connection.
   - `OP_SENDTO` to a multicast destination was measured, not trusted: the QEMU gate's host-side
     prober takes the guest's group-addressed datagram off the raw wire.

What was *not* needed is any change to smoltcp itself.

## The configuration: what a person edits

**`user/mdns_responder.conf`**, parsed by `crates/mdns_config`, host-tested, and the responder's
only source for what it says:

```
host      = GL-BE9300
smb-port  = 445
model     = MacSamba
sys-flags = 0x100
disk      = corinne 0x82
disk      = chris   0x82
```

Two properties are worth stating because both were decisions rather than defaults.

**The IP address is not in it.** Which address this machine holds is established by the DHCP lease,
so the spawn site drains the lease *before* spawning the responder and passes it as an argument; the
responder announces an A record with it, or no A record at all when it is zero. A configuration file
naming an address goes stale the first time the network changes, and an announcement with a wrong A
record is worse than one with none: a Mac caches it and then cannot connect.

**The document is compiled in, not read from disk.** `include_str!`, because a program reading a
file needs a file capability wired through the spawn and a fixture the QEMU gate cannot seed today.
That is a delivery limitation and not a design one: the format, the parser, the line-numbered errors
and every test are unaffected by where the bytes come from. The fix is a `FileSpec` grant plus an
`fs_proto` open-and-read at startup, and it is the shortest remaining piece of milestone 55's
discovery half.

The gate reads the same document (`xtask` depends on `mdns_config`) and derives its expectations
from it, so editing what this machine advertises moves the assertion with it. What the gate does
**not** share is the wire format: it decodes the guest's answers with a parser of its own, because a
check that decoded them with `mdns_proto` would agree with `mdns_proto` about anything wrong.

## The QEMU gate: what it proves, and how

Slirp cannot carry multicast in either direction, so the gate goes under it. When xtask runs the
suite, the runners attach the mmio NIC to a **QEMU hub** (`-netdev hubport`) with two backends:
slirp, unchanged (DHCP, TFTP, guestfwd, hostfwd all keep working, because a hub floods every frame
to every port), and a `-netdev socket` listener that xtask's **multicast prober** connects to,
speaking QEMU's frame protocol (4-byte big-endian length, then the raw ethernet frame). The prober
is the multicast twin of milestone 107's inbound prober: constructed before the child so the runner
inherits `NIFE_MCAST_PORT`, passive for the whole boot, reported after the suite.

The exchange rides **inside milestone 107's accept test**
(`a_host_process_connects_to_the_guest_and_is_answered`, both ISAs), after its TCP rounds, rather
than in a spawn of its own: a net server's spawn is ~154 frames nothing ever reclaims, and a
twelfth one died as `Unmappable(OutOfFrames)` in an unrelated later test, the exact failure
notes/net.md's memory receipt predicted. So that one spawn now carries three clients on one `Stack`
endpoint: `socket_test_client` (socket ids 0 and 1), `smb_server` (2 and 3), and `mdns_responder`
(4). Its grant word is `listen_grant(7778, 7779) | udp_bind_grant(5353, 5354)`, so the *composed*
packing is what the machine exercises, not one half alone.

**The guest side splits in two, and the split says which half can prove what.**

`socket_test_client`'s `udp_mdns_half` keeps only the **refusals**, because they are what a program
holding a granted port cannot demonstrate about itself: 4444 is outside the grant and is refused as
`LISTEN_DENIED` (authority, a different answer from "in use" and calling for a different response),
5354 is inside it and binds, and asking for 5354 again on a second socket id collides.

`mdns_responder` does the rest, with real DNS:

1. It binds 5353 (its whole authority), then **announces** all three service types to the group. The
   announcement's arrival at the prober, off the raw wire, is the proof that a multicast `SENDTO`
   leaves the guest at all, and it is also where the prober learns the guest's address, from the
   announcement's own A record.
2. The prober sends an **ARP request for the guest's address** from the source it spoofs
   (10.0.2.99). This is load-bearing rather than polite; see the finding below.
3. The prober injects a **multicast browse**: a real PTR query for `_adisk._tcp.local`, addressed to
   the group rather than to the guest, from a spoofed source nothing on the virtual network holds.
   The guest's answer must come back to the group with the PTR in the answer section, the instance's
   SRV, TXT and the host's A as **additionals** (RFC 6763 §12.1), cache-flush set on the three the
   responder owns and clear on the shared PTR, and every value matching `user/mdns_responder.conf`.
   That the injected datagram is accepted at all is the RX-acceptance proof the `multicast` feature
   exists for: without the join, the IPv4 input path drops it before UDP sees it.
4. The prober then asks the **same question as a legacy one-shot**, from source port 5399 with
   transaction id 0x4321. RFC 6762 §6.7 makes the answer a different shape *and a different
   destination*: it must arrive **unicast at 10.0.2.99:5399**, with the id echoed, the question
   repeated, every record in the answer section, no cache-flush bits, and every TTL capped at 10.
   This is also the end-to-end proof that a datagram's source endpoint survives the socket contract,
   which is why that leg replaced the stack half's hand-rolled assertion of the same thing.

**The finding worth telling somebody about, because it is the one that could have produced a false
green.** Slirp is on the same hub, and it forwards a group-addressed datagram out to the *host's
real network*. On 2026-08-16 the gate's injected browse for `_adisk._tcp.local` therefore left the
laptop, and **the reference router answered it**: a 212-byte response from 192.168.8.1, NATed back
onto the virtual network by slirp as a unicast to the spoofed source, carrying the five records
notes/mdns.md's capture describes. Those are the records this gate expects, because the expectations
were captured from that router. A prober that checked whichever answer arrived first would have
passed on the GL-BE9300's own bytes while the guest said nothing at all, on the developer's network
and nowhere else. So the prober takes only datagrams whose source is the guest, and the guest's
announcement must speak from the address its own A record advertises.

**The finding that cost the most to learn**: smoltcp fills its neighbour cache **only from an ARP
packet whose target is an address the interface holds** (`process_arp` returns early otherwise, in
0.13.1), and `dispatch` **drops** the datagram that triggers a neighbour resolution rather than
queueing it. So a gratuitous ARP announcing the prober's address is discarded, the guest would have
to resolve 10.0.2.99 when it answered the legacy query, and that first answer would be lost with
nothing to retry it. Asking the guest for its own address fills the cache in the same breath, which
is why step 2 is a request rather than an announcement.

**Retries are self-synchronising, which matters because the responder answers a fixed number of
queries.** The responder re-announces whenever a receive times out, so an announcement means "the
last thing you sent me did not arrive"; the prober re-injects the query for the stage it is in, and
advances only when it has *verified* an answer. Nothing counts a datagram that was lost.

The TFTP gate carries the slirp-shaped half of source reporting on both ISAs: it asserts the DATA
packet's source and ACKs to it, which is what TFTP's TID scheme (RFC 1350 §4) wanted all along and
is the same reply-to-the-querier move the legacy leg above makes.

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

**Running the responder here.** The serve-forever boot starts the net server, the SMB adapter and
the responder together, each with exactly the port its spawn granted it:

```sh
cargo xtask smb-serve      # boots with a RedoxFS disk and a NIC, and parks
```

It prints the DHCP lease, the mount line for the SMB share, and then:

```
smb-serve: the mDNS responder is advertising _smb._tcp, _adisk._tcp and _device-info._tcp on 5353.
smb-serve:   on a Mac on the SAME SEGMENT: dns-sd -B _adisk._tcp
smb-serve:   what it advertises is user/mdns_responder.conf, not compiled-in.
```

**That `dns-sd` will find nothing under QEMU**, and the reason is the same one the gate exists for:
user-mode networking does not carry multicast, so the announcement never leaves the emulator for the
host's segment. The responder is doing its job and nobody can hear it. Proving discovery end to end
needs the kernel on hardware with a real NIC on the family network, which is milestone 55's bench
and the first place a Mac's Time Machine UI could list this share.

**Changing what it advertises** is one file and a rebuild:

```sh
$EDITOR user/mdns_responder.conf
cargo test -p mdns_config     # the shipped document must parse, and the disks must reach the TXT
cargo xtask build
```

A mistake in the document does not put a half-formed advertisement on the network: the responder
refuses to start and reports `0xE20L`, where `L` is the line number.

## BUGS

- **What the QEMU gate cannot prove, for the bench to pick up.** The hub is a wire with no router
  on it, so everything a real network's multicast turns on is out of its reach: **IGMP snooping**
  (a switch that forwards group traffic only to reported members; the gate never checks the
  guest's membership report is well-formed enough to satisfy one, only that acceptance works),
  **TTL handling by real forwarding** (the injected frame carries TTL 255 but nothing routes it),
  coexistence with a real network's **mDNS chatter** (the gate's group traffic is a handful of known
  datagrams; a live segment delivers a firehose of other hosts' queries and announcements to every
  member, and nothing here proves the stack keeps up or that the 2048-byte socket buffer survives
  it), and **a real querier**: no Mac's mDNSResponder has asked this stack anything, and no Time
  Machine UI has listed this share. The bench on hardware, on the family network, with `dns-sd -B`
  and then with System Settings, is where those claims get proven; the gate's job is that the
  stack's filters, grants, headers and **records** are right.
- **No probing before claiming the name** (RFC 6762 §8.1). `mdns_proto` has the wire halves
  (`probe_query`, `conflicts`, `tiebreak`) and the responder does not use them: it announces
  straight away. Two nife machines with the same `host` in their configuration would both claim it
  and neither would notice. The missing part is a timer and a state machine, not bytes.
- **One announcement, not the RFC's two to eight**, and no goodbye records when the responder stops.
  Both need the same timer. A Mac that misses the announcement finds us at its next browse, which is
  frequent; a Mac that was watching keeps a dead server in its list until the TTL runs out.
- **A response that would exceed the MTU is not sent.** The virtio transport carries 576 bytes (a
  single-page DMA region), so `mdns_proto` composes at most 548 and the responder stays silent
  rather than handing smoltcp a datagram it will not fragment. `announcement()` emits one service
  type per call for the same reason, and a host test pins that all three fit, including at
  `mdns_config`'s eight-disk ceiling. RFC 6762's own answer is the TC bit and a follow-up message,
  which nothing here implements.
- **The configuration is compiled in.** See "The configuration" above; the fix is a file capability.
- **The prober holds one TCP connection and never reconnects.** If QEMU drops the frame socket
  mid-run the check fails as "reading frames failed" rather than retrying; acceptable for a gate,
  recorded so its first flake is not a mystery.
- **The multicast-response shape is asserted from the RFC, not from a capture.** All three captured
  packets are legacy unicast responses (the capture tool cannot bind 5353 while mDNSResponder holds
  it). The crate's multicast responses (ID 0, additionals, cache-flush, TTL 4500/120) follow RFC
  6762 and are now pinned by the QEMU gate as well as by host tests, but no working implementation's
  multicast bytes have been compared. Capturing the router's *multicast* answer to a Mac's real
  browse (tcpdump on port 5353) would close that, and is cheap for whoever next holds a root shell
  on the network.
- **No AAAA emission.** `Advertisement` carries an optional IPv4 address only. The reference emits
  AAAA; a Mac on an IPv6-only network would not find us. The responder joins no IPv6 group either.
- **`respond()` does not act on the QU bit** (it parses; the responder answering multicast either
  way is always legal, just occasionally chattier).
- **Known-answer suppression is PTR-only**, and the responder inherits that: a querier that already
  holds our SRV or TXT is told again. Chatty, not wrong.
- The crate's own BUGS section (`crates/mdns_proto/src/lib.rs`) records the remaining wire-level
  limits: uncompressed emission, probe and announce timing left to the caller, no TC-bit delay.
- **smoltcp's `socket-mdns` feature was not evaluated.** It exists in 0.13.1 for *client-side*
  DNS-over-multicast lookups (a `socket-dns` variant), not for a responder, so it does not change
  the verdict above; recorded so nobody re-derives that.

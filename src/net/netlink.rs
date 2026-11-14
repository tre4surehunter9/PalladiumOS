// Copyright (c) 2026 tre4surehunter9
//! Netlink (AF_NETLINK, NETLINK_ROUTE), read-only: just enough for
//! programs to list the network interfaces and their addresses, the way
//! Linux answers them. libc's getifaddrs() asks this way (so fastfetch's
//! Local IP and other programs work), and so does `ip addr`.
//!
//! Supported requests: RTM_GETLINK, RTM_GETADDR and RTM_GETROUTE dumps
//! (other dumps come back empty; other requests get EOPNOTSUPP: nothing
//! can be changed here). Also the interface ioctls (SIOCGIFNAME,
//! SIOCGIFINDEX, SIOCGIFFLAGS, SIOCGIFADDR, SIOCGIFNETMASK, SIOCGIFMTU,
//! SIOCGIFTXQLEN,
//! SIOCGIFHWADDR) that if_indextoname() and friends use, on any socket.

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use spin::Mutex;

use crate::syscall::{EAGAIN, EINTR, EINVAL};

pub const AF_NETLINK: u64 = 16;
pub const NETLINK_ROUTE: u64 = 0;
const MSG_DONTWAIT: u64 = 0x40;

const NLMSG_ERROR: u16 = 2;
const NLMSG_DONE: u16 = 3;
const RTM_NEWLINK: u16 = 16;
const RTM_GETLINK: u16 = 18;
const RTM_NEWADDR: u16 = 20;
const RTM_GETADDR: u16 = 22;
const RTM_NEWROUTE: u16 = 24;
const RTM_DELROUTE: u16 = 25;
const RTM_GETROUTE: u16 = 26;
const NLM_F_ACK: u16 = 4;
const NLM_F_MULTI: u16 = 2;
const NLM_F_DUMP: u16 = 0x300;
const AF_UNSPEC: u8 = 0;
const AF_INET: u8 = 2;
const AF_INET6: u8 = 10;

static NEXT_ID: AtomicU64 = AtomicU64::new(1 << 40);

pub struct NetlinkSocket {
    /// Replies waiting to be read, one buffer per recv().
    replies: Mutex<VecDeque<Vec<u8>>>,
    nonblocking: AtomicBool,
    /// The port id (nl_pid): the creating process's pid, as Linux gives
    /// a process's first netlink socket. Replies carry it.
    pub port: u32,
    pub id: u64,
}

/// One network interface, as netlink describes it.
struct Link {
    index: u32,
    name: &'static [u8],
    flags: u32,
    kind: u16,
    mtu: u32,
    mac: [u8; 6],
    addr: Option<([u8; 4], u8)>,
    gateway: Option<[u8; 4]>,
}

fn links() -> Vec<Link> {
    const IFF_UP: u32 = 0x1;
    const IFF_BROADCAST: u32 = 0x2;
    const IFF_LOOPBACK: u32 = 0x8;
    const IFF_RUNNING: u32 = 0x40;
    const IFF_MULTICAST: u32 = 0x1000;
    const IFF_LOWER_UP: u32 = 0x10000;
    let mut v = alloc::vec![Link {
        index: 1,
        name: b"lo",
        flags: IFF_UP | IFF_LOOPBACK | IFF_RUNNING | IFF_LOWER_UP,
        kind: 772, // ARPHRD_LOOPBACK
        mtu: 65536,
        mac: [0; 6],
        addr: Some(([127, 0, 0, 1], 8)),
        gateway: None,
    }];
    const NAMES: [&[u8]; super::card::MAX_CARDS] = [b"eth0", b"eth1"];
    let cards: Vec<_> = super::NET.lock().as_ref().map_or(Vec::new(), |n| n.eth.iter().map(|e| (e.index, e.dev.mac(), e.config)).collect());
    for (i, mac, config) in cards {
        v.push(Link {
            index: 2 + i as u32,
            name: NAMES[i],
            flags: if super::ETH_UP[i].load(core::sync::atomic::Ordering::Relaxed) {
                IFF_UP | IFF_BROADCAST | IFF_RUNNING | IFF_MULTICAST | IFF_LOWER_UP
            } else {
                IFF_BROADCAST | IFF_MULTICAST | IFF_LOWER_UP
            },
            kind: 1, // ARPHRD_ETHER
            mtu: 1500,
            mac,
            addr: config.map(|c| (c.address.address().octets(), c.address.prefix_len())),
            gateway: config.and_then(|c| c.gateway).map(|g| g.octets()),
        });
    }
    v
}

fn align4(n: usize) -> usize {
    (n + 3) & !3
}

/// Append a route attribute (type, data), padded to 4 bytes.
fn attr(out: &mut Vec<u8>, ty: u16, data: &[u8]) {
    out.extend_from_slice(&((4 + data.len()) as u16).to_le_bytes());
    out.extend_from_slice(&ty.to_le_bytes());
    out.extend_from_slice(data);
    out.resize(align4(out.len()), 0);
}

/// Append a netlink message: header, then `body`.
fn message(out: &mut Vec<u8>, ty: u16, flags: u16, seq: u32, pid: u32, body: &[u8]) {
    let len = 16 + body.len();
    out.extend_from_slice(&(len as u32).to_le_bytes());
    out.extend_from_slice(&ty.to_le_bytes());
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&seq.to_le_bytes());
    out.extend_from_slice(&pid.to_le_bytes()); // the receiving socket's port
    out.extend_from_slice(body);
    out.resize(align4(out.len()), 0);
}

fn link_message(out: &mut Vec<u8>, seq: u32, pid: u32, l: &Link) {
    let mut body = Vec::new();
    // struct ifinfomsg: family, pad, type, index, flags, change
    body.extend_from_slice(&[AF_UNSPEC, 0]);
    body.extend_from_slice(&l.kind.to_le_bytes());
    body.extend_from_slice(&(l.index as i32).to_le_bytes());
    body.extend_from_slice(&l.flags.to_le_bytes());
    body.extend_from_slice(&0xffff_ffffu32.to_le_bytes());
    let mut name = l.name.to_vec();
    name.push(0);
    attr(&mut body, 3, &name); // IFLA_IFNAME
    attr(&mut body, 1, &l.mac); // IFLA_ADDRESS
    if l.kind == 1 {
        attr(&mut body, 2, &[0xff; 6]); // IFLA_BROADCAST
    }
    attr(&mut body, 4, &l.mtu.to_le_bytes()); // IFLA_MTU
    message(out, RTM_NEWLINK, NLM_F_MULTI, seq, pid, &body);
}

fn addr_message(out: &mut Vec<u8>, seq: u32, pid: u32, l: &Link, addr: [u8; 4], prefix: u8) {
    let mut body = Vec::new();
    // struct ifaddrmsg: family, prefixlen, flags (IFA_F_PERMANENT), scope, index
    let scope = if l.kind == 772 { 254 } else { 0 }; // host / universe
    body.extend_from_slice(&[AF_INET, prefix, 0x80, scope]);
    body.extend_from_slice(&l.index.to_le_bytes());
    attr(&mut body, 1, &addr); // IFA_ADDRESS
    attr(&mut body, 2, &addr); // IFA_LOCAL
    if l.kind == 1 {
        let mask = if prefix == 0 { 0 } else { u32::MAX << (32 - prefix as u32) };
        let bcast = (u32::from_be_bytes(addr) | !mask).to_be_bytes();
        attr(&mut body, 4, &bcast); // IFA_BROADCAST
    }
    let mut name = l.name.to_vec();
    name.push(0);
    attr(&mut body, 3, &name); // IFA_LABEL
    message(out, RTM_NEWADDR, NLM_F_MULTI, seq, pid, &body);
}

/// An IPv6 address (as Linux reports them: no label or local address).
fn addr6_message(out: &mut Vec<u8>, seq: u32, pid: u32, index: u32, addr: [u8; 16], prefix: u8) {
    let mut body = Vec::new();
    // struct ifaddrmsg: family, prefixlen, flags (IFA_F_PERMANENT), scope, index
    let scope = if addr == [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1] {
        254 // host
    } else if addr[0] == 0xfe && addr[1] & 0xc0 == 0x80 {
        253 // link
    } else {
        0 // universe
    };
    body.extend_from_slice(&[AF_INET6, prefix, 0x80, scope]);
    body.extend_from_slice(&index.to_le_bytes());
    attr(&mut body, 1, &addr); // IFA_ADDRESS
    message(out, RTM_NEWADDR, NLM_F_MULTI, seq, pid, &body);
}

/// Routes in the main table: each interface's own network, and the
/// default route through the gateway.
fn route_messages(out: &mut Vec<u8>, seq: u32, pid: u32, l: &Link) {
    const RT_TABLE_MAIN: u8 = 254;
    const RTPROT_KERNEL: u8 = 2;
    const RTPROT_DHCP: u8 = 16;
    const RTN_UNICAST: u8 = 1;
    let Some((addr, prefix)) = l.addr else { return };
    if l.kind == 772 {
        return; // loopback routes live in the local table
    }
    // struct rtmsg: family, dst_len, src_len, tos, table, protocol, scope, type, flags
    let route = |out: &mut Vec<u8>, dst: Option<([u8; 4], u8)>, gw: Option<[u8; 4]>, proto: u8, scope: u8| {
        let mut body = Vec::new();
        body.extend_from_slice(&[AF_INET, dst.map_or(0, |d| d.1), 0, 0, RT_TABLE_MAIN, proto, scope, RTN_UNICAST]);
        body.extend_from_slice(&0u32.to_le_bytes());
        attr(&mut body, 15, &(RT_TABLE_MAIN as u32).to_le_bytes()); // RTA_TABLE
        if let Some((d, _)) = dst {
            attr(&mut body, 1, &d); // RTA_DST
        }
        if let Some(g) = gw {
            attr(&mut body, 5, &g); // RTA_GATEWAY
        }
        attr(&mut body, 7, &addr); // RTA_PREFSRC
        attr(&mut body, 4, &l.index.to_le_bytes()); // RTA_OIF
        message(out, RTM_NEWROUTE, NLM_F_MULTI, seq, pid, &body);
    };
    // The routes through a gateway: the default one, and any added.
    const RTPROT_STATIC: u8 = 4;
    for (n, dst, len, gw) in super::ipv4_routes() {
        if 2 + n as u32 != l.index {
            continue; // (another card's)
        }
        let proto = if len == 0 && l.gateway == Some(gw) { RTPROT_DHCP } else { RTPROT_STATIC };
        route(out, (len > 0).then_some((dst, len)), Some(gw), proto, 0);
    }
    let mask = if prefix == 0 { 0 } else { u32::MAX << (32 - prefix as u32) };
    let net = (u32::from_be_bytes(addr) & mask).to_be_bytes();
    route(out, Some((net, prefix)), None, RTPROT_KERNEL, 253); // scope link
}

/// An IPv6 route on eth0: to `dst`/`prefix`, on the link or through `gw`
/// (learned from router advertisements: protocol "ra").
fn route6_message(out: &mut Vec<u8>, seq: u32, pid: u32, oif: u32, dst: [u8; 16], prefix: u8, gw: Option<[u8; 16]>) {
    const RT_TABLE_MAIN: u8 = 254;
    const RTPROT_KERNEL: u8 = 2;
    const RTPROT_RA: u8 = 9;
    const RTN_UNICAST: u8 = 1;
    let mut body = Vec::new();
    // struct rtmsg: family, dst_len, src_len, tos, table, protocol, scope (universe), type, flags
    let proto = if gw.is_some() { RTPROT_RA } else { RTPROT_KERNEL };
    body.extend_from_slice(&[AF_INET6, prefix, 0, 0, RT_TABLE_MAIN, proto, 0, RTN_UNICAST]);
    body.extend_from_slice(&0u32.to_le_bytes());
    attr(&mut body, 15, &(RT_TABLE_MAIN as u32).to_le_bytes()); // RTA_TABLE
    if prefix > 0 {
        attr(&mut body, 1, &dst); // RTA_DST
    }
    if let Some(g) = gw {
        attr(&mut body, 5, &g); // RTA_GATEWAY
    }
    attr(&mut body, 4, &oif.to_le_bytes()); // RTA_OIF
    attr(&mut body, 6, &(if gw.is_some() { 1024u32 } else { 256 }).to_le_bytes()); // RTA_PRIORITY (Linux's metrics)
    message(out, RTM_NEWROUTE, NLM_F_MULTI, seq, pid, &body);
}

/// The interface ioctls on `req` (struct ifreq: a 16-byte name, then the
/// answer). None: not one of these.
pub fn interface_ioctl(request: u64, req: &mut [u8; 40]) -> Option<Result<(), i64>> {
    const SIOCGIFNAME: u64 = 0x8910;
    const SIOCGIFFLAGS: u64 = 0x8913;
    const SIOCGIFADDR: u64 = 0x8915;
    const SIOCGIFNETMASK: u64 = 0x891b;
    const SIOCGIFMTU: u64 = 0x8921;
    const SIOCGIFHWADDR: u64 = 0x8927;
    const SIOCGIFINDEX: u64 = 0x8933;
    const SIOCGIFTXQLEN: u64 = 0x8942;
    const SIOCGIFDSTADDR: u64 = 0x8917;
    const SIOCGIFBRDADDR: u64 = 0x8919;
    const SIOCGIFMETRIC: u64 = 0x891d;
    const SIOCGIFMAP: u64 = 0x8970;
    // Changes (ifconfig eth0 up/down, ADDR, netmask MASK): root only.
    const SIOCSIFFLAGS: u64 = 0x8914;
    const SIOCSIFADDR: u64 = 0x8916;
    const SIOCSIFNETMASK: u64 = 0x891c;
    const ENODEV: i64 = 19;
    const EPERM: i64 = 1;
    const EINVAL: i64 = 22;
    const EADDRNOTAVAIL: i64 = 99;
    const GETS: [u64; 12] = [
        SIOCGIFNAME, SIOCGIFFLAGS, SIOCGIFADDR, SIOCGIFNETMASK, SIOCGIFMTU, SIOCGIFHWADDR, SIOCGIFINDEX, SIOCGIFTXQLEN,
        SIOCGIFDSTADDR, SIOCGIFBRDADDR, SIOCGIFMETRIC, SIOCGIFMAP,
    ];
    if [SIOCSIFFLAGS, SIOCSIFADDR, SIOCSIFNETMASK].contains(&request) {
        let end = req[..16].iter().position(|&c| c == 0).unwrap_or(16);
        let name = &req[..end];
        // lo, or card n (eth0, eth1) if there is one.
        let card = match name {
            b"lo" => None,
            [b'e', b't', b'h', d @ b'0'..=b'9'] if ((d - b'0') as usize) < super::cards() => Some((d - b'0') as usize),
            _ => return Some(Err(ENODEV)),
        };
        if !crate::process::current_cred().is_root() {
            return Some(Err(EPERM));
        }
        let ipv4 = |req: &[u8; 40]| -> Option<[u8; 4]> {
            (u16::from_le_bytes([req[16], req[17]]) == AF_INET as u16).then(|| req[20..24].try_into().unwrap())
        };
        return Some(match (request, card) {
            (SIOCSIFFLAGS, Some(n)) => {
                let up = u16::from_le_bytes([req[16], req[17]]) & 1 != 0;
                super::ETH_UP[n].store(up, core::sync::atomic::Ordering::SeqCst);
                Ok(())
            }
            (SIOCSIFFLAGS, None) => Ok(()), // (lo stays up)
            (_, None) => Err(EPERM), // (lo's address is fixed)
            (SIOCSIFADDR, Some(n)) => match ipv4(req) {
                Some(a) if super::set_ipv4(n, Some(a), None) => Ok(()),
                Some(_) => Err(ENODEV),
                None => Err(EINVAL),
            },
            _ => match ipv4(req) {
                Some(m) => {
                    let m = u32::from_be_bytes(m);
                    if m.leading_ones() + m.trailing_zeros() != 32 {
                        Err(EINVAL) // not a netmask
                    } else if super::set_ipv4(card.unwrap_or(0), None, Some(m.leading_ones() as u8)) {
                        Ok(())
                    } else {
                        Err(ENODEV)
                    }
                }
                None => Err(EINVAL),
            },
        });
    }
    if !GETS.contains(&request) {
        return None;
    }
    let all = links();
    let link = if request == SIOCGIFNAME {
        let index = u32::from_le_bytes(req[16..20].try_into().unwrap());
        all.iter().find(|l| l.index == index)
    } else {
        let end = req[..16].iter().position(|&c| c == 0).unwrap_or(16);
        all.iter().find(|l| l.name == &req[..end])
    };
    let Some(l) = link else { return Some(Err(ENODEV)) };
    let sockaddr_in = |a: [u8; 4]| {
        let mut sa = [0u8; 16];
        sa[0] = AF_INET;
        sa[4..8].copy_from_slice(&a);
        sa
    };
    let answer = &mut req[16..];
    answer.fill(0);
    match request {
        SIOCGIFNAME => {
            let mut name = [0u8; 16];
            name[..l.name.len()].copy_from_slice(l.name);
            req[..16].copy_from_slice(&name);
            req[16..20].copy_from_slice(&l.index.to_le_bytes());
        }
        SIOCGIFINDEX => answer[..4].copy_from_slice(&l.index.to_le_bytes()),
        SIOCGIFFLAGS => answer[..2].copy_from_slice(&(l.flags as u16).to_le_bytes()),
        SIOCGIFMTU => answer[..4].copy_from_slice(&l.mtu.to_le_bytes()),
        SIOCGIFTXQLEN => answer[..4].copy_from_slice(&1000u32.to_le_bytes()),
        SIOCGIFMETRIC | SIOCGIFMAP => {} // (metric 0; no I/O ports or memory to tell)
        SIOCGIFDSTADDR => return Some(Err(EADDRNOTAVAIL)), // (not a point-to-point link)
        SIOCGIFBRDADDR => {
            let Some((addr, prefix)) = l.addr else { return Some(Err(EADDRNOTAVAIL)) };
            if l.flags & 0x2 == 0 {
                return Some(Err(EADDRNOTAVAIL)); // (no broadcast on lo)
            }
            let mask = if prefix == 0 { 0 } else { u32::MAX << (32 - prefix as u32) };
            let b = (u32::from_be_bytes(addr) | !mask).to_be_bytes();
            answer[..16].copy_from_slice(&sockaddr_in(b));
        }
        SIOCGIFHWADDR => {
            answer[..2].copy_from_slice(&l.kind.to_le_bytes()); // sa_family = ARPHRD_*
            answer[2..8].copy_from_slice(&l.mac);
        }
        SIOCGIFADDR | SIOCGIFNETMASK => {
            let Some((addr, prefix)) = l.addr else { return Some(Err(EADDRNOTAVAIL)) };
            let mask = if prefix == 0 { 0 } else { u32::MAX << (32 - prefix as u32) };
            let a = if request == SIOCGIFADDR { addr } else { mask.to_be_bytes() };
            answer[..16].copy_from_slice(&sockaddr_in(a));
        }
        _ => unreachable!(),
    }
    Some(Ok(()))
}

impl NetlinkSocket {
    pub fn new(nonblocking: bool) -> NetlinkSocket {
        NetlinkSocket {
            replies: Mutex::new(VecDeque::new()),
            nonblocking: AtomicBool::new(nonblocking),
            port: crate::process::current_tgid() as u32,
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        }
    }

    pub fn set_nonblocking(&self, on: bool) {
        self.nonblocking.store(on, Ordering::Relaxed);
    }

    pub fn is_nonblocking(&self) -> bool {
        self.nonblocking.load(Ordering::Relaxed)
    }

    /// A request: answer it at once (the replies wait for recv()). A dump
    /// comes as one buffer of entries, then NLMSG_DONE in its own (as
    /// Linux sends them: programs may stop reading entries early).
    pub fn send(&self, data: &[u8]) -> Result<usize, i64> {
        if data.len() < 16 {
            return Err(EINVAL);
        }
        let ty = u16::from_le_bytes([data[4], data[5]]);
        let flags = u16::from_le_bytes([data[6], data[7]]);
        let seq = u32::from_le_bytes(data[8..12].try_into().unwrap());
        let family = data.get(16).copied().unwrap_or(AF_UNSPEC);
        let pid = self.port;
        let ipv4 = family == AF_UNSPEC || family == AF_INET;
        let ipv6 = family == AF_UNSPEC || family == AF_INET6;
        let mut out = Vec::new();
        match ty {
            RTM_GETLINK => {
                for l in links() {
                    link_message(&mut out, seq, pid, &l);
                }
            }
            RTM_GETADDR if ipv4 || ipv6 => {
                if ipv4 {
                    for l in links() {
                        if let Some((addr, prefix)) = l.addr {
                            addr_message(&mut out, seq, pid, &l, addr, prefix);
                        }
                    }
                }
                if ipv6 {
                    for (index, addr, prefix) in super::ipv6_addresses() {
                        addr6_message(&mut out, seq, pid, index, addr.octets(), prefix);
                    }
                }
            }
            RTM_GETROUTE if ipv4 || ipv6 => {
                if ipv4 {
                    for l in links() {
                        route_messages(&mut out, seq, pid, &l);
                    }
                }
                if ipv6 {
                    for (oif, dst, prefix, gw) in super::ipv6_routes() {
                        route6_message(&mut out, seq, pid, oif, dst, prefix, gw);
                    }
                }
            }
            // ip route add / del (IPv4, through a gateway): root only.
            RTM_NEWROUTE | RTM_DELROUTE if family == AF_INET => {
                let result = if !crate::process::current_cred().is_root() { Err(crate::syscall::EPERM) } else { route_request(ty, data) };
                // An acknowledgement (error 0) if asked for, or the error.
                if result.is_err() || flags & NLM_F_ACK != 0 {
                    let mut body = (-result.err().unwrap_or(0) as i32).to_le_bytes().to_vec();
                    body.extend_from_slice(&data[..16]);
                    message(&mut out, NLMSG_ERROR, 0, seq, pid, &body);
                    self.replies.lock().push_back(out);
                }
                return Ok(data.len());
            }
            _ if flags & NLM_F_DUMP == NLM_F_DUMP => {} // nothing to list (IPv6...)
            _ => {
                // struct nlmsgerr: -errno, then the request's header.
                let mut body = (-crate::syscall::EOPNOTSUPP as i32).to_le_bytes().to_vec();
                body.extend_from_slice(&data[..16]);
                message(&mut out, NLMSG_ERROR, 0, seq, pid, &body);
                self.replies.lock().push_back(out);
                return Ok(data.len());
            }
        }
        let mut done = Vec::new();
        message(&mut done, NLMSG_DONE, NLM_F_MULTI, seq, pid, &0i32.to_le_bytes());
        let mut replies = self.replies.lock();
        if !out.is_empty() {
            replies.push_back(out);
        }
        replies.push_back(done);
        Ok(data.len())
    }

    /// The next reply (truncated to `buf`, like a datagram).
    pub fn recv(&self, buf: &mut [u8], flags: u64) -> Result<usize, i64> {
        loop {
            if let Some(reply) = self.replies.lock().pop_front() {
                let n = reply.len().min(buf.len());
                buf[..n].copy_from_slice(&reply[..n]);
                return Ok(n);
            }
            // Replies are made by send(): with none pending, nothing will come.
            if self.is_nonblocking() || flags & MSG_DONTWAIT != 0 {
                return Err(EAGAIN);
            }
            if crate::process::signal_pending() {
                return Err(EINTR);
            }
            crate::process::block_on(self as *const NetlinkSocket as u64);
        }
    }

    pub fn readiness(&self) -> (bool, bool, bool) {
        (!self.replies.lock().is_empty(), true, false)
    }

    pub fn pending_bytes(&self) -> usize {
        self.replies.lock().front().map_or(0, Vec::len)
    }
}

/// SIOCGIFCONF's answer: an ifreq (name, IPv4 address) per interface with
/// an IPv4 address, at most `room` bytes of them.
pub fn interface_list(room: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for l in links() {
        let Some((addr, _)) = l.addr else { continue };
        if out.len() + 40 > room {
            break;
        }
        let mut r = [0u8; 40];
        r[..l.name.len()].copy_from_slice(l.name);
        r[16] = AF_INET;
        r[20..24].copy_from_slice(&addr);
        out.extend_from_slice(&r);
    }
    out
}

/// The interfaces, for /proc/net/dev: (name, interface index).
pub fn link_names() -> Vec<(&'static str, u32)> {
    links().iter().map(|l| (core::str::from_utf8(l.name).unwrap_or("?"), l.index)).collect()
}

/// RTM_NEWROUTE / RTM_DELROUTE: struct rtmsg (destination prefix length
/// at byte 17 of the message), then attributes (RTA_DST, RTA_GATEWAY).
fn route_request(ty: u16, data: &[u8]) -> Result<(), i64> {
    const EINVAL: i64 = 22;
    let len = (u32::from_le_bytes(data[..4].try_into().unwrap()) as usize).min(data.len());
    let prefix = *data.get(17).ok_or(EINVAL)?;
    let (mut dst, mut gw) = ([0u8; 4], None);
    let mut at = 28;
    while at + 4 <= len {
        let alen = u16::from_le_bytes([data[at], data[at + 1]]) as usize;
        let aty = u16::from_le_bytes([data[at + 2], data[at + 3]]);
        if alen < 4 || at + alen > len {
            break;
        }
        let v = &data[at + 4..at + alen];
        match (aty, v.len()) {
            (1, 4) => dst.copy_from_slice(v), // RTA_DST
            (5, 4) => gw = Some(<[u8; 4]>::try_from(v).unwrap()), // RTA_GATEWAY
            _ => {}
        }
        at += align4(alen);
    }
    if ty == RTM_NEWROUTE {
        super::add_ipv4_route(dst, prefix, gw.ok_or(EINVAL)?) // (on-link routes: not supported)
    } else {
        super::del_ipv4_route(dst, prefix, gw)
    }
}

/// route add / route del (SIOCADDRT, SIOCDELRT): struct rtentry (rt_dst
/// at 8, rt_gateway at 24, rt_genmask at 40, rt_flags at 56). None: not
/// one of these.
pub fn route_ioctl(request: u64, rt: &[u8]) -> Option<Result<(), i64>> {
    const SIOCADDRT: u64 = 0x890B;
    const SIOCDELRT: u64 = 0x890C;
    const RTF_GATEWAY: u16 = 2;
    const RTF_HOST: u16 = 4;
    const EINVAL: i64 = 22;
    if request != SIOCADDRT && request != SIOCDELRT {
        return None;
    }
    if !crate::process::current_cred().is_root() {
        return Some(Err(crate::syscall::EPERM));
    }
    if rt.len() < 120 {
        return Some(Err(EINVAL));
    }
    let ipv4 = |off: usize| -> [u8; 4] { rt[off + 4..off + 8].try_into().unwrap() };
    let flags = u16::from_le_bytes([rt[56], rt[57]]);
    let dst = ipv4(8);
    let mask = u32::from_be_bytes(ipv4(40));
    let prefix = if flags & RTF_HOST != 0 { 32 } else { mask.leading_ones() as u8 };
    let gw = (flags & RTF_GATEWAY != 0).then(|| ipv4(24));
    Some(if request == SIOCADDRT {
        match gw {
            Some(g) => super::add_ipv4_route(dst, prefix, g),
            None => Err(EINVAL), // (on-link routes: not supported)
        }
    } else {
        super::del_ipv4_route(dst, prefix, gw)
    })
}

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
const RTM_GETROUTE: u16 = 26;
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
    let eth = super::NET.lock().as_ref().and_then(|n| n.eth.as_ref()).map(|e| (e.dev.mac, e.config));
    if let Some((mac, config)) = eth {
        v.push(Link {
            index: 2,
            name: b"eth0",
            flags: IFF_UP | IFF_BROADCAST | IFF_RUNNING | IFF_MULTICAST | IFF_LOWER_UP,
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
    if let Some(gw) = l.gateway {
        route(out, None, Some(gw), RTPROT_DHCP, 0);
    }
    let mask = if prefix == 0 { 0 } else { u32::MAX << (32 - prefix as u32) };
    let net = (u32::from_be_bytes(addr) & mask).to_be_bytes();
    route(out, Some((net, prefix)), None, RTPROT_KERNEL, 253); // scope link
}

/// An IPv6 route on eth0: to `dst`/`prefix`, on the link or through `gw`
/// (learned from router advertisements: protocol "ra").
fn route6_message(out: &mut Vec<u8>, seq: u32, pid: u32, dst: [u8; 16], prefix: u8, gw: Option<[u8; 16]>) {
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
    attr(&mut body, 4, &2u32.to_le_bytes()); // RTA_OIF: eth0
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
    const ENODEV: i64 = 19;
    const EADDRNOTAVAIL: i64 = 99;
    if ![SIOCGIFNAME, SIOCGIFFLAGS, SIOCGIFADDR, SIOCGIFNETMASK, SIOCGIFMTU, SIOCGIFHWADDR, SIOCGIFINDEX, SIOCGIFTXQLEN].contains(&request) {
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
                    for (dst, prefix, gw) in super::ipv6_routes() {
                        route6_message(&mut out, seq, pid, dst, prefix, gw);
                    }
                }
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

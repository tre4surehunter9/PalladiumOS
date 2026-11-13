// Copyright (c) 2026 tre4surehunter9
//! BSD sockets on smoltcp: AF_INET and AF_INET6, TCP (SOCK_STREAM), UDP
//! (SOCK_DGRAM) and raw IP (SOCK_RAW, root only: ping uses it).
//!
//! An AF_INET6 socket also talks IPv4, as on Linux: IPv4 addresses are
//! given to and from it as IPv4-mapped ones (::ffff:a.b.c.d; vfs.rs turns
//! them into IPv4 on the way in). A socket on the unspecified address
//! listens on both.
//!
//! Raw sockets work as on Linux: received IPv4 packets come with their
//! header, IPv6 ones (ICMPv6, for ping6) without; sent data gets one from
//! the kernel (source address of the interface, the socket's TTL or hop
//! limit; ICMPv6's checksum too), unless IP_HDRINCL is set (or the protocol
//! is IPPROTO_RAW): then the program supplies it.
//!
//! A socket file descriptor holds an `Arc<Socket>`. The smoltcp socket(s)
//! behind it are created when the address is known: `connect` picks the
//! interface by destination (127.x.x.x: loopback, else eth0), `bind` /
//! `listen` on 0.0.0.0 cover both. A listening socket keeps a few smoltcp
//! sockets in LISTEN (each takes one connection: `accept` hands it over and
//! puts a new one in its place).
//!
//! Blocking calls poll the stack and wait for network activity (or one
//! timer tick, for TCP's timers). Non-blocking ones (SOCK_NONBLOCK,
//! O_NONBLOCK, MSG_DONTWAIT) return EAGAIN instead of waiting.

use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};
use smoltcp::iface::SocketHandle;
use smoltcp::socket::{raw, tcp, udp};
use smoltcp::wire::{IpAddress, IpEndpoint, IpListenEndpoint, IpProtocol, IpVersion, Ipv4Address, Ipv6Address};
use spin::Mutex;

use super::{Net, NET, WAIT_KEY};
use crate::process;
use crate::syscall::{EAGAIN, EINTR, EINVAL, EPIPE};

pub const AF_INET: u64 = 2;
pub const AF_INET6: u64 = 10;
const IPPROTO_ICMPV6: u8 = 58;
pub const SOCK_STREAM: u64 = 1;
pub const SOCK_DGRAM: u64 = 2;
pub const SOCK_RAW: u64 = 3;
const IPPROTO_RAW: u8 = 255;
pub const SOCK_NONBLOCK: u64 = 0o4000;
pub const SOCK_CLOEXEC: u64 = 0o2000000;
const MSG_DONTWAIT: u64 = 0x40;
const MSG_NOSIGNAL: u64 = 0x4000;

const EADDRINUSE: i64 = 98;
const ENETUNREACH: i64 = 101;
const ECONNRESET: i64 = 104;
const EISCONN: i64 = 106;
const ENOTCONN: i64 = 107;
const ETIMEDOUT: i64 = 110;
const ECONNREFUSED: i64 = 111;
const EALREADY: i64 = 114;
const EINPROGRESS: i64 = 115;
const EDESTADDRREQ: i64 = 89;
const EOPNOTSUPP: i64 = 95;

const TCP_BUFFER: usize = 64 * 1024;
const UDP_BUFFER: usize = 16 * 1024;
const BACKLOG: usize = 4;
/// How long connect() waits for an answer.
const CONNECT_TIMEOUT_MS: u64 = 20_000;

/// Which interface a smoltcp socket lives on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Which {
    Eth,
    Lo,
}

enum State {
    /// Created; maybe bound (address, port), not yet used.
    Fresh { bound: Option<(IpAddress, u16)> },
    /// A TCP connection (or connecting).
    Tcp { which: Which, handle: SocketHandle },
    /// A TCP server: smoltcp sockets in LISTEN on each interface.
    Listening { addr: Option<IpAddress>, port: u16, waiting: Vec<(Which, SocketHandle)> },
    /// UDP: one smoltcp socket per interface it's bound on.
    Udp { port: u16, handles: Vec<(Which, SocketHandle)>, peer: Option<IpEndpoint> },
    /// Raw IP for one protocol: a smoltcp raw socket on each interface.
    Raw { protocol: u8, handles: Vec<(Which, SocketHandle)>, peer: Option<IpAddress>, hdrincl: bool, ttl: u8 },
    Closed,
}

pub struct Socket {
    /// AF_INET or AF_INET6.
    pub family: u64,
    stream: bool,
    state: Mutex<State>,
    nonblocking: AtomicBool,
    /// A connect() error to report through SO_ERROR (non-blocking connect).
    error: AtomicU64,
    pub id: u64,
}

static NEXT_PORT: AtomicU16 = AtomicU16::new(49152);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn ephemeral_port() -> u16 {
    let p = NEXT_PORT.fetch_add(1, Ordering::Relaxed);
    if p < 49152 {
        NEXT_PORT.store(49153, Ordering::Relaxed);
        49152
    } else {
        p
    }
}

/// The interface for talking to `addr`.
fn route(addr: IpAddress) -> Which {
    let lo = match addr {
        IpAddress::Ipv4(a) => a.octets()[0] == 127,
        IpAddress::Ipv6(a) => a.is_loopback(),
    };
    if lo { Which::Lo } else { Which::Eth }
}

/// A family's unspecified address (0.0.0.0, ::).
fn unspecified(family: u64) -> IpAddress {
    if family == AF_INET6 { IpAddress::Ipv6(Ipv6Address::UNSPECIFIED) } else { IpAddress::Ipv4(Ipv4Address::UNSPECIFIED) }
}

/// May a socket of `family` talk to `addr`? (AF_INET6: both; AF_INET:
/// IPv4 only. A listener or UDP socket on 0.0.0.0 hears IPv6 too in
/// smoltcp; those are turned away.)
fn family_ok(family: u64, addr: IpAddress) -> bool {
    family == AF_INET6 || matches!(addr, IpAddress::Ipv4(_))
}

/// Run `f` on an interface's sockets (and the interface).
fn with_set<T>(
    net: &mut Net,
    which: Which,
    f: impl FnOnce(&mut smoltcp::iface::Interface, &mut smoltcp::iface::SocketSet<'static>) -> T,
) -> Result<T, i64> {
    match which {
        Which::Lo => Ok(f(&mut net.lo.iface, &mut net.lo.sockets)),
        Which::Eth => match net.eth.as_mut() {
            Some(eth) => Ok(f(&mut eth.iface, &mut eth.sockets)),
            None => Err(ENETUNREACH),
        },
    }
}

fn new_tcp() -> tcp::Socket<'static> {
    tcp::Socket::new(tcp::SocketBuffer::new(vec![0; TCP_BUFFER]), tcp::SocketBuffer::new(vec![0; TCP_BUFFER]))
}

fn new_udp() -> udp::Socket<'static> {
    udp::Socket::new(
        udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 16], vec![0; UDP_BUFFER]),
        udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 16], vec![0; UDP_BUFFER]),
    )
}

/// Interfaces a socket bound to `addr` (None: 0.0.0.0) listens on.
fn interfaces_for(net: &Net, addr: Option<IpAddress>) -> Vec<Which> {
    match addr {
        Some(a) => vec![route(a)],
        None => {
            let mut v = vec![Which::Lo];
            if net.eth.is_some() {
                v.push(Which::Eth);
            }
            v
        }
    }
}

/// Wait until `ready` returns Some (or fails), polling the stack.
fn wait<T>(sock: &Socket, flags: u64, deadline: Option<u64>, mut ready: impl FnMut(&mut Net) -> Option<Result<T, i64>>) -> Result<T, i64> {
    loop {
        super::poll();
        {
            let mut guard = NET.lock();
            let net = guard.as_mut().ok_or(ENETUNREACH)?;
            if let Some(r) = ready(net) {
                return r;
            }
        }
        if sock.nonblocking.load(Ordering::Relaxed) || flags & MSG_DONTWAIT != 0 {
            return Err(EAGAIN);
        }
        if process::signal_pending() {
            return Err(EINTR);
        }
        if deadline.is_some_and(|d| process::ticks() >= d) {
            return Err(ETIMEDOUT);
        }
        // Network activity wakes us; so does the next tick (TCP timers).
        process::block_on_until(WAIT_KEY, process::ticks() + 1);
    }
}

impl Socket {
    pub fn new(family: u64, ty: u64, protocol: u64) -> Result<Socket, i64> {
        let stream = match ty & 0xf {
            SOCK_STREAM => true,
            SOCK_DGRAM => false,
            SOCK_RAW => return Self::new_raw(family, ty, protocol),
            _ => return Err(crate::syscall::EPROTONOSUPPORT),
        };
        Ok(Socket {
            family,
            stream,
            state: Mutex::new(State::Fresh { bound: None }),
            nonblocking: AtomicBool::new(ty & SOCK_NONBLOCK != 0),
            error: AtomicU64::new(0),
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        })
    }

    /// A raw socket for IP protocol `protocol` (root only, as Linux needs
    /// CAP_NET_RAW). It gets every packet of that protocol, on every interface.
    fn new_raw(family: u64, ty: u64, protocol: u64) -> Result<Socket, i64> {
        if !process::current_cred().is_root() {
            return Err(crate::syscall::EPERM);
        }
        if protocol == 0 || protocol > 255 {
            return Err(crate::syscall::EPROTONOSUPPORT);
        }
        let mut guard = NET.lock();
        let net = guard.as_mut().ok_or(ENETUNREACH)?;
        let mut handles = Vec::new();
        for which in interfaces_for(net, None) {
            let s = raw::Socket::new(
                Some(if family == AF_INET6 { IpVersion::Ipv6 } else { IpVersion::Ipv4 }),
                Some(IpProtocol::from(protocol as u8)),
                raw::PacketBuffer::new(vec![raw::PacketMetadata::EMPTY; 16], vec![0; UDP_BUFFER]),
                raw::PacketBuffer::new(vec![raw::PacketMetadata::EMPTY; 16], vec![0; UDP_BUFFER]),
            );
            handles.push((which, with_set(net, which, |_, set| set.add(s))?));
        }
        let protocol = protocol as u8;
        Ok(Socket {
            family,
            stream: false,
            state: Mutex::new(State::Raw { protocol, handles, peer: None, hdrincl: protocol == IPPROTO_RAW, ttl: 64 }),
            nonblocking: AtomicBool::new(ty & SOCK_NONBLOCK != 0),
            error: AtomicU64::new(0),
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        })
    }

    /// IP_HDRINCL / IP_TTL for raw sockets (other options are ignored).
    pub fn set_ip_option(&self, name: u64, value: u32) {
        const IP_TTL: u64 = 2;
        const IP_HDRINCL: u64 = 3;
        if let State::Raw { hdrincl, ttl, .. } = &mut *self.state.lock() {
            match name {
                IP_HDRINCL => *hdrincl = value != 0,
                IP_TTL if (1..=255).contains(&value) => *ttl = value as u8,
                _ => {}
            }
        }
    }

    pub fn is_raw(&self) -> bool {
        matches!(&*self.state.lock(), State::Raw { .. })
    }

    pub fn set_nonblocking(&self, on: bool) {
        self.nonblocking.store(on, Ordering::Relaxed);
    }

    pub fn is_nonblocking(&self) -> bool {
        self.nonblocking.load(Ordering::Relaxed)
    }

    pub fn is_stream(&self) -> bool {
        self.stream
    }

    /// bind(addr, port): port 0 picks one.
    pub fn bind(&self, addr: IpAddress, port: u16) -> Result<(), i64> {
        let mut state = self.state.lock();
        let State::Fresh { bound } = &mut *state else { return Err(EINVAL) };
        let port = if port == 0 { ephemeral_port() } else { port };
        let addr_opt = (!addr.is_unspecified()).then_some(addr);
        if self.stream {
            *bound = Some((addr, port));
            return Ok(());
        }
        // UDP: bind smoltcp sockets now (it can receive right away).
        let mut guard = NET.lock();
        let net = guard.as_mut().ok_or(ENETUNREACH)?;
        let mut handles = Vec::new();
        for which in interfaces_for(net, addr_opt) {
            let mut s = new_udp();
            s.bind(IpListenEndpoint { addr: addr_opt, port }).map_err(|_| EADDRINUSE)?;
            handles.push((which, with_set(net, which, |_, set| set.add(s))?));
        }
        *state = State::Udp { port, handles, peer: None };
        Ok(())
    }

    /// Make sure a UDP socket has smoltcp sockets (an implicit bind).
    fn ensure_udp(&self) -> Result<(), i64> {
        let fresh = matches!(&*self.state.lock(), State::Fresh { .. });
        if fresh {
            self.bind(unspecified(self.family), 0)?;
        }
        Ok(())
    }

    pub fn listen(&self, backlog: usize) -> Result<(), i64> {
        if !self.stream {
            return Err(EOPNOTSUPP);
        }
        let mut state = self.state.lock();
        let (addr, port) = match &*state {
            State::Fresh { bound: Some((a, p)) } => ((!a.is_unspecified()).then_some(*a), *p),
            State::Fresh { bound: None } => (None, ephemeral_port()),
            State::Listening { .. } => return Ok(()),
            _ => return Err(EINVAL),
        };
        let mut guard = NET.lock();
        let net = guard.as_mut().ok_or(ENETUNREACH)?;
        let mut waiting = Vec::new();
        for which in interfaces_for(net, addr) {
            for _ in 0..backlog.clamp(1, BACKLOG) {
                let mut s = new_tcp();
                s.listen(IpListenEndpoint { addr, port }).map_err(|_| EADDRINUSE)?;
                waiting.push((which, with_set(net, which, |_, set| set.add(s))?));
            }
        }
        *state = State::Listening { addr, port, waiting };
        Ok(())
    }

    /// accept(): the next connection, as a new socket, and its address.
    pub fn accept(&self, nonblocking: bool) -> Result<(Socket, Option<IpEndpoint>), i64> {
        let found = wait(self, 0, None, |net| {
            let mut state = self.state.lock();
            let State::Listening { addr, port, waiting } = &mut *state else { return Some(Err(EINVAL)) };
            // A listener that got a connection is no longer in LISTEN.
            let pos = waiting.iter().position(|&(which, h)| {
                with_set(net, which, |_, set| {
                    let s = set.get::<tcp::Socket>(h);
                    s.state() != tcp::State::Listen && s.state() != tcp::State::SynReceived
                })
                .unwrap_or(false)
            })?;
            let (which, handle) = waiting.remove(pos);
            // Put a fresh listener in its place.
            let mut s = new_tcp();
            let _ = s.listen(IpListenEndpoint { addr: *addr, port: *port });
            if let Ok(h) = with_set(net, which, |_, set| set.add(s)) {
                waiting.push((which, h));
            }
            let peer = with_set(net, which, |_, set| set.get::<tcp::Socket>(handle).remote_endpoint()).ok().flatten();
            if peer.is_some_and(|p| !family_ok(self.family, p.addr)) {
                // IPv6 on an IPv4 socket: turned away (reset), as if nobody listened.
                let _ = with_set(net, which, |_, set| set.get_mut::<tcp::Socket>(handle).abort());
                net.closing.push((which, handle));
                return None;
            }
            Some(Ok((which, handle, peer)))
        })?;
        let (which, handle, peer) = found;
        let sock = Socket {
            family: self.family,
            stream: true,
            state: Mutex::new(State::Tcp { which, handle }),
            nonblocking: AtomicBool::new(nonblocking),
            error: AtomicU64::new(0),
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        };
        Ok((sock, peer))
    }

    pub fn connect(&self, addr: IpAddress, port: u16) -> Result<(), i64> {
        if let State::Raw { peer, .. } = &mut *self.state.lock() {
            *peer = Some(addr); // raw: just the default destination
            return Ok(());
        }
        if !self.stream {
            // UDP: just the default destination.
            self.ensure_udp()?;
            if let State::Udp { peer, .. } = &mut *self.state.lock() {
                *peer = Some(IpEndpoint::new(addr, port));
            }
            return Ok(());
        }
        let which = route(addr);
        {
            let mut state = self.state.lock();
            let local_port = match &*state {
                State::Fresh { bound } => bound.map_or_else(ephemeral_port, |(_, p)| p),
                State::Tcp { which, handle } => {
                    // A second connect: done, still going, or failed.
                    let mut guard = NET.lock();
                    let net = guard.as_mut().ok_or(ENETUNREACH)?;
                    let st = with_set(net, *which, |_, set| set.get::<tcp::Socket>(*handle).state())?;
                    return match st {
                        tcp::State::Established => Err(EISCONN),
                        tcp::State::SynSent => Err(EALREADY),
                        _ => Err(ECONNREFUSED),
                    };
                }
                _ => return Err(EISCONN),
            };
            let mut guard = NET.lock();
            let net = guard.as_mut().ok_or(ENETUNREACH)?;
            let handle = with_set(net, which, |iface, set| {
                let mut s = new_tcp();
                s.connect(iface.context(), IpEndpoint::new(addr, port), local_port)
                    .map(|_| set.add(s))
                    .map_err(|_| ENETUNREACH)
            })??;
            *state = State::Tcp { which, handle };
        }
        if self.is_nonblocking() {
            super::poll();
            return Err(EINPROGRESS);
        }
        let deadline = process::ticks() + (CONNECT_TIMEOUT_MS * 1_000_000).div_ceil(process::TICK_NS);
        let r = wait(self, 0, Some(deadline), |net| match self.tcp_state(net) {
            Some(tcp::State::Established) | Some(tcp::State::CloseWait) => Some(Ok(())),
            Some(tcp::State::SynSent) | Some(tcp::State::SynReceived) => None,
            _ => Some(Err(ECONNREFUSED)),
        });
        if let Err(e) = r {
            self.error.store(e as u64, Ordering::Relaxed);
        }
        r
    }

    fn tcp_state(&self, net: &mut Net) -> Option<tcp::State> {
        match &*self.state.lock() {
            State::Tcp { which, handle } => with_set(net, *which, |_, set| set.get::<tcp::Socket>(*handle).state()).ok(),
            _ => None,
        }
    }

    /// recv / read. For UDP, also the sender.
    pub fn recv(&self, buf: &mut [u8], flags: u64) -> Result<(usize, Option<IpEndpoint>), i64> {
        if self.is_raw() {
            return self.recv_raw(buf, flags);
        }
        if !self.stream {
            self.ensure_udp()?;
        }
        let r = wait(self, flags, None, |net| {
            let state = self.state.lock();
            match &*state {
                State::Tcp { which, handle } => with_set(net, *which, |_, set| {
                    let s = set.get_mut::<tcp::Socket>(*handle);
                    if s.can_recv() {
                        return Some(s.recv_slice(buf).map(|n| (n, None)).map_err(|_| ECONNRESET));
                    }
                    if !s.may_recv() {
                        // The other side closed (or reset): end of file.
                        return Some(Ok((0, None)));
                    }
                    None
                })
                .unwrap_or(Some(Err(ENETUNREACH))),
                State::Udp { handles, .. } => {
                    let family = self.family;
                    for &(which, h) in handles {
                        let got = with_set(net, which, |_, set| {
                            let s = set.get_mut::<udp::Socket>(h);
                            while s.can_recv() {
                                match s.recv_slice(buf) {
                                    // (IPv6 for an IPv4 socket: dropped.)
                                    Ok((_, meta)) if !family_ok(family, meta.endpoint.addr) => continue,
                                    Ok((n, meta)) => return Some((n, Some(meta.endpoint))),
                                    Err(_) => return None,
                                }
                            }
                            None
                        });
                        if let Ok(Some(r)) = got {
                            return Some(Ok(r));
                        }
                    }
                    None
                }
                _ => Some(Err(ENOTCONN)),
            }
        });
        // Reading made room in the receive buffer: tell the sender now (a
        // window update). Otherwise, if the buffer had filled, the sender
        // waits until something else polls the stack (the next tick, or its
        // own slow retries), and downloads crawl.
        if self.stream && matches!(r, Ok((n, _)) if n > 0) {
            super::poll();
        }
        r
    }

    /// send / write (to `to`, for an unconnected UDP socket).
    pub fn send(&self, data: &[u8], flags: u64, to: Option<IpEndpoint>) -> Result<usize, i64> {
        if self.is_raw() {
            return self.send_raw(data, to);
        }
        if !self.stream {
            self.ensure_udp()?;
            let r = {
                let mut guard = NET.lock();
                let net = guard.as_mut().ok_or(ENETUNREACH)?;
                let state = self.state.lock();
                let State::Udp { handles, peer, .. } = &*state else { return Err(EINVAL) };
                let dest = to.or(*peer).ok_or(EDESTADDRREQ)?;
                let which = route(dest.addr);
                let &(_, h) = handles.iter().find(|(w, _)| *w == which).ok_or(ENETUNREACH)?;
                with_set(net, which, |_, set| set.get_mut::<udp::Socket>(h).send_slice(data, dest))?
                    .map(|_| data.len())
                    .map_err(|_| EAGAIN)
            };
            super::poll(); // send it now
            return r;
        }
        let mut sent = 0;
        while sent < data.len() {
            let n = wait(self, flags, None, |net| {
                let state = self.state.lock();
                let State::Tcp { which, handle } = &*state else { return Some(Err(ENOTCONN)) };
                with_set(net, *which, |_, set| {
                    let s = set.get_mut::<tcp::Socket>(*handle);
                    if !s.may_send() {
                        return Some(Err(EPIPE));
                    }
                    if s.can_send() {
                        return Some(s.send_slice(&data[sent..]).map_err(|_| EPIPE));
                    }
                    None
                })
                .unwrap_or(Some(Err(ENETUNREACH)))
            });
            match n {
                Ok(n) => sent += n,
                Err(EAGAIN) if sent > 0 => break,
                Err(EPIPE) if sent == 0 => {
                    if flags & MSG_NOSIGNAL == 0 {
                        process::raise(process::SIGPIPE);
                    }
                    return Err(EPIPE);
                }
                Err(e) if sent == 0 => return Err(e),
                Err(_) => break,
            }
            if self.is_nonblocking() || flags & MSG_DONTWAIT != 0 {
                break;
            }
        }
        super::poll(); // send it now
        Ok(sent)
    }

    /// A raw packet: IPv4 with its header, IPv6 without (as Linux gives
    /// them); the sender is its source.
    fn recv_raw(&self, buf: &mut [u8], flags: u64) -> Result<(usize, Option<IpEndpoint>), i64> {
        let v6 = self.family == AF_INET6;
        wait(self, flags, None, |net| {
            let state = self.state.lock();
            let State::Raw { handles, .. } = &*state else { return Some(Err(EINVAL)) };
            for &(which, h) in handles {
                if v6 {
                    let mut packet = [0u8; 1500];
                    let got = with_set(net, which, |_, set| {
                        let s = set.get_mut::<raw::Socket>(h);
                        if s.can_recv() { s.recv_slice(&mut packet).ok() } else { None }
                    });
                    if let Ok(Some(n)) = got {
                        if n < 40 {
                            continue;
                        }
                        let payload = &packet[40..n];
                        let k = payload.len().min(buf.len());
                        buf[..k].copy_from_slice(&payload[..k]);
                        let src: [u8; 16] = packet[8..24].try_into().unwrap();
                        return Some(Ok((k, Some(IpEndpoint::new(IpAddress::Ipv6(Ipv6Address::from(src)), 0)))));
                    }
                    continue;
                }
                let got = with_set(net, which, |_, set| {
                    let s = set.get_mut::<raw::Socket>(h);
                    if s.can_recv() { s.recv_slice(buf).ok() } else { None }
                });
                if let Ok(Some(n)) = got {
                    let from = (n >= 20).then(|| {
                        IpEndpoint::new(IpAddress::Ipv4(Ipv4Address::new(buf[12], buf[13], buf[14], buf[15])), 0)
                    });
                    return Some(Ok((n, from)));
                }
            }
            None
        })
    }

    /// Send a raw packet: put an IPv4 header in front (unless IP_HDRINCL).
    fn send_raw(&self, data: &[u8], to: Option<IpEndpoint>) -> Result<usize, i64> {
        static IP_ID: AtomicU16 = AtomicU16::new(1);
        let r = {
            let mut guard = NET.lock();
            let net = guard.as_mut().ok_or(ENETUNREACH)?;
            let state = self.state.lock();
            let State::Raw { protocol, handles, peer, hdrincl, ttl } = &*state else { return Err(EINVAL) };
            let dest = match to.map(|e| e.addr) {
                Some(a) => a,
                None => peer.ok_or(EDESTADDRREQ)?,
            };
            let which = route(dest);
            let dest = match dest {
                IpAddress::Ipv6(d) => {
                    // IPv6: a header from the kernel, and ICMPv6's checksum.
                    let source = ipv6_source(net, which, d).ok_or(ENETUNREACH)?;
                    let packet = ipv6_packet(source, d, *protocol, *ttl, data)?;
                    let &(_, h) = handles.iter().find(|(w, _)| *w == which).ok_or(ENETUNREACH)?;
                    let r = with_set(net, which, |_, set| set.get_mut::<raw::Socket>(h).send_slice(&packet))?
                        .map(|_| data.len())
                        .map_err(|_| EAGAIN);
                    drop(state);
                    drop(guard);
                    super::poll();
                    return r;
                }
                IpAddress::Ipv4(d) => d,
            };
            let source = match which {
                Which::Lo => Ipv4Address::new(127, 0, 0, 1),
                Which::Eth => net.eth.as_ref().and_then(|e| e.config).map(|c| c.address.address()).ok_or(ENETUNREACH)?,
            };
            let packet = if *hdrincl {
                data.to_vec()
            } else {
                let total = 20 + data.len();
                if total > 1500 {
                    return Err(crate::syscall::EMSGSIZE);
                }
                let mut p = vec![0u8; total];
                p[0] = 0x45; // IPv4, 20-byte header
                p[2..4].copy_from_slice(&(total as u16).to_be_bytes());
                p[4..6].copy_from_slice(&IP_ID.fetch_add(1, Ordering::Relaxed).to_be_bytes());
                p[8] = *ttl;
                p[9] = *protocol;
                p[12..16].copy_from_slice(&source.octets());
                p[16..20].copy_from_slice(&dest.octets());
                p[20..].copy_from_slice(data);
                p // (smoltcp fills in the header checksum)
            };
            let &(_, h) = handles.iter().find(|(w, _)| *w == which).ok_or(ENETUNREACH)?;
            with_set(net, which, |_, set| set.get_mut::<raw::Socket>(h).send_slice(&packet))?
                .map(|_| data.len())
                .map_err(|_| EAGAIN)
        };
        super::poll(); // send it now
        r
    }

    /// shutdown(how): 1 or 2 stop sending (TCP FIN).
    pub fn shutdown(&self, how: u64) -> Result<(), i64> {
        let mut guard = NET.lock();
        let net = guard.as_mut().ok_or(ENETUNREACH)?;
        match &*self.state.lock() {
            State::Tcp { which, handle } => {
                if how != 0 {
                    with_set(net, *which, |_, set| set.get_mut::<tcp::Socket>(*handle).close())?;
                }
                Ok(())
            }
            _ => Err(ENOTCONN),
        }
    }

    /// Our address (getsockname).
    pub fn local(&self) -> IpEndpoint {
        let unspec = IpEndpoint::new(unspecified(self.family), 0);
        let mut guard = NET.lock();
        let Some(net) = guard.as_mut() else { return unspec };
        match &*self.state.lock() {
            State::Fresh { bound: Some((a, p)) } => IpEndpoint::new(*a, *p),
            State::Tcp { which, handle } => {
                with_set(net, *which, |_, set| set.get::<tcp::Socket>(*handle).local_endpoint()).ok().flatten().unwrap_or(unspec)
            }
            State::Listening { addr, port, .. } => IpEndpoint::new(addr.unwrap_or(unspecified(self.family)), *port),
            // Connected: the address packets to the peer go from (as Linux
            // says; libc's getaddrinfo sorts addresses by it, RFC 6724).
            State::Udp { port, peer: Some(p), .. } => IpEndpoint::new(source_for(net, p.addr).unwrap_or(unspecified(self.family)), *port),
            State::Udp { port, .. } => IpEndpoint::new(unspecified(self.family), *port),
            _ => unspec,
        }
    }

    /// The other end (getpeername).
    pub fn peer(&self) -> Result<IpEndpoint, i64> {
        let mut guard = NET.lock();
        let net = guard.as_mut().ok_or(ENOTCONN)?;
        match &*self.state.lock() {
            State::Tcp { which, handle } => {
                with_set(net, *which, |_, set| set.get::<tcp::Socket>(*handle).remote_endpoint())?.ok_or(ENOTCONN)
            }
            State::Udp { peer: Some(p), .. } => Ok(*p),
            _ => Err(ENOTCONN),
        }
    }

    /// SO_ERROR: a failed non-blocking connect.
    pub fn take_error(&self) -> i64 {
        // A non-blocking connect that has since failed shows up here too.
        if let Some(mut guard) = NET.try_lock() {
            if let Some(net) = guard.as_mut() {
                if matches!(self.tcp_state(net), Some(tcp::State::Closed)) && self.error.load(Ordering::Relaxed) == 0 {
                    if matches!(&*self.state.lock(), State::Tcp { .. }) {
                        return ECONNREFUSED;
                    }
                }
            }
        }
        self.error.swap(0, Ordering::Relaxed) as i64
    }

    /// For poll(): (readable, writable, hung up).
    pub fn readiness(&self) -> (bool, bool, bool) {
        let Some(mut guard) = NET.try_lock() else { return (false, false, false) };
        let Some(net) = guard.as_mut() else { return (false, false, true) };
        match &*self.state.lock() {
            State::Tcp { which, handle } => with_set(net, *which, |_, set| {
                let s = set.get::<tcp::Socket>(*handle);
                let connecting = matches!(s.state(), tcp::State::SynSent | tcp::State::SynReceived);
                let closed = s.state() == tcp::State::Closed;
                // A closed socket (a refused or reset connection) is writable
                // too, as on Linux: a program waiting for a non-blocking
                // connect wakes up and finds the error with SO_ERROR.
                (s.can_recv() || (!s.may_recv() && !connecting), (s.can_send() && !connecting) || closed, closed)
            })
            .unwrap_or((false, false, true)),
            State::Listening { waiting, .. } => {
                let ready = waiting.iter().any(|&(which, h)| {
                    with_set(net, which, |_, set| {
                        let st = set.get::<tcp::Socket>(h).state();
                        st != tcp::State::Listen && st != tcp::State::SynReceived
                    })
                    .unwrap_or(false)
                });
                (ready, false, false)
            }
            State::Udp { handles, .. } => {
                let readable = handles
                    .iter()
                    .any(|&(which, h)| with_set(net, which, |_, set| set.get::<udp::Socket>(h).can_recv()).unwrap_or(false));
                (readable, true, false)
            }
            State::Raw { handles, .. } => {
                let readable = handles
                    .iter()
                    .any(|&(which, h)| with_set(net, which, |_, set| set.get::<raw::Socket>(h).can_recv()).unwrap_or(false));
                (readable, true, false)
            }
            State::Fresh { .. } => (false, !self.stream, false),
            State::Closed => (true, true, true),
        }
    }

    /// Bytes waiting to be read (FIONREAD).
    pub fn pending_bytes(&self) -> usize {
        let Some(mut guard) = NET.try_lock() else { return 0 };
        let Some(net) = guard.as_mut() else { return 0 };
        match &*self.state.lock() {
            State::Tcp { which, handle } => {
                with_set(net, *which, |_, set| set.get::<tcp::Socket>(*handle).recv_queue()).unwrap_or(0)
            }
            _ => 0,
        }
    }
}

impl Drop for Socket {
    /// The last descriptor closed: TCP says goodbye (FIN) and lingers until
    /// it's done (poll removes it); listeners and UDP go at once.
    fn drop(&mut self) {
        let mut guard = NET.lock();
        let Some(net) = guard.as_mut() else { return };
        match core::mem::replace(&mut *self.state.lock(), State::Closed) {
            State::Tcp { which, handle } => {
                // Like Linux: closing with unread data resets the connection
                // (RST); otherwise a normal goodbye (FIN).
                let _ = with_set(net, which, |_, set| {
                    let s = set.get_mut::<tcp::Socket>(handle);
                    if s.recv_queue() > 0 { s.abort() } else { s.close() }
                });
                net.closing.push((which, handle));
            }
            State::Listening { waiting, .. } => {
                for (which, h) in waiting {
                    let _ = with_set(net, which, |_, set| set.remove(h));
                }
            }
            State::Udp { handles, .. } | State::Raw { handles, .. } => {
                for (which, h) in handles {
                    let _ = with_set(net, which, |_, set| set.remove(h));
                }
            }
            _ => {}
        }
    }
}

/// The source address for packets to `dest`.
fn source_for(net: &Net, dest: IpAddress) -> Option<IpAddress> {
    let which = route(dest);
    match dest {
        IpAddress::Ipv6(d) => ipv6_source(net, which, d).map(IpAddress::Ipv6),
        IpAddress::Ipv4(_) => match which {
            Which::Lo => Some(IpAddress::Ipv4(Ipv4Address::new(127, 0, 0, 1))),
            Which::Eth => net.eth.as_ref().and_then(|e| e.config).map(|c| IpAddress::Ipv4(c.address.address())),
        },
    }
}

/// The source address for an IPv6 packet to `dest` on interface `which`:
/// ::1 on loopback; on eth0 a link-local address for a link-local
/// destination, else the global one (from SLAAC), if there is one.
fn ipv6_source(net: &Net, which: Which, dest: Ipv6Address) -> Option<Ipv6Address> {
    if which == Which::Lo {
        return Some(Ipv6Address::LOCALHOST);
    }
    let link_local = |a: &Ipv6Address| a.segments()[0] & 0xffc0 == 0xfe80;
    let addrs: Vec<Ipv6Address> = net
        .eth
        .as_ref()?
        .iface
        .ip_addrs()
        .iter()
        .filter_map(|c| match c {
            smoltcp::wire::IpCidr::Ipv6(c) => Some(c.address()),
            _ => None,
        })
        .collect();
    if link_local(&dest) {
        addrs.iter().find(|a| link_local(a)).copied()
    } else {
        addrs.iter().find(|a| !link_local(a)).or(addrs.first()).copied()
    }
}

/// An IPv6 packet: a 40-byte header (`next` protocol, hop limit) and
/// `data`, with ICMPv6's checksum filled in (the kernel computes it for raw
/// ICMPv6 sockets, as Linux does).
fn ipv6_packet(src: Ipv6Address, dst: Ipv6Address, next: u8, hops: u8, data: &[u8]) -> Result<Vec<u8>, i64> {
    if 40 + data.len() > 1500 {
        return Err(crate::syscall::EMSGSIZE);
    }
    let mut p = vec![0u8; 40 + data.len()];
    p[0] = 0x60; // version 6
    p[4..6].copy_from_slice(&(data.len() as u16).to_be_bytes());
    p[6] = next;
    p[7] = hops;
    p[8..24].copy_from_slice(&src.octets());
    p[24..40].copy_from_slice(&dst.octets());
    p[40..].copy_from_slice(data);
    if next == IPPROTO_ICMPV6 && data.len() >= 4 {
        p[42] = 0;
        p[43] = 0;
        // Over the pseudo-header (addresses, length, protocol) and the message.
        let mut sum: u32 = 0;
        let mut add = |bytes: &[u8]| {
            for pair in bytes.chunks(2) {
                sum += u32::from(u16::from_be_bytes([pair[0], *pair.get(1).unwrap_or(&0)]));
            }
        };
        add(&src.octets());
        add(&dst.octets());
        add(&(data.len() as u32).to_be_bytes());
        add(&[0, 0, 0, next]);
        add(&p[40..]);
        while sum >> 16 != 0 {
            sum = (sum & 0xffff) + (sum >> 16);
        }
        let check = !(sum as u16);
        p[42..44].copy_from_slice(&check.to_be_bytes());
    }
    Ok(p)
}

/// Remove closed TCP sockets whose goodbye has finished (called by poll).
/// Data arriving for a closed socket nobody can read resets the connection,
/// as on Linux: the sender's next write fails with EPIPE.
pub fn reap(net: &mut Net) {
    let mut i = 0;
    while i < net.closing.len() {
        let (which, h) = net.closing[i];
        let done = with_set(net, which, |_, set| {
            let s = set.get_mut::<tcp::Socket>(h);
            if s.recv_queue() > 0 && s.state() != tcp::State::Closed {
                // Reset it, and keep it until the next pass: the reset
                // goes out when the interface is polled after this.
                s.abort();
                return false;
            }
            matches!(s.state(), tcp::State::Closed | tcp::State::TimeWait)
        })
        .unwrap_or(true);
        if done {
            let _ = with_set(net, which, |_, set| set.remove(h));
            net.closing.swap_remove(i);
        } else {
            i += 1;
        }
    }
}

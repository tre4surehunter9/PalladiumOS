// Copyright (c) 2026 tre4surehunter9
//! BSD sockets on smoltcp: AF_INET, TCP (SOCK_STREAM) and UDP (SOCK_DGRAM).
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
use smoltcp::socket::{tcp, udp};
use smoltcp::wire::{IpAddress, IpEndpoint, IpListenEndpoint, Ipv4Address};
use spin::Mutex;

use super::{Net, NET, WAIT_KEY};
use crate::process;
use crate::syscall::{EAGAIN, EINTR, EINVAL, EPIPE};

pub const AF_INET: u64 = 2;
pub const SOCK_STREAM: u64 = 1;
pub const SOCK_DGRAM: u64 = 2;
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
const CONNECT_TIMEOUT_TICKS: u64 = 20 * 18;

/// Which interface a smoltcp socket lives on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Which {
    Eth,
    Lo,
}

enum State {
    /// Created; maybe bound (address, port), not yet used.
    Fresh { bound: Option<(Ipv4Address, u16)> },
    /// A TCP connection (or connecting).
    Tcp { which: Which, handle: SocketHandle },
    /// A TCP server: smoltcp sockets in LISTEN on each interface.
    Listening { addr: Option<Ipv4Address>, port: u16, waiting: Vec<(Which, SocketHandle)> },
    /// UDP: one smoltcp socket per interface it's bound on.
    Udp { port: u16, handles: Vec<(Which, SocketHandle)>, peer: Option<IpEndpoint> },
    Closed,
}

pub struct Socket {
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
fn route(addr: Ipv4Address) -> Which {
    if addr.octets()[0] == 127 { Which::Lo } else { Which::Eth }
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
fn interfaces_for(net: &Net, addr: Option<Ipv4Address>) -> Vec<Which> {
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
    pub fn new(ty: u64) -> Result<Socket, i64> {
        let stream = match ty & 0xf {
            SOCK_STREAM => true,
            SOCK_DGRAM => false,
            _ => return Err(crate::syscall::EPROTONOSUPPORT),
        };
        Ok(Socket {
            stream,
            state: Mutex::new(State::Fresh { bound: None }),
            nonblocking: AtomicBool::new(ty & SOCK_NONBLOCK != 0),
            error: AtomicU64::new(0),
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        })
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
    pub fn bind(&self, addr: Ipv4Address, port: u16) -> Result<(), i64> {
        let mut state = self.state.lock();
        let State::Fresh { bound } = &mut *state else { return Err(EINVAL) };
        let port = if port == 0 { ephemeral_port() } else { port };
        let addr_opt = (addr != Ipv4Address::UNSPECIFIED).then_some(addr);
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
            s.bind(IpListenEndpoint { addr: addr_opt.map(IpAddress::Ipv4), port }).map_err(|_| EADDRINUSE)?;
            handles.push((which, with_set(net, which, |_, set| set.add(s))?));
        }
        *state = State::Udp { port, handles, peer: None };
        Ok(())
    }

    /// Make sure a UDP socket has smoltcp sockets (an implicit bind).
    fn ensure_udp(&self) -> Result<(), i64> {
        let fresh = matches!(&*self.state.lock(), State::Fresh { .. });
        if fresh {
            self.bind(Ipv4Address::UNSPECIFIED, 0)?;
        }
        Ok(())
    }

    pub fn listen(&self, backlog: usize) -> Result<(), i64> {
        if !self.stream {
            return Err(EOPNOTSUPP);
        }
        let mut state = self.state.lock();
        let (addr, port) = match &*state {
            State::Fresh { bound: Some((a, p)) } => ((*a != Ipv4Address::UNSPECIFIED).then_some(*a), *p),
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
                s.listen(IpListenEndpoint { addr: addr.map(IpAddress::Ipv4), port }).map_err(|_| EADDRINUSE)?;
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
            let _ = s.listen(IpListenEndpoint { addr: addr.map(IpAddress::Ipv4), port: *port });
            if let Ok(h) = with_set(net, which, |_, set| set.add(s)) {
                waiting.push((which, h));
            }
            let peer = with_set(net, which, |_, set| set.get::<tcp::Socket>(handle).remote_endpoint()).ok().flatten();
            Some(Ok((which, handle, peer)))
        })?;
        let (which, handle, peer) = found;
        let sock = Socket {
            stream: true,
            state: Mutex::new(State::Tcp { which, handle }),
            nonblocking: AtomicBool::new(nonblocking),
            error: AtomicU64::new(0),
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        };
        Ok((sock, peer))
    }

    pub fn connect(&self, addr: Ipv4Address, port: u16) -> Result<(), i64> {
        if !self.stream {
            // UDP: just the default destination.
            self.ensure_udp()?;
            if let State::Udp { peer, .. } = &mut *self.state.lock() {
                *peer = Some(IpEndpoint::new(IpAddress::Ipv4(addr), port));
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
                s.connect(iface.context(), IpEndpoint::new(IpAddress::Ipv4(addr), port), local_port)
                    .map(|_| set.add(s))
                    .map_err(|_| ENETUNREACH)
            })??;
            *state = State::Tcp { which, handle };
        }
        if self.is_nonblocking() {
            super::poll();
            return Err(EINPROGRESS);
        }
        let deadline = process::ticks() + CONNECT_TIMEOUT_TICKS;
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
        if !self.stream {
            self.ensure_udp()?;
        }
        wait(self, flags, None, |net| {
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
                    for &(which, h) in handles {
                        let got = with_set(net, which, |_, set| {
                            let s = set.get_mut::<udp::Socket>(h);
                            if s.can_recv() { s.recv_slice(buf).ok().map(|(n, meta)| (n, Some(meta.endpoint))) } else { None }
                        });
                        if let Ok(Some(r)) = got {
                            return Some(Ok(r));
                        }
                    }
                    None
                }
                _ => Some(Err(ENOTCONN)),
            }
        })
    }

    /// send / write (to `to`, for an unconnected UDP socket).
    pub fn send(&self, data: &[u8], flags: u64, to: Option<IpEndpoint>) -> Result<usize, i64> {
        if !self.stream {
            self.ensure_udp()?;
            let r = {
                let mut guard = NET.lock();
                let net = guard.as_mut().ok_or(ENETUNREACH)?;
                let state = self.state.lock();
                let State::Udp { handles, peer, .. } = &*state else { return Err(EINVAL) };
                let dest = to.or(*peer).ok_or(EDESTADDRREQ)?;
                let IpAddress::Ipv4(dest_v4) = dest.addr;
                let which = route(dest_v4);
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
        let unspec = IpEndpoint::new(IpAddress::Ipv4(Ipv4Address::UNSPECIFIED), 0);
        let mut guard = NET.lock();
        let Some(net) = guard.as_mut() else { return unspec };
        match &*self.state.lock() {
            State::Fresh { bound: Some((a, p)) } => IpEndpoint::new(IpAddress::Ipv4(*a), *p),
            State::Tcp { which, handle } => {
                with_set(net, *which, |_, set| set.get::<tcp::Socket>(*handle).local_endpoint()).ok().flatten().unwrap_or(unspec)
            }
            State::Listening { addr, port, .. } => {
                IpEndpoint::new(IpAddress::Ipv4(addr.unwrap_or(Ipv4Address::UNSPECIFIED)), *port)
            }
            State::Udp { port, .. } => IpEndpoint::new(IpAddress::Ipv4(Ipv4Address::UNSPECIFIED), *port),
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
                (s.can_recv() || (!s.may_recv() && !connecting), s.can_send() && !connecting, closed)
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
            State::Udp { handles, .. } => {
                for (which, h) in handles {
                    let _ = with_set(net, which, |_, set| set.remove(h));
                }
            }
            _ => {}
        }
    }
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
            if s.recv_queue() > 0 {
                s.abort();
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

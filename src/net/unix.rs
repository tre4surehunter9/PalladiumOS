// Copyright (c) 2026 tre4surehunter9
//! Unix-domain sockets (AF_UNIX): local sockets between processes.
//!
//! - Stream sockets are a pair of pipes, crossed: what one end writes the
//!   other reads (blocking, end-of-file, EPIPE/SIGPIPE come from pipes).
//! - Datagram sockets keep a queue of whole messages.
//! - Names: a path (`bind` creates a file there; `connect` needs write
//!   permission on it, and `unlink` removes it), or an abstract name
//!   (sun_path starting with a NUL byte: no file). A registry maps bound
//!   names to sockets.
//! - socketpair() makes a connected pair directly. SO_PEERCRED tells who's
//!   at the other end (pid, uid, gid).
//! File descriptors can be passed (SCM_RIGHTS, see vfs::sys_sendmsg): the
//! open files travel with a datagram, or on a stream in a queue beside the
//! bytes, and become new fds in the process that receives them.

use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use spin::Mutex;

use crate::pipe::{self, PipeReader, PipeWriter};
use crate::process;
use crate::syscall::{EAGAIN, EINTR, EINVAL, ENOENT, EPIPE};

pub const AF_UNIX: u64 = 1;
const MSG_DONTWAIT: u64 = 0x40;
const MSG_NOSIGNAL: u64 = 0x4000;
const EADDRINUSE: i64 = 98;
const EISCONN: i64 = 106;
const ENOTCONN: i64 = 107;
const ECONNREFUSED: i64 = 111;
const EDESTADDRREQ: i64 = 89;
const EOPNOTSUPP: i64 = 95;
/// Pending connections a listener holds, and datagrams a socket holds.
const QUEUE_MAX: usize = 64;

/// A socket's address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Name {
    /// A path, as given (and its absolute form, which finds the file).
    Path { given: String, abs: String },
    /// An abstract name (no file): the bytes after the leading NUL.
    Abstract(Vec<u8>),
}

/// What the registry is keyed by: a path's file, or an abstract name.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    File(usize, u64),
    Abstract(Vec<u8>),
}

static REGISTRY: Mutex<BTreeMap<Key, Weak<UnixSocket>>> = Mutex::new(BTreeMap::new());
static NEXT_ID: AtomicU64 = AtomicU64::new(1 << 32);

/// pid, uid, gid (struct ucred).
#[derive(Clone, Copy, Debug, Default)]
pub struct Creds {
    pub pid: u32,
    pub uid: u32,
    pub gid: u32,
}

fn my_creds() -> Creds {
    let c = process::current_cred();
    Creds { pid: process::current_tgid() as u32, uid: c.euid, gid: c.egid }
}

enum State {
    /// Created, maybe bound, not connected.
    Fresh { name: Option<Name>, key: Option<Key> },
    Listening { name: Name, key: Key, queue: VecDeque<Arc<UnixSocket>> },
    /// A connected stream: our read end and write end (None after shutdown),
    /// and the fds passed to us and by us (each batch given with the next
    /// read that returns data).
    Stream {
        rx: Option<PipeReader>,
        tx: Option<PipeWriter>,
        local: Option<Name>,
        peer: Option<Name>,
        peer_creds: Creds,
        fds_in: FdQueue,
        fds_out: FdQueue,
    },
    /// A datagram socket: received messages (who sent them, fds passed).
    Dgram {
        name: Option<Name>,
        key: Option<Key>,
        inbox: VecDeque<(Vec<u8>, Option<Name>, Vec<Handle>)>,
        peer: Option<Weak<UnixSocket>>,
        peer_creds: Creds,
    },
}

use crate::vfs::Handle;

/// One direction of a stream's passed fds: each batch at the position (in
/// bytes since the connection started) of the first byte sent with it, and
/// how many bytes have been written and read so far. A read never goes
/// past a batch's position, and the read starting at it gets the batch,
/// so fds arrive with the data they were sent with, as on Linux.
#[derive(Default)]
struct FdChan {
    batches: VecDeque<(u64, Vec<Handle>)>,
    written: u64,
    read: u64,
}

type FdQueue = Arc<Mutex<FdChan>>;

/// The two directions of a connected pair: (one side's in, the other side's in).
fn fd_queues() -> (FdQueue, FdQueue) {
    (Arc::new(Mutex::new(FdChan::default())), Arc::new(Mutex::new(FdChan::default())))
}

pub struct UnixSocket {
    stream: bool,
    state: Mutex<State>,
    nonblocking: AtomicBool,
    creds: Creds,
    pub id: u64,
}

fn key_for(name: &Name, must_exist: bool) -> Result<Key, i64> {
    match name {
        Name::Abstract(b) => Ok(Key::Abstract(b.clone())),
        Name::Path { abs, .. } => {
            if !must_exist {
                // bind: the file must not exist yet; make it.
                if crate::mounts::lstat(abs).is_ok() {
                    return Err(EADDRINUSE);
                }
                let umask = crate::process::with_current_files(|f| f.umask);
                let loc = crate::mounts::create(abs, crate::mounts::FileType::Socket, 0o777 & !umask)?;
                return Ok(Key::File(loc.mount, loc.ino));
            }
            let (loc, meta) = crate::mounts::stat(abs)?;
            // Connecting needs write permission on the socket file.
            crate::perm::check(&meta, &process::current_cred(), crate::perm::WRITE)?;
            Ok(Key::File(loc.mount, loc.ino))
        }
    }
}

fn lookup(name: &Name) -> Result<Arc<UnixSocket>, i64> {
    let key = key_for(name, true)?;
    REGISTRY.lock().get(&key).and_then(Weak::upgrade).ok_or(ECONNREFUSED)
}

impl UnixSocket {
    fn make(stream: bool, state: State, nonblocking: bool) -> UnixSocket {
        UnixSocket {
            stream,
            state: Mutex::new(state),
            nonblocking: AtomicBool::new(nonblocking),
            creds: my_creds(),
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        }
    }

    pub fn new(stream: bool, nonblocking: bool) -> UnixSocket {
        let state = if stream {
            State::Fresh { name: None, key: None }
        } else {
            State::Dgram { name: None, key: None, inbox: VecDeque::new(), peer: None, peer_creds: Creds::default() }
        };
        Self::make(stream, state, nonblocking)
    }

    /// socketpair(): two connected sockets.
    pub fn pair(stream: bool, nonblocking: bool) -> (Arc<UnixSocket>, Arc<UnixSocket>) {
        let me = my_creds();
        if stream {
            let ((r1, w1), (r2, w2)) = (pipe::new_pipe(), pipe::new_pipe());
            let (qa, qb) = fd_queues();
            let a = State::Stream { rx: Some(r1), tx: Some(w2), local: None, peer: None, peer_creds: me, fds_in: qa.clone(), fds_out: qb.clone() };
            let b = State::Stream { rx: Some(r2), tx: Some(w1), local: None, peer: None, peer_creds: me, fds_in: qb, fds_out: qa };
            (Arc::new(Self::make(true, a, nonblocking)), Arc::new(Self::make(true, b, nonblocking)))
        } else {
            let a = Arc::new(Self::new(false, nonblocking));
            let b = Arc::new(Self::new(false, nonblocking));
            for (x, y) in [(&a, &b), (&b, &a)] {
                if let State::Dgram { peer, peer_creds, .. } = &mut *x.state.lock() {
                    *peer = Some(Arc::downgrade(y));
                    *peer_creds = me;
                }
            }
            (a, b)
        }
    }

    fn wait_key(&self) -> u64 {
        self as *const UnixSocket as u64
    }

    fn nonblocking_for(&self, flags: u64) -> bool {
        self.nonblocking.load(Ordering::Relaxed) || flags & MSG_DONTWAIT != 0
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

    pub fn bind(self: &Arc<Self>, name: Name) -> Result<(), i64> {
        let mut state = self.state.lock();
        let (slot_name, slot_key) = match &mut *state {
            State::Fresh { name, key } | State::Dgram { name, key, .. } if name.is_none() => (name, key),
            _ => return Err(EINVAL),
        };
        if let Name::Abstract(_) = &name {
            if REGISTRY.lock().get(&key_for(&name, false)?).and_then(Weak::upgrade).is_some() {
                return Err(EADDRINUSE);
            }
        }
        let key = key_for(&name, false)?;
        REGISTRY.lock().insert(key.clone(), Arc::downgrade(self));
        *slot_name = Some(name);
        *slot_key = Some(key);
        Ok(())
    }

    pub fn listen(&self) -> Result<(), i64> {
        if !self.stream {
            return Err(EOPNOTSUPP);
        }
        let mut state = self.state.lock();
        match &mut *state {
            State::Listening { .. } => Ok(()),
            State::Fresh { name: Some(name), key: Some(key) } => {
                *state = State::Listening { name: name.clone(), key: key.clone(), queue: VecDeque::new() };
                Ok(())
            }
            _ => Err(EINVAL), // must be bound first
        }
    }

    pub fn connect(&self, name: Name) -> Result<(), i64> {
        let target = match lookup(&name) {
            Err(ENOENT) => return Err(ENOENT),
            r => r?,
        };
        if !self.stream {
            if target.stream {
                return Err(crate::syscall::EPROTOTYPE);
            }
            if let State::Dgram { peer, peer_creds, .. } = &mut *self.state.lock() {
                *peer = Some(Arc::downgrade(&target));
                *peer_creds = target.creds;
            }
            return Ok(());
        }
        let mut state = self.state.lock();
        let local = match &*state {
            State::Fresh { name, .. } => name.clone(),
            State::Stream { .. } => return Err(EISCONN),
            _ => return Err(EINVAL),
        };
        let mut tstate = target.state.lock();
        let State::Listening { queue, .. } = &mut *tstate else { return Err(ECONNREFUSED) };
        if queue.len() >= QUEUE_MAX {
            return Err(EAGAIN);
        }
        // A connected pair: our end now, the server's end for accept().
        let ((r1, w1), (r2, w2)) = (pipe::new_pipe(), pipe::new_pipe());
        let (q_client, q_server) = fd_queues();
        let server_end = State::Stream {
            rx: Some(r2),
            tx: Some(w1),
            local: Some(name.clone()),
            peer: local.clone(),
            peer_creds: self.creds,
            fds_in: q_server.clone(),
            fds_out: q_client.clone(),
        };
        let mut server = Self::make(true, server_end, false);
        server.creds = target.creds;
        queue.push_back(Arc::new(server));
        *state = State::Stream { rx: Some(r1), tx: Some(w2), local, peer: Some(name), peer_creds: target.creds, fds_in: q_client, fds_out: q_server };
        drop(tstate);
        process::wake(target.wait_key());
        Ok(())
    }

    pub fn accept(&self, nonblocking: bool) -> Result<(Arc<UnixSocket>, Option<Name>), i64> {
        loop {
            {
                let mut state = self.state.lock();
                let State::Listening { queue, .. } = &mut *state else { return Err(EINVAL) };
                if let Some(conn) = queue.pop_front() {
                    conn.set_nonblocking(nonblocking);
                    let peer = conn.peer_name();
                    return Ok((conn, peer));
                }
            }
            if self.is_nonblocking() {
                return Err(EAGAIN);
            }
            if process::signal_pending() {
                return Err(EINTR);
            }
            process::block_on(self.wait_key());
        }
    }

    fn peer_name(&self) -> Option<Name> {
        match &*self.state.lock() {
            State::Stream { peer, .. } => peer.clone(),
            _ => None,
        }
    }

    /// recv / read. For datagrams, also the sender.
    pub fn recv(&self, buf: &mut [u8], flags: u64) -> Result<(usize, Option<Name>), i64> {
        self.recv_msg(buf, flags).map(|(n, from, _)| (n, from))
    }

    /// recvmsg: also the fds passed with the data (SCM_RIGHTS).
    pub fn recv_msg(&self, buf: &mut [u8], flags: u64) -> Result<(usize, Option<Name>, Vec<Handle>), i64> {
        if self.stream {
            let (rx, fds_in) = match &*self.state.lock() {
                State::Stream { rx: Some(r), fds_in, .. } => (r.pipe(), fds_in.clone()),
                State::Stream { rx: None, .. } => return Ok((0, None, Vec::new())), // shut down for reading
                _ => return Err(ENOTCONN),
            };
            let (has_data, writers_gone) = pipe::read_status(&rx);
            if !has_data && !writers_gone && self.nonblocking_for(flags) {
                return Err(EAGAIN);
            }
            // Where the next passed fds are: read up to them, or (starting
            // right at them) the data sent with them, up to the batch after.
            let (limit, take) = {
                let mut ch = fds_in.lock();
                let read = ch.read;
                while ch.batches.front().is_some_and(|&(pos, _)| pos < read) {
                    ch.batches.pop_front(); // (stale: can't happen, but never stall on one)
                }
                match ch.batches.front() {
                    Some(&(pos, _)) if pos > read => ((pos - read) as usize, false),
                    Some(_) => (ch.batches.get(1).map_or(usize::MAX, |&(p, _)| (p - read) as usize), true),
                    None => (usize::MAX, false),
                }
            };
            let len = buf.len().min(limit);
            let n = pipe::read(&rx, &mut buf[..len]);
            if n < 0 {
                return Err(-n);
            }
            let mut ch = fds_in.lock();
            ch.read += n as u64;
            let fds = if take && n > 0 { ch.batches.pop_front().map(|(_, f)| f).unwrap_or_default() } else { Vec::new() };
            return Ok((n as usize, None, fds));
        }
        loop {
            {
                let mut state = self.state.lock();
                let State::Dgram { inbox, .. } = &mut *state else { return Err(EINVAL) };
                if let Some((msg, from, fds)) = inbox.pop_front() {
                    let n = msg.len().min(buf.len()); // the rest of a long message is lost
                    buf[..n].copy_from_slice(&msg[..n]);
                    return Ok((n, from, fds));
                }
            }
            if self.nonblocking_for(flags) {
                return Err(EAGAIN);
            }
            if process::signal_pending() {
                return Err(EINTR);
            }
            process::block_on(self.wait_key());
        }
    }

    /// send / write (to `to`, for an unconnected datagram socket).
    pub fn send(&self, data: &[u8], flags: u64, to: Option<Name>) -> Result<usize, i64> {
        self.send_msg(data, flags, to, Vec::new())
    }

    /// sendmsg: also passing fds (SCM_RIGHTS), given to the receiver with
    /// these bytes.
    pub fn send_msg(&self, data: &[u8], flags: u64, to: Option<Name>, fds: Vec<Handle>) -> Result<usize, i64> {
        if self.stream {
            let (tx, fds_out) = match &*self.state.lock() {
                State::Stream { tx: Some(w), fds_out, .. } => (w.pipe(), fds_out.clone()),
                State::Stream { tx: None, .. } => return Err(EPIPE),
                _ => return Err(ENOTCONN),
            };
            let (room, readers_gone) = pipe::write_status(&tx);
            if readers_gone {
                if flags & MSG_NOSIGNAL == 0 {
                    process::raise(process::SIGPIPE);
                }
                return Err(EPIPE);
            }
            if !room && self.nonblocking_for(flags) {
                return Err(EAGAIN);
            }
            // The fds first, at the position of the first byte sent with
            // them, so they're waiting when that byte is read.
            let passing = !fds.is_empty();
            if passing {
                let mut ch = fds_out.lock();
                let pos = ch.written;
                ch.batches.push_back((pos, fds));
            }
            let n = pipe::write(&tx, data);
            let mut ch = fds_out.lock();
            if n > 0 {
                ch.written += n as u64;
            } else if passing {
                ch.batches.pop_back(); // nothing sent: the fds weren't either
            }
            return if n < 0 { Err(-n) } else { Ok(n as usize) };
        }
        let (dest, from) = {
            let state = self.state.lock();
            let State::Dgram { name, peer, .. } = &*state else { return Err(EINVAL) };
            let dest = match &to {
                Some(n) => lookup(n)?,
                None => peer.as_ref().and_then(Weak::upgrade).ok_or(if peer.is_some() { ECONNREFUSED } else { EDESTADDRREQ })?,
            };
            (dest, name.clone())
        };
        if dest.stream {
            return Err(crate::syscall::EPROTOTYPE);
        }
        {
            let mut dstate = dest.state.lock();
            let State::Dgram { inbox, .. } = &mut *dstate else { return Err(ECONNREFUSED) };
            if inbox.len() >= QUEUE_MAX {
                return Err(EAGAIN);
            }
            inbox.push_back((data.to_vec(), from, fds));
        }
        process::wake(dest.wait_key());
        Ok(data.len())
    }

    /// shutdown(how): 0 stop reading, 1 stop writing (the other end sees
    /// end-of-file), 2 both.
    pub fn shutdown(&self, how: u64) -> Result<(), i64> {
        let mut state = self.state.lock();
        let State::Stream { rx, tx, .. } = &mut *state else { return Err(ENOTCONN) };
        if how == 0 || how == 2 {
            *rx = None;
        }
        if how == 1 || how == 2 {
            *tx = None;
        }
        Ok(())
    }

    pub fn local(&self) -> Option<Name> {
        match &*self.state.lock() {
            State::Fresh { name, .. } | State::Dgram { name, .. } => name.clone(),
            State::Listening { name, .. } => Some(name.clone()),
            State::Stream { local, .. } => local.clone(),
        }
    }

    pub fn peer(&self) -> Result<Option<Name>, i64> {
        match &*self.state.lock() {
            State::Stream { peer, .. } => Ok(peer.clone()),
            State::Dgram { peer: Some(p), .. } => Ok(p.upgrade().and_then(|p| p.local())),
            _ => Err(ENOTCONN),
        }
    }

    /// SO_PEERCRED: who's at the other end.
    pub fn peer_creds(&self) -> Creds {
        match &*self.state.lock() {
            State::Stream { peer_creds, .. } | State::Dgram { peer_creds, .. } => *peer_creds,
            _ => Creds::default(),
        }
    }

    /// For poll(): (readable, writable, hung up).
    pub fn readiness(&self) -> (bool, bool, bool) {
        match &*self.state.lock() {
            State::Stream { rx, tx, .. } => {
                let (data, eof) = rx.as_ref().map_or((false, true), |r| pipe::read_status(&r.pipe()));
                let (room, gone) = tx.as_ref().map_or((false, true), |w| pipe::write_status(&w.pipe()));
                (data || eof, room && !gone, eof && gone)
            }
            State::Listening { queue, .. } => (!queue.is_empty(), false, false),
            State::Dgram { inbox, .. } => (!inbox.is_empty(), true, false),
            State::Fresh { .. } => (false, false, false),
        }
    }

    /// Bytes waiting (FIONREAD): the next datagram, or what's in the pipe.
    pub fn pending_bytes(&self) -> usize {
        match &*self.state.lock() {
            State::Dgram { inbox, .. } => inbox.front().map_or(0, |(m, _, _)| m.len()),
            State::Stream { rx: Some(r), .. } => pipe::buffered(&r.pipe()),
            _ => 0,
        }
    }
}

impl Drop for UnixSocket {
    /// A bound socket's name is free again (a path's file stays, as on Linux).
    fn drop(&mut self) {
        let key = match &*self.state.lock() {
            State::Listening { key, .. } => Some(key.clone()),
            State::Fresh { key, .. } | State::Dgram { key, .. } => key.clone(),
            _ => None,
        };
        if let Some(key) = key {
            let mut reg = REGISTRY.lock();
            if reg.get(&key).is_some_and(|w| w.as_ptr() == self as *const UnixSocket) {
                reg.remove(&key);
            }
        }
    }
}

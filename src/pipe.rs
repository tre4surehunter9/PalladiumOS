// Copyright (c) 2026 tre4surehunter9
//! Pipes: a byte buffer with a read end and a write end.
//!
//! - read() blocks while the pipe is empty, and returns 0 (end-of-file)
//!   once every write end has been closed.
//! - write() blocks while the pipe is full. Writes of up to PIPE_BUF bytes
//!   are atomic (never interleaved with other writers). Writing when every
//!   read end is closed raises SIGPIPE and fails with EPIPE.
//!
//! The ends are counted: each `PipeReader`/`PipeWriter` is one open end, and
//! dropping it (closing the last fd that refers to it) updates the count and
//! wakes anyone blocked on the pipe.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;

use crate::process;
use crate::syscall::{EINTR, EPIPE};

/// How many bytes a pipe holds.
pub const PIPE_CAPACITY: usize = 16 * 1024;
/// Writes up to this size are atomic (POSIX PIPE_BUF).
pub const PIPE_BUF: usize = 4096;

pub struct Pipe {
    buf: Mutex<VecDeque<u8>>,
    readers: AtomicUsize,
    writers: AtomicUsize,
}

impl Pipe {
    /// Key processes block on while waiting for this pipe.
    fn key(self: &Arc<Self>) -> u64 {
        Arc::as_ptr(self) as u64
    }
}

/// A number identifying a pipe (shown as pipe:[N] in /proc/PID/fd).
pub fn id(pipe: &Arc<Pipe>) -> u64 {
    // Heap addresses are unique while the pipe exists; make them short.
    (Arc::as_ptr(pipe) as u64 >> 4) & 0xFFFFF
}

/// Create a pipe, returning its read and write ends.
pub fn new_pipe() -> (PipeReader, PipeWriter) {
    let pipe = Arc::new(Pipe {
        buf: Mutex::new(VecDeque::new()),
        readers: AtomicUsize::new(1),
        writers: AtomicUsize::new(1),
    });
    (PipeReader(pipe.clone()), PipeWriter(pipe))
}

pub struct PipeReader(Arc<Pipe>);
pub struct PipeWriter(Arc<Pipe>);

impl Drop for PipeReader {
    fn drop(&mut self) {
        self.0.readers.fetch_sub(1, Ordering::SeqCst);
        process::wake(self.0.key()); // blocked writers get EPIPE now
    }
}

impl Drop for PipeWriter {
    fn drop(&mut self) {
        self.0.writers.fetch_sub(1, Ordering::SeqCst);
        process::wake(self.0.key()); // blocked readers see end-of-file now
    }
}

impl PipeReader {
    pub fn pipe(&self) -> Arc<Pipe> {
        self.0.clone()
    }
}

impl PipeWriter {
    pub fn pipe(&self) -> Arc<Pipe> {
        self.0.clone()
    }
}

/// Read from a pipe: blocks until there's data or no writers are left.
pub fn read(pipe: &Arc<Pipe>, out: &mut [u8]) -> i64 {
    if out.is_empty() {
        return 0;
    }
    loop {
        {
            let mut buf = pipe.buf.lock();
            if !buf.is_empty() {
                let n = out.len().min(buf.len());
                for (dst, src) in out.iter_mut().zip(buf.drain(..n)) {
                    *dst = src;
                }
                drop(buf);
                process::wake(pipe.key()); // room for blocked writers
                return n as i64;
            }
        }
        if pipe.writers.load(Ordering::SeqCst) == 0 {
            return 0; // end-of-file
        }
        if process::signal_pending() {
            return -EINTR;
        }
        process::block_on(pipe.key());
    }
}

/// Write to a pipe: blocks while it's full.
pub fn write(pipe: &Arc<Pipe>, data: &[u8]) -> i64 {
    let atomic = data.len() <= PIPE_BUF;
    let mut written = 0usize;
    loop {
        if pipe.readers.load(Ordering::SeqCst) == 0 {
            if written > 0 {
                return written as i64;
            }
            process::raise(process::SIGPIPE);
            return -EPIPE;
        }
        {
            let mut buf = pipe.buf.lock();
            let space = PIPE_CAPACITY - buf.len();
            let remaining = data.len() - written;
            // A small write goes in all at once or waits for room.
            if space > 0 && (!atomic || space >= remaining) {
                let n = space.min(remaining);
                buf.extend(&data[written..written + n]);
                written += n;
            }
        }
        if written > 0 {
            process::wake(pipe.key()); // data for blocked readers
        }
        if written == data.len() {
            return written as i64;
        }
        if process::signal_pending() {
            return if written > 0 { written as i64 } else { -EINTR };
        }
        process::block_on(pipe.key());
    }
}

/// A non-blocking write: what fits now, or -EAGAIN (a write of up to
/// PIPE_BUF bytes goes in whole or not at all). No readers: SIGPIPE/EPIPE.
pub fn try_write(pipe: &Arc<Pipe>, data: &[u8]) -> i64 {
    if pipe.readers.load(Ordering::SeqCst) == 0 {
        process::raise(process::SIGPIPE);
        return -EPIPE;
    }
    let n = {
        let mut buf = pipe.buf.lock();
        let space = PIPE_CAPACITY - buf.len();
        let n = if data.len() <= PIPE_BUF && space < data.len() { 0 } else { space.min(data.len()) };
        buf.extend(&data[..n]);
        n
    };
    if n == 0 && !data.is_empty() {
        return -crate::syscall::EAGAIN;
    }
    process::wake(pipe.key());
    n as i64
}

/// A non-blocking write for a stream socket: whatever fits (no PIPE_BUF
/// all-or-nothing rule: poll's "writable" means some room, and any room
/// takes some bytes), -EAGAIN when full, -EPIPE without readers (the
/// socket decides about SIGPIPE).
pub fn try_write_stream(pipe: &Arc<Pipe>, data: &[u8]) -> i64 {
    if pipe.readers.load(Ordering::SeqCst) == 0 {
        return -EPIPE;
    }
    let n = {
        let mut buf = pipe.buf.lock();
        let n = (PIPE_CAPACITY - buf.len()).min(data.len());
        buf.extend(&data[..n]);
        n
    };
    if n == 0 && !data.is_empty() {
        return -crate::syscall::EAGAIN;
    }
    process::wake(pipe.key());
    n as i64
}

/// A non-blocking read: what's there, 0 at end-of-file, else -EAGAIN.
pub fn try_read(pipe: &Arc<Pipe>, out: &mut [u8]) -> i64 {
    if out.is_empty() {
        return 0;
    }
    let n = {
        let mut buf = pipe.buf.lock();
        let n = out.len().min(buf.len());
        for (dst, src) in out.iter_mut().zip(buf.drain(..n)) {
            *dst = src;
        }
        n
    };
    if n > 0 {
        process::wake(pipe.key()); // room for blocked writers
        return n as i64;
    }
    if pipe.writers.load(Ordering::SeqCst) == 0 { 0 } else { -crate::syscall::EAGAIN }
}

/// For poll: (has data, every write end closed).
pub fn read_status(pipe: &Arc<Pipe>) -> (bool, bool) {
    (!pipe.buf.lock().is_empty(), pipe.writers.load(Ordering::SeqCst) == 0)
}

/// For poll: (has room, every read end closed).
pub fn write_status(pipe: &Arc<Pipe>) -> (bool, bool) {
    (pipe.buf.lock().len() < PIPE_CAPACITY, pipe.readers.load(Ordering::SeqCst) == 0)
}

/// For FIONREAD: bytes waiting in the pipe.
pub fn buffered(pipe: &Arc<Pipe>) -> usize {
    pipe.buf.lock().len()
}

/// A read without the kernel lock (the lock-free system calls): what's
/// there now. None: nothing to read yet (the usual way blocks), or the
/// pipe was busy.
pub fn fast_read(pipe: &Arc<Pipe>, out: &mut [u8]) -> Option<i64> {
    let n = {
        let mut buf = crate::smp::try_lock_spinning(&pipe.buf)?;
        let n = out.len().min(buf.len());
        for (dst, src) in out.iter_mut().zip(buf.drain(..n)) {
            *dst = src;
        }
        n
    };
    if n == 0 {
        return if pipe.writers.load(Ordering::SeqCst) == 0 { Some(0) } else { None };
    }
    process::wake(pipe.key()); // room for blocked writers (queued: see process::wake)
    Some(n as i64)
}

/// A write without the kernel lock: all of `data` if it fits now. None:
/// it doesn't (the usual way waits or writes part), no readers (SIGPIPE:
/// the usual way), or the pipe was busy.
pub fn fast_write(pipe: &Arc<Pipe>, data: &[u8]) -> Option<i64> {
    if pipe.readers.load(Ordering::SeqCst) == 0 {
        return None;
    }
    {
        let mut buf = crate::smp::try_lock_spinning(&pipe.buf)?;
        if PIPE_CAPACITY - buf.len() < data.len() {
            return None;
        }
        buf.extend(data);
    }
    process::wake(pipe.key()); // data for blocked readers
    Some(data.len() as i64)
}

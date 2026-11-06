// Copyright (c) 2026 tre4surehunter9
//! Helpers for text-processing programs: reading lines, buffered output,
//! and the usual "files or standard input" arguments.

use alloc::vec::Vec;
use crate::{close, open, read, strerror, write_all, O_RDONLY, STDIN, STDOUT};

/// Reads a file descriptor one line at a time (lines keep their '\n').
pub struct Lines {
    fd: i32,
    buf: Vec<u8>,
    start: usize,
    eof: bool,
}

impl Lines {
    pub fn new(fd: i32) -> Self {
        Lines { fd, buf: Vec::new(), start: 0, eof: false }
    }

    /// The next line, None at end of input, or Err(-errno).
    pub fn next_line(&mut self) -> Option<Result<Vec<u8>, isize>> {
        loop {
            if let Some(i) = self.buf[self.start..].iter().position(|&b| b == b'\n') {
                let line = self.buf[self.start..self.start + i + 1].to_vec();
                self.start += i + 1;
                return Some(Ok(line));
            }
            if self.eof {
                if self.start < self.buf.len() {
                    let line = self.buf[self.start..].to_vec();
                    self.start = self.buf.len();
                    return Some(Ok(line)); // last line without '\n'
                }
                return None;
            }
            // Drop consumed bytes, then read more.
            self.buf.drain(..self.start);
            self.start = 0;
            let mut chunk = [0u8; 1024];
            let n = read(self.fd, &mut chunk);
            if n < 0 {
                return Some(Err(n));
            }
            if n == 0 {
                self.eof = true;
            } else {
                self.buf.extend_from_slice(&chunk[..n as usize]);
            }
        }
    }
}

/// Read everything from `fd`.
pub fn read_to_end(fd: i32) -> Result<Vec<u8>, isize> {
    let mut data = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let n = read(fd, &mut chunk);
        if n < 0 {
            return Err(n);
        }
        if n == 0 {
            return Ok(data);
        }
        data.extend_from_slice(&chunk[..n as usize]);
    }
}

/// Buffered standard output (one write per 4 KiB instead of per line).
/// Flushes when dropped. If the reader went away (EPIPE), further output
/// is discarded.
pub struct Out {
    buf: Vec<u8>,
    pub failed: bool,
}

impl Out {
    pub fn new() -> Self {
        Out { buf: Vec::with_capacity(4096), failed: false }
    }

    pub fn write(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
        if self.buf.len() >= 4096 {
            self.flush();
        }
    }

    pub fn flush(&mut self) {
        if !self.buf.is_empty() && !self.failed && write_all(STDOUT, &self.buf).is_err() {
            self.failed = true;
        }
        self.buf.clear();
    }
}

impl Default for Out {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Out {
    fn drop(&mut self) {
        self.flush();
    }
}

/// Call `f(fd, name)` for each input: the named files, or standard input
/// if there are none ("-" also means standard input). Prints an error for
/// files that can't be opened. Returns 1 if any failed, else 0.
pub fn for_each_input(prog: &str, files: &[&str], mut f: impl FnMut(i32, &str)) -> i32 {
    if files.is_empty() {
        f(STDIN, "-");
        return 0;
    }
    let mut status = 0;
    for &name in files {
        if name == "-" {
            f(STDIN, "-");
            continue;
        }
        let fd = open(name, O_RDONLY);
        if fd < 0 {
            crate::eprintln!("{}: {}: {}", prog, name, strerror(-fd));
            status = 1;
            continue;
        }
        f(fd as i32, name);
        close(fd as i32);
    }
    status
}

/// Parse a "-n N" / "-nN" / "-N" style count option. Returns the count and
/// the remaining (non-option) arguments.
pub fn count_option<'a>(args: &[&'a str], default: usize) -> Result<(usize, Vec<&'a str>), &'a str> {
    let mut count = default;
    let mut rest = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i];
        if a == "-n" {
            i += 1;
            count = args.get(i).and_then(|n| n.parse().ok()).ok_or(a)?;
        } else if let Some(n) = a.strip_prefix("-n") {
            count = n.parse().map_err(|_| a)?;
        } else if a.len() > 1 && a.starts_with('-') && a[1..].bytes().all(|b| b.is_ascii_digit()) {
            count = a[1..].parse().map_err(|_| a)?;
        } else {
            rest.push(a);
        }
        i += 1;
    }
    Ok((count, rest))
}

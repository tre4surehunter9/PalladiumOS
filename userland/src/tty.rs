// Copyright (c) 2026 tre4surehunter9
//! Terminal control: termios (raw/cooked mode), isatty, window size.

use crate::sys::{self, syscall3};

pub const ISIG: u32 = 0o1;
pub const ICANON: u32 = 0o2;
pub const ECHO: u32 = 0o10;
pub const ICRNL: u32 = 0o400;

const TCGETS: u64 = 0x5401;
const TCSETS: u64 = 0x5402;
const TIOCGWINSZ: u64 = 0x5413;
const TIOCGPGRP: u64 = 0x540F;
const TIOCSPGRP: u64 = 0x5410;

/// tcsetpgrp(3): give the terminal (Ctrl-C) to process group `pgid`.
pub fn tcsetpgrp(fd: i32, pgid: i32) -> isize {
    unsafe { syscall3(sys::IOCTL, fd as u64, TIOCSPGRP, &pgid as *const i32 as u64) as isize }
}

/// tcgetpgrp(3)
pub fn tcgetpgrp(fd: i32) -> isize {
    let mut pgid = 0i32;
    let r = unsafe { syscall3(sys::IOCTL, fd as u64, TIOCGPGRP, &mut pgid as *mut i32 as u64) } as isize;
    if r < 0 { r } else { pgid as isize }
}

/// struct termios (Linux kernel layout, 36 bytes).
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Termios {
    pub c_iflag: u32,
    pub c_oflag: u32,
    pub c_cflag: u32,
    pub c_lflag: u32,
    pub c_line: u8,
    pub c_cc: [u8; 19],
}

pub fn tcgetattr(fd: i32) -> Result<Termios, isize> {
    let mut t = Termios::default();
    let r = unsafe { syscall3(sys::IOCTL, fd as u64, TCGETS, &mut t as *mut Termios as u64) } as isize;
    if r < 0 { Err(r) } else { Ok(t) }
}

pub fn tcsetattr(fd: i32, t: &Termios) -> isize {
    unsafe { syscall3(sys::IOCTL, fd as u64, TCSETS, t as *const Termios as u64) as isize }
}

/// Is `fd` a terminal (the console)?
pub fn isatty(fd: i32) -> bool {
    tcgetattr(fd).is_ok()
}

/// (rows, columns) of the terminal.
pub fn window_size(fd: i32) -> (u16, u16) {
    let mut ws = [0u16; 4];
    let r = unsafe { syscall3(sys::IOCTL, fd as u64, TIOCGWINSZ, ws.as_mut_ptr() as u64) };
    if r < 0 { (25, 80) } else { (ws[0], ws[1]) }
}

/// Switch `fd` to raw mode (no line buffering, no echo, no signal keys).
/// Returns the previous settings, to restore with tcsetattr.
pub fn enable_raw_mode(fd: i32) -> Result<Termios, isize> {
    let saved = tcgetattr(fd)?;
    let mut raw = saved;
    raw.c_lflag &= !(ICANON | ECHO | ISIG);
    raw.c_iflag &= !ICRNL;
    let r = tcsetattr(fd, &raw);
    if r < 0 { Err(r) } else { Ok(saved) }
}

/// A key from a raw-mode terminal, with escape sequences decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    /// Ctrl+letter, as the lowercase letter ('a' for Ctrl+A).
    Ctrl(char),
    Enter,
    Backspace,
    Delete,
    Tab,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
}

/// Reads keys from a raw-mode fd, decoding VT100 escape sequences.
pub struct KeyReader {
    fd: i32,
    buf: [u8; 64],
    len: usize,
    pos: usize,
}

impl KeyReader {
    pub fn new(fd: i32) -> Self {
        KeyReader { fd, buf: [0; 64], len: 0, pos: 0 }
    }

    fn byte(&mut self) -> Option<u8> {
        if self.pos == self.len {
            let n = crate::read(self.fd, &mut self.buf);
            if n <= 0 {
                return None;
            }
            self.len = n as usize;
            self.pos = 0;
        }
        let b = self.buf[self.pos];
        self.pos += 1;
        Some(b)
    }

    /// Next byte only if it's already buffered (escape sequences arrive whole).
    fn buffered_byte(&mut self) -> Option<u8> {
        if self.pos < self.len {
            self.pos += 1;
            Some(self.buf[self.pos - 1])
        } else {
            None
        }
    }

    /// The next key, or None at end-of-file.
    pub fn next_key(&mut self) -> Option<Key> {
        let b = self.byte()?;
        Some(match b {
            b'\r' | b'\n' => Key::Enter,
            0x7f | 0x08 => Key::Backspace,
            b'\t' => Key::Tab,
            0x1b => {
                if self.buffered_byte() != Some(b'[') {
                    return Some(Key::Escape);
                }
                match self.buffered_byte() {
                    Some(b'A') => Key::Up,
                    Some(b'B') => Key::Down,
                    Some(b'C') => Key::Right,
                    Some(b'D') => Key::Left,
                    Some(b'H') => Key::Home,
                    Some(b'F') => Key::End,
                    Some(d @ b'0'..=b'9') => {
                        let _ = self.buffered_byte(); // the '~'
                        match d {
                            b'1' | b'7' => Key::Home,
                            b'4' | b'8' => Key::End,
                            b'3' => Key::Delete,
                            b'5' => Key::PageUp,
                            b'6' => Key::PageDown,
                            _ => Key::Escape,
                        }
                    }
                    _ => Key::Escape,
                }
            }
            1..=26 => Key::Ctrl((b'a' + b - 1) as char),
            b if b.is_ascii() => Key::Char(b as char),
            _ => Key::Char('?'),
        })
    }
}

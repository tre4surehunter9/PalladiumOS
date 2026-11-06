// Copyright (c) 2026 tre4surehunter9
//! The console TTY: turns keyboard scancodes into bytes for read(0, ...).
//!
//! Like a POSIX terminal, it has two modes, set with termios (ioctl TCSETS):
//!
//! - Canonical ("cooked", the default): typed characters are echoed and
//!   collected into a line; backspace and Ctrl-U edit it, and read() only
//!   returns once Enter is pressed. Ctrl-D on an empty line is end-of-file
//!   (read returns 0); on a non-empty line it hands over the line without '\n'.
//! - Raw (ICANON off): every key is available to read() immediately, as the
//!   bytes a VT100-style terminal sends (arrows are ESC [ A..D, Enter is '\r'
//!   unless ICRNL, Backspace is 0x7f, Ctrl+letter is 1..26). Editors and the
//!   shell's line editor use this.

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use pc_keyboard::{layouts, DecodedKey, HandleControl, Keyboard, ScancodeSet1};
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use spin::Mutex;

// termios flag bits (Linux values)
pub const ISIG: u32 = 0o1;
pub const ICANON: u32 = 0o2;
pub const ECHO: u32 = 0o10;
pub const ICRNL: u32 = 0o400;

/// The termios settings the kernel keeps (c_iflag and c_lflag; the rest of
/// struct termios is accepted but has no effect).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mode {
    pub iflag: u32,
    pub lflag: u32,
    /// Raw mode: a read returns once VMIN bytes are there; with VMIN 0 it
    /// waits at most VTIME tenths of a second (0: doesn't wait at all).
    pub vmin: u8,
    pub vtime: u8,
}

pub const DEFAULT_MODE: Mode = Mode { iflag: ICRNL, lflag: ISIG | ICANON | ECHO, vmin: 1, vtime: 0 };

// Atomics, not a lock: the keyboard interrupt reads them (intercept_scancode).
static IFLAG: AtomicU32 = AtomicU32::new(DEFAULT_MODE.iflag);
static LFLAG: AtomicU32 = AtomicU32::new(DEFAULT_MODE.lflag);
static VMIN_VTIME: AtomicU32 = AtomicU32::new(1);
static CTRL_HELD: AtomicBool = AtomicBool::new(false);

pub fn mode() -> Mode {
    let mv = VMIN_VTIME.load(Ordering::SeqCst);
    Mode {
        iflag: IFLAG.load(Ordering::SeqCst),
        lflag: LFLAG.load(Ordering::SeqCst),
        vmin: mv as u8,
        vtime: (mv >> 8) as u8,
    }
}

pub fn set_mode(mode: Mode) {
    IFLAG.store(mode.iflag, Ordering::SeqCst);
    LFLAG.store(mode.lflag, Ordering::SeqCst);
    VMIN_VTIME.store(mode.vmin as u32 | (mode.vtime as u32) << 8, Ordering::SeqCst);
}

/// Called by the keyboard interrupt for every scancode. Returns true if it
/// was Ctrl-C turned into SIGINT for the foreground program (then the key
/// is dropped). Only in cooked mode with ISIG, and only when a user program
/// has the keyboard; otherwise Ctrl-C is an ordinary key (byte 3).
pub fn intercept_scancode(scancode: u8) -> bool {
    const CTRL: u8 = 0x1d;
    const KEY_C: u8 = 0x2e;
    match scancode {
        CTRL => CTRL_HELD.store(true, Ordering::SeqCst),
        s if s == CTRL | 0x80 => CTRL_HELD.store(false, Ordering::SeqCst),
        KEY_C if CTRL_HELD.load(Ordering::SeqCst)
            && LFLAG.load(Ordering::SeqCst) & ISIG != 0
            && crate::process::foreground() != 0 =>
        {
            crate::process::ctrl_c_pressed();
            return true;
        }
        _ => {}
    }
    false
}

/// Back to cooked mode with echo (e.g. after a program left the console raw).
pub fn reset_mode() {
    set_mode(DEFAULT_MODE);
    if let Some(tty) = TTY.lock().as_mut() {
        tty.line.clear();
    }
}

struct Tty {
    keyboard: Keyboard<layouts::Us104Key, ScancodeSet1>,
    /// The line being edited.
    line: Vec<u8>,
    /// Finished input waiting to be read.
    ready: VecDeque<u8>,
    /// Ctrl-D was pressed on an empty line.
    eof: bool,
}

static TTY: Mutex<Option<Tty>> = Mutex::new(None);

const MAX_LINE: usize = 1024;

/// Block until input is available, then copy up to `buf.len()` bytes into it.
/// Returns the number of bytes read (0 means end-of-file), or -EINTR if a
/// signal arrived while waiting.
///
/// Called from the read syscall, so interrupts are disabled on entry. While
/// there's nothing to read, other ready processes run; if there are none,
/// the CPU halts until the next interrupt (e.g. a key press).
pub fn read(buf: &mut [u8]) -> i64 {
    use crate::vga_buffer::{draw_cursor, erase_cursor};

    if buf.is_empty() {
        return 0;
    }
    crate::task::keyboard::init_queue();
    // Raw mode with VMIN 0: a timeout of VTIME tenths of a second.
    let m = mode();
    let timeout = (m.lflag & ICANON == 0 && m.vmin == 0).then(|| {
        let ticks = (m.vtime as u64 * 100_000_000).div_ceil(crate::process::TICK_NS);
        crate::process::ticks() + ticks
    });

    loop {
        {
            let mut guard = TTY.lock();
            let tty = guard.get_or_insert_with(|| Tty {
                keyboard: Keyboard::new(ScancodeSet1::new(), layouts::Us104Key, HandleControl::MapLettersToUnicode),
                line: Vec::new(),
                ready: VecDeque::new(),
                eof: false,
            });
            if !tty.ready.is_empty() {
                erase_cursor();
                let n = buf.len().min(tty.ready.len());
                for (dst, src) in buf.iter_mut().zip(tty.ready.drain(..n)) {
                    *dst = src;
                }
                return n as i64;
            }
            if tty.eof {
                erase_cursor();
                tty.eof = false;
                return 0;
            }
            if let Some(scancode) = crate::task::keyboard::pop_scancode() {
                erase_cursor();
                tty.handle_scancode(scancode);
                continue;
            }
        } // TTY lock released before waiting

        if let Some(deadline) = timeout {
            if crate::process::ticks() >= deadline {
                return 0; // nothing typed in time
            }
        }

        if crate::process::signal_pending() {
            return -crate::syscall::EINTR;
        }

        if crate::process::others_ready() {
            // Let background processes run while we wait for a key.
            erase_cursor();
            crate::process::yield_now();
        } else {
            draw_cursor();
            // sti; hlt is atomic with respect to interrupts, so a key pressed
            // between the check above and the hlt still wakes us up.
            crate::process::idle_halt();
        }
    }
}

impl Tty {
    fn handle_scancode(&mut self, scancode: u8) {
        use pc_keyboard::KeyCode;

        let Ok(Some(event)) = self.keyboard.add_byte(scancode) else { return };
        let event = crate::task::keyboard::fix_key_event(event);
        // The layout decodes Delete as 0x7f, the same as what Backspace sends
        // in raw mode, so tell them apart by key code.
        let is_delete = event.code == KeyCode::Delete;
        let Some(key) = self.keyboard.process_keyevent(event) else { return };
        let mode = mode();

        if mode.lflag & ICANON == 0 {
            // Raw mode: hand over the bytes a VT100 terminal would send.
            let bytes: &[u8] = match key {
                _ if is_delete => b"\x1b[3~",
                DecodedKey::Unicode('\n') => if mode.iflag & ICRNL != 0 { b"\n" } else { b"\r" },
                DecodedKey::Unicode('\x08') => b"\x7f",
                DecodedKey::Unicode(c) if c.is_ascii() => {
                    let byte = c as u8;
                    self.ready.push_back(byte);
                    if mode.lflag & ECHO != 0 && !c.is_ascii_control() {
                        crate::print!("{}", c);
                    }
                    return;
                }
                DecodedKey::RawKey(KeyCode::ArrowUp) => b"\x1b[A",
                DecodedKey::RawKey(KeyCode::ArrowDown) => b"\x1b[B",
                DecodedKey::RawKey(KeyCode::ArrowRight) => b"\x1b[C",
                DecodedKey::RawKey(KeyCode::ArrowLeft) => b"\x1b[D",
                DecodedKey::RawKey(KeyCode::Home) => b"\x1b[H",
                DecodedKey::RawKey(KeyCode::End) => b"\x1b[F",
                DecodedKey::RawKey(KeyCode::Delete) => b"\x1b[3~",
                DecodedKey::RawKey(KeyCode::PageUp) => b"\x1b[5~",
                DecodedKey::RawKey(KeyCode::PageDown) => b"\x1b[6~",
                _ => return,
            };
            self.ready.extend(bytes);
            return;
        }

        let DecodedKey::Unicode(c) = key else { return };
        if is_delete {
            return; // no meaning in a cooked line
        }
        let echo = mode.lflag & ECHO != 0;
        match c {
            '\n' => {
                if echo {
                    crate::println!();
                }
                self.line.push(b'\n');
                self.ready.extend(self.line.drain(..));
            }
            '\x08' | '\x7f' => {
                if self.line.pop().is_some() && echo {
                    crate::print!("\x08");
                }
            }
            '\x15' => {
                // Ctrl-U: erase the whole line
                while self.line.pop().is_some() {
                    if echo {
                        crate::print!("\x08");
                    }
                }
            }
            '\x04' => {
                // Ctrl-D
                if self.line.is_empty() {
                    self.eof = true;
                } else {
                    self.ready.extend(self.line.drain(..));
                }
            }
            c if c.is_ascii() && !c.is_ascii_control() && self.line.len() < MAX_LINE => {
                self.line.push(c as u8);
                if echo {
                    crate::print!("{}", c);
                }
            }
            _ => {}
        }
    }
}

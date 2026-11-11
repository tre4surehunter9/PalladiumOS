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
/// Keyboard modes (KDSKBMODE): K_XLATE is the normal, decoded one;
/// K_RAW gives the scancodes, K_MEDIUMRAW Linux keycodes (an X server).
pub const K_RAW: u8 = 0;
pub const K_XLATE: u8 = 1;
pub const K_MEDIUMRAW: u8 = 2;
pub const K_UNICODE: u8 = 3;
static KB_MODE: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(K_XLATE);
/// An 0xE0 prefix was the last scancode (medium raw mode).
static E0_SEEN: AtomicBool = AtomicBool::new(false);

pub fn keyboard_mode() -> u8 {
    KB_MODE.load(Ordering::SeqCst)
}

pub fn set_keyboard_mode(mode: u8) {
    KB_MODE.store(mode, Ordering::SeqCst);
    E0_SEEN.store(false, Ordering::SeqCst);
    *TTY.lock() = None; // decoded state starts over
}

fn raw_mode() -> bool {
    matches!(keyboard_mode(), K_RAW | K_MEDIUMRAW)
}

/// A scancode as the keyboard mode wants it: as is (K_RAW), or as a Linux
/// keycode (K_MEDIUMRAW: one byte, bit 7 for a release; None for the 0xE0
/// prefix, which only changes the next code).
fn raw_byte(scancode: u8) -> Option<u8> {
    if keyboard_mode() == K_RAW {
        return Some(scancode);
    }
    if scancode == 0xE0 {
        E0_SEEN.store(true, Ordering::SeqCst);
        return None;
    }
    let release = scancode & 0x80;
    let code = scancode & 0x7f;
    let keycode = if E0_SEEN.swap(false, Ordering::SeqCst) {
        match code {
            0x1C => 96,  // keypad Enter
            0x1D => 97,  // right Ctrl
            0x35 => 98,  // keypad /
            0x37 => 99,  // Print Screen
            0x38 => 100, // right Alt
            0x47 => 102, // Home
            0x48 => 103, // Up
            0x49 => 104, // Page Up
            0x4B => 105, // Left
            0x4D => 106, // Right
            0x4F => 107, // End
            0x50 => 108, // Down
            0x51 => 109, // Page Down
            0x52 => 110, // Insert
            0x53 => 111, // Delete
            0x5B => 125, // left Meta
            0x5C => 126, // right Meta
            0x5D => 127, // Menu
            _ => return None,
        }
    } else {
        code // the rest: the keycode is the scancode
    };
    Some(keycode | release)
}

/// KDGKBENT: the key map entry of `keycode` in `table` (0 plain, 1 shift,
/// 2 AltGr, 3 shift+AltGr) as Linux's kb_value, (type << 8) | value: a US
/// keyboard. Unknown keys are K_HOLE.
pub fn keymap_entry(table: u8, keycode: u8) -> u16 {
    const KT_LATIN: u16 = 0;
    const KT_FN: u16 = 1;
    const KT_SPEC: u16 = 2;
    const KT_PAD: u16 = 3;
    const KT_CUR: u16 = 6;
    const KT_SHIFT: u16 = 7;
    const KT_LETTER: u16 = 11;
    const HOLE: u16 = KT_SPEC << 8;
    let shift = table & 1 != 0;
    if table & 2 != 0 {
        return HOLE; // no AltGr layer on a US keyboard
    }
    let k = |t: u16, v: u16| (t << 8) | v;
    const ROW1: &[u8; 12] = b"1234567890-=";
    const ROW1S: &[u8; 12] = b"!@#$%^&*()_+";
    let pick = |plain: u8, shifted: u8| k(KT_LATIN, if shift { shifted } else { plain } as u16);
    let letter = |c: u8| k(KT_LETTER, if shift { c.to_ascii_uppercase() } else { c } as u16);
    match keycode {
        1 => k(KT_LATIN, 0x1b),
        2..=13 => pick(ROW1[keycode as usize - 2], ROW1S[keycode as usize - 2]),
        14 => k(KT_LATIN, 0x7f),
        15 => k(KT_LATIN, 0x09),
        16..=25 => letter(b"qwertyuiop"[keycode as usize - 16]),
        26 => pick(b'[', b'{'),
        27 => pick(b']', b'}'),
        28 => k(KT_SPEC, 1), // Enter
        29 | 97 => k(KT_SHIFT, 2), // Ctrl
        30..=38 => letter(b"asdfghjkl"[keycode as usize - 30]),
        39 => pick(b';', b':'),
        40 => pick(b'\'', b'"'),
        41 => pick(b'`', b'~'),
        42 | 54 => k(KT_SHIFT, 0), // Shift
        43 => pick(b'\\', b'|'),
        44..=50 => letter(b"zxcvbnm"[keycode as usize - 44]),
        51 => pick(b',', b'<'),
        52 => pick(b'.', b'>'),
        53 => pick(b'/', b'?'),
        55 => k(KT_PAD, 12), // keypad *
        56 => k(KT_SHIFT, 3), // Alt
        57 => k(KT_LATIN, 0x20),
        58 => k(KT_SPEC, 7), // Caps Lock
        59..=68 => k(KT_FN, keycode as u16 - 59), // F1-F10
        69 => k(KT_SPEC, 8), // Num Lock
        70 => k(KT_SPEC, 6), // Scroll Lock
        71 => k(KT_PAD, 7),
        72 => k(KT_PAD, 8),
        73 => k(KT_PAD, 9),
        74 => k(KT_PAD, 11), // keypad -
        75 => k(KT_PAD, 4),
        76 => k(KT_PAD, 5),
        77 => k(KT_PAD, 6),
        78 => k(KT_PAD, 10), // keypad +
        79 => k(KT_PAD, 1),
        80 => k(KT_PAD, 2),
        81 => k(KT_PAD, 3),
        82 => k(KT_PAD, 0),
        83 => k(KT_PAD, 16), // keypad .
        86 => pick(b'<', b'>'),
        87 => k(KT_FN, 10), // F11
        88 => k(KT_FN, 11), // F12
        96 => k(KT_PAD, 14), // keypad Enter
        98 => k(KT_PAD, 13), // keypad /
        100 => k(KT_SHIFT, 1), // AltGr
        102 => k(KT_FN, 20), // Home (K_FIND)
        103 => k(KT_CUR, 3),
        104 => k(KT_FN, 24), // Page Up
        105 => k(KT_CUR, 1),
        106 => k(KT_CUR, 2),
        107 => k(KT_FN, 23), // End (K_SELECT)
        108 => k(KT_CUR, 0),
        109 => k(KT_FN, 25), // Page Down
        110 => k(KT_FN, 21), // Insert
        111 => k(KT_FN, 22), // Delete (K_REMOVE)
        119 => k(KT_FN, 29), // Pause
        _ => HOLE,
    }
}

pub fn intercept_scancode(scancode: u8) -> bool {
    if raw_mode() {
        return false; // the program gets every key, Ctrl-C included
    }
    const CTRL: u8 = 0x1d;
    const KEY_C: u8 = 0x2e;
    const KEY_Z: u8 = 0x2c;
    const KEY_BACKSLASH: u8 = 0x2b;
    match scancode {
        CTRL => CTRL_HELD.store(true, Ordering::SeqCst),
        s if s == CTRL | 0x80 => CTRL_HELD.store(false, Ordering::SeqCst),
        KEY_C | KEY_Z | KEY_BACKSLASH
            if CTRL_HELD.load(Ordering::SeqCst)
                && LFLAG.load(Ordering::SeqCst) & ISIG != 0
                && crate::process::foreground() != 0 =>
        {
            let sig = match scancode {
                KEY_C => crate::process::SIGINT,
                KEY_Z => crate::process::SIGTSTP,
                _ => crate::process::SIGQUIT,
            };
            crate::process::tty_signal(sig);
            return true;
        }
        _ => {}
    }
    false
}

/// Forget which keys are held and any half-typed input: called when the
/// kernel hands the keyboard to a program. Otherwise a key pressed in the
/// kernel shell and released here (or the reverse, like the Ctrl of the
/// Ctrl-D that left login) would look stuck, turning every letter into a
/// control character.
pub fn new_session() {
    *TTY.lock() = None;
    CTRL_HELD.store(false, Ordering::SeqCst);
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
        // Raw keyboard modes: the codes themselves, no line editing or echo.
        if raw_mode() {
            let mut n = 0;
            while n < buf.len() {
                let Some(scancode) = crate::task::keyboard::pop_scancode() else { break };
                if let Some(b) = raw_byte(scancode) {
                    buf[n] = b;
                    n += 1;
                }
            }
            if n > 0 {
                return n as i64;
            }
        } else {
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
        if raw_mode() && m.lflag & ICANON == 0 && m.vmin == 0 && timeout.is_none() {
            return 0;
        }

        if let Some(deadline) = timeout {
            if crate::process::ticks() >= deadline {
                return 0; // nothing typed in time
            }
        }

        if crate::process::signal_pending() {
            return -crate::syscall::EINTR;
        }

        // Sleep until a key arrives (the scheduler wakes CONSOLE_KEY when
        // the keyboard queue isn't empty), a signal, or the VTIME timeout.
        draw_cursor();
        match timeout {
            Some(deadline) => crate::process::block_on_until(crate::process::CONSOLE_KEY, deadline),
            None => crate::process::block_on(crate::process::CONSOLE_KEY),
        }
    }
}

/// Is there input to read (for poll)? Processes keys typed so far, as a
/// read would (echoing them, finishing lines).
pub fn input_ready() -> bool {
    crate::task::keyboard::init_queue();
    if raw_mode() {
        return crate::task::keyboard::has_scancodes();
    }
    let mut guard = TTY.lock();
    let tty = guard.get_or_insert_with(|| Tty {
        keyboard: Keyboard::new(ScancodeSet1::new(), layouts::Us104Key, HandleControl::MapLettersToUnicode),
        line: Vec::new(),
        ready: VecDeque::new(),
        eof: false,
    });
    while let Some(scancode) = crate::task::keyboard::pop_scancode() {
        tty.handle_scancode(scancode);
    }
    !tty.ready.is_empty() || tty.eof
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

// Copyright (c) 2026 tre4surehunter9
//! The console TTY: turns keyboard scancodes into bytes for read(0, ...).
//!
//! Works like a POSIX terminal in canonical ("cooked") mode: typed characters
//! are echoed and collected into a line, backspace edits the line, and read()
//! only returns once Enter is pressed. Ctrl-D on an empty line is end-of-file
//! (read returns 0); on a non-empty line it hands over the line without '\n'.

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use pc_keyboard::{layouts, DecodedKey, HandleControl, Keyboard, ScancodeSet1};
use spin::Mutex;

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
/// Returns the number of bytes read; 0 means end-of-file.
///
/// Called from the read syscall, so interrupts are disabled on entry. While
/// there's nothing to read, other ready processes run; if there are none,
/// the CPU halts until the next interrupt (e.g. a key press).
pub fn read(buf: &mut [u8]) -> usize {
    use crate::vga_buffer::{draw_cursor, erase_cursor};
    use x86_64::instructions::interrupts;

    if buf.is_empty() {
        return 0;
    }
    crate::task::keyboard::init_queue();

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
                return n;
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

        if crate::process::others_ready() {
            // Let background processes run while we wait for a key.
            erase_cursor();
            crate::process::yield_now();
        } else {
            draw_cursor();
            // sti; hlt is atomic with respect to interrupts, so a key pressed
            // between the check above and the hlt still wakes us up.
            interrupts::enable_and_hlt();
            interrupts::disable();
        }
    }
}

impl Tty {
    fn handle_scancode(&mut self, scancode: u8) {
        let Ok(Some(event)) = self.keyboard.add_byte(scancode) else { return };
        let event = crate::task::keyboard::fix_key_event(event);
        let Some(DecodedKey::Unicode(c)) = self.keyboard.process_keyevent(event) else { return };

        match c {
            '\n' => {
                crate::println!();
                self.line.push(b'\n');
                self.ready.extend(self.line.drain(..));
            }
            '\x08' => {
                if self.line.pop().is_some() {
                    crate::print!("\x08");
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
                crate::print!("{}", c);
            }
            _ => {}
        }
    }
}

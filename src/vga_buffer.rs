// Copyright (c) 2026 tre4surehunter9

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Color {
    Black = 0,
    Blue = 1,
    Green = 2,
    Cyan = 3,
    Red = 4,
    Magenta = 5,
    Brown = 6,
    LightGray = 7,
    DarkGray = 8,
    LightBlue = 9,
    LightGreen = 10,
    LightCyan = 11,
    LightRed = 12,
    Pink = 13,
    Yellow = 14,
    White = 15,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
struct ColorCode(u8);

impl ColorCode {
    fn new(foreground: Color, background: Color) -> ColorCode {
        ColorCode((background as u8) << 4 | (foreground as u8))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
struct ScreenChar {
    ascii_character: u8,
    color_code: ColorCode,
}

const BUFFER_HEIGHT: usize = 25;
const BUFFER_WIDTH:  usize = 80;

use volatile::Volatile;

struct Buffer {
    chars: [[Volatile<ScreenChar>; BUFFER_WIDTH]; BUFFER_HEIGHT],
}

pub struct Writer {
    row_position: usize,
    column_position: usize,
    color_code: ColorCode,
    buffer: &'static mut Buffer,
    /// ANSI text attributes (see write_ansi), as VGA color numbers.
    fg: u8,
    bg: u8,
    bold: bool,
    reverse: bool,
    /// The start of an escape sequence that was cut off at the end of a
    /// write; it continues with the next write (like a real terminal).
    pending: [u8; 32],
    pending_len: usize,
}


impl Writer {
    pub fn write_byte(&mut self, byte: u8) {
        match byte {
            b'\n' => self.new_line(),
            0x08 => {
                if self.column_position > 0 {
                    self.column_position -= 1;
                } else if self.row_position > 0 {
                    // The line wrapped: go back to the end of the row above.
                    self.row_position -= 1;
                    self.column_position = BUFFER_WIDTH - 1;
                } else {
                    return;
                }
                let row = self.row_position;
                let col = self.column_position;
                self.buffer.chars[row][col].write(ScreenChar {
                    ascii_character: b' ',
                    color_code: self.color_code,
                });
            }
            byte => {
                if self.column_position >= BUFFER_WIDTH {
                    self.new_line();
                }
                let row = self.row_position;
                let col = self.column_position;
                let color_code = self.color_code;
                self.buffer.chars[row][col].write(ScreenChar {
                    ascii_character: byte,
                    color_code,
                });
                self.column_position += 1;
            }
        }
    }
}

impl Writer {
    pub fn write_string(&mut self, s: &str) {
        for byte in s.bytes() {
            match byte {
                // printable ascii byte or new line
                0x08 => self.write_byte(0x08),
                0x20..=0x7e | b'\n' => self.write_byte(byte),
                // not part of printable ascii range
                _ => self.write_byte(0xfe),
            }

        }
    }
}


// formatting macros

use core::fmt;

impl fmt::Write for Writer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_string(s);
        Ok(())
    }
}

//new lines
impl Writer {
    fn new_line(&mut self) {
        if self.row_position < BUFFER_HEIGHT - 1 {
            self.row_position += 1;
        } else {
            for row in 1..BUFFER_HEIGHT {
                for col in 0..BUFFER_WIDTH {
                    let character = self.buffer.chars[row][col].read();
                    self.buffer.chars[row - 1][col].write(character);
                }
            }

            self.clear_row(BUFFER_HEIGHT - 1);

        }
        self.column_position = 0;
    }
}


// finish new line code

impl Writer {
    fn clear_row(&mut self, row: usize) {
        let blank = ScreenChar {
            ascii_character: b' ',
            color_code: self.color_code
        };
        for col in 0..BUFFER_WIDTH {
            self.buffer.chars[row][col].write(blank);
        }
    }
}


//spinlocks
use spin::Mutex;
use lazy_static::lazy_static;

lazy_static! {
    pub static ref WRITER: Mutex<Writer> = Mutex::new(Writer {
        column_position: 0,
        row_position: 0,
        color_code: ColorCode::new(Color::Yellow, Color::Black),
        buffer: unsafe { &mut *(0xb8000 as *mut Buffer) },
        fg: DEFAULT_FG,
        bg: DEFAULT_BG,
        bold: false,
        reverse: false,
        pending: [0; 32],
        pending_len: 0,
    });
}

// macro

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::vga_buffer::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}



#[test_case]
fn test_println_simple() {
    println!("test_println_simple output")
}

#[test_case]
fn test_println_many() {
    for _ in 0..200 {
        println!("test_println_many output")
    }
}

#[test_case]
fn test_println_output() {
    use core::fmt::Write;
    use x86_64::instructions::interrupts;

    let s = "Some test string that fits on a single line";
    interrupts::without_interrupts(|| {
        let mut writer = WRITER.lock();
        writeln!(writer, "\n{}", s).expect("writeln failed");
        for (i, c) in s.chars().enumerate() {
            let screen_char = writer.buffer.chars[BUFFER_HEIGHT - 2][i].read();
            assert_eq!(char::from(screen_char.ascii_character), c);
        }
    });
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    use x86_64::instructions::interrupts;

    interrupts::without_interrupts(|| {
        let mut writer = WRITER.lock();
        writer.write_fmt(args).unwrap();
        writer.update_hw_cursor();
    });
}


pub fn draw_cursor() {
    WRITER.lock().draw_cursor_inner();
}

pub fn erase_cursor() {
    WRITER.lock().erase_cursor_inner();
}

impl Writer {
/// Move the blinking VGA hardware cursor to the writing position. (A drawn
/// cursor character would hide the text underneath it, e.g. in the editor.)
pub fn draw_cursor_inner(&mut self) {
    // At the end of a line the next character wraps (see write_byte),
    // so the cursor belongs at the start of the next line.
    if self.column_position >= BUFFER_WIDTH {
        self.new_line();
    }
    self.update_hw_cursor();
 }

fn update_hw_cursor(&self) {
    use x86_64::instructions::port::Port;
    let pos = (self.row_position * BUFFER_WIDTH + self.column_position.min(BUFFER_WIDTH - 1)) as u16;
    unsafe {
        let mut index: Port<u8> = Port::new(0x3D4);
        let mut data: Port<u8> = Port::new(0x3D5);
        // Make sure the cursor is enabled, as an underline (scanlines 14-15).
        index.write(0x0A);
        data.write(14);
        index.write(0x0B);
        data.write(15);
        index.write(0x0F);
        data.write((pos & 0xFF) as u8);
        index.write(0x0E);
        data.write((pos >> 8) as u8);
    }
}
}

impl Writer {
/// Nothing to erase with a hardware cursor; kept so callers don't change.
pub fn erase_cursor_inner(&mut self) {}
}

pub fn clear_screen() {
    WRITER.lock().clear_screen_inner();
}

impl Writer {
    pub fn clear_screen_inner(&mut self) {
        for row in 0..BUFFER_HEIGHT {
            self.clear_row(row);
        }
        self.row_position = 0;
        self.column_position = 0;
    }
}

impl Writer {
    pub fn set_color(&mut self, fg: Color, bg: Color) {
        self.color_code = ColorCode::new(fg, bg);
    }
}

impl Writer {
    pub fn set_cursor_pos(&mut self, row: usize, col: usize) {
        self.row_position    = row.min(BUFFER_HEIGHT - 1);
        self.column_position = col.min(BUFFER_WIDTH  - 1);
    }
}



// ---- ANSI escape sequences (used by user programs through the console) ----

const DEFAULT_FG: u8 = Color::Yellow as u8;
const DEFAULT_BG: u8 = Color::Black as u8;

/// ANSI color number (0-7) to VGA color number.
const ANSI_TO_VGA: [u8; 8] = [0, 4, 2, 6, 1, 5, 3, 7];

impl Writer {
    /// Write bytes from a program, interpreting the common ANSI/VT100 CSI
    /// sequences: cursor movement (A B C D H f), erase (J K), and text
    /// attributes (m: 0 reset, 1 bold, 7 reverse, 30-37/40-47 colors, 39/49
    /// defaults). Unknown sequences are dropped instead of shown as garbage.
    pub fn write_ansi(&mut self, bytes: &[u8]) {
        // Continue a sequence left unfinished by the previous write.
        let joined;
        let bytes = if self.pending_len > 0 {
            let mut v = alloc::vec::Vec::with_capacity(self.pending_len + bytes.len());
            v.extend_from_slice(&self.pending[..self.pending_len]);
            v.extend_from_slice(bytes);
            self.pending_len = 0;
            joined = v;
            &joined[..]
        } else {
            bytes
        };
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            if b == 0x1b && i + 1 == bytes.len() {
                self.pending[0] = 0x1b;
                self.pending_len = 1;
                break;
            }
            if b == 0x1b && bytes.get(i + 1) == Some(&b'[') {
                let mut params = [0u16; 8];
                let mut count = 0;
                let mut have_digit = false;
                let mut j = i + 2;
                while j < bytes.len() {
                    match bytes[j] {
                        d @ b'0'..=b'9' => {
                            if count < params.len() {
                                params[count] = params[count].saturating_mul(10).saturating_add((d - b'0') as u16);
                            }
                            have_digit = true;
                        }
                        b';' => {
                            count += 1;
                            have_digit = false;
                        }
                        b'?' => {}
                        _ => break,
                    }
                    j += 1;
                }
                if j >= bytes.len() {
                    // Cut off: keep it for the next write (unless absurdly long).
                    let rest = &bytes[i..];
                    if rest.len() <= self.pending.len() {
                        self.pending[..rest.len()].copy_from_slice(rest);
                        self.pending_len = rest.len();
                    }
                    break;
                }
                let n = (count + if have_digit || count > 0 { 1 } else { 0 }).min(params.len());
                self.csi(bytes[j], &params[..n]);
                i = j + 1;
                continue;
            }
            match b {
                b'\n' => self.new_line(),
                b'\r' => self.column_position = 0,
                0x08 => self.write_byte(0x08),
                b'\t' => {
                    let next = (self.column_position / 8 + 1) * 8;
                    while self.column_position < next.min(BUFFER_WIDTH) {
                        self.write_byte(b' ');
                    }
                }
                0x07 => {} // bell
                0x20..=0x7e => self.write_byte(b),
                _ => self.write_byte(0xfe),
            }
            i += 1;
        }
        self.update_hw_cursor();
    }

    fn csi(&mut self, cmd: u8, params: &[u16]) {
        let arg = |i: usize, default: u16| -> usize {
            match params.get(i) {
                Some(&0) | None => default as usize,
                Some(&v) => v as usize,
            }
        };
        let total = BUFFER_WIDTH * BUFFER_HEIGHT;
        let pos = self.row_position * BUFFER_WIDTH + self.column_position;
        let set_pos = |w: &mut Writer, p: usize| {
            let p = p.min(total - 1);
            w.row_position = p / BUFFER_WIDTH;
            w.column_position = p % BUFFER_WIDTH;
        };
        match cmd {
            // Moves wrap across lines, so line editors can move over wrapped input.
            b'A' => self.row_position = self.row_position.saturating_sub(arg(0, 1)),
            b'B' => self.row_position = (self.row_position + arg(0, 1)).min(BUFFER_HEIGHT - 1),
            b'C' => set_pos(self, pos + arg(0, 1)),
            b'D' => set_pos(self, pos.saturating_sub(arg(0, 1))),
            b'H' | b'f' => {
                self.row_position = (arg(0, 1) - 1).min(BUFFER_HEIGHT - 1);
                self.column_position = (arg(1, 1) - 1).min(BUFFER_WIDTH - 1);
            }
            b'J' => {
                let (from, to) = match params.first().copied().unwrap_or(0) {
                    0 => (pos, total),
                    1 => (0, pos + 1),
                    _ => (0, total),
                };
                self.blank(from, to);
            }
            b'K' => {
                let line = self.row_position * BUFFER_WIDTH;
                let (from, to) = match params.first().copied().unwrap_or(0) {
                    0 => (pos, line + BUFFER_WIDTH),
                    1 => (line, pos + 1),
                    _ => (line, line + BUFFER_WIDTH),
                };
                self.blank(from, to);
            }
            b'm' => {
                if params.is_empty() {
                    self.reset_attributes();
                }
                for &p in params {
                    match p {
                        0 => self.reset_attributes(),
                        1 => self.bold = true,
                        22 => self.bold = false,
                        7 => self.reverse = true,
                        27 => self.reverse = false,
                        30..=37 => self.fg = ANSI_TO_VGA[(p - 30) as usize],
                        39 => self.fg = DEFAULT_FG,
                        40..=47 => self.bg = ANSI_TO_VGA[(p - 40) as usize],
                        49 => self.bg = DEFAULT_BG,
                        90..=97 => self.fg = ANSI_TO_VGA[(p - 90) as usize] | 8,
                        _ => {}
                    }
                }
                self.apply_attributes();
            }
            _ => {}
        }
    }

    fn reset_attributes(&mut self) {
        self.fg = DEFAULT_FG;
        self.bg = DEFAULT_BG;
        self.bold = false;
        self.reverse = false;
    }

    fn apply_attributes(&mut self) {
        let fg = if self.bold { self.fg | 8 } else { self.fg };
        let (fg, bg) = if self.reverse { (self.bg, fg) } else { (fg, self.bg) };
        self.color_code = ColorCode((bg & 0x7) << 4 | (fg & 0xf));
    }

    /// Blank the screen cells [from, to) (linear positions).
    fn blank(&mut self, from: usize, to: usize) {
        let blank = ScreenChar { ascii_character: b' ', color_code: self.color_code };
        for p in from..to.min(BUFFER_WIDTH * BUFFER_HEIGHT) {
            self.buffer.chars[p / BUFFER_WIDTH][p % BUFFER_WIDTH].write(blank);
        }
    }

    /// Restore the default colors (when a program leaves them changed).
    pub fn reset_ansi(&mut self) {
        self.reset_attributes();
        self.apply_attributes();
    }
}

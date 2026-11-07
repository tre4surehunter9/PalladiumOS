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
    /// The last character printed (for CSI b, "repeat it").
    last_char: u8,
    /// Inside a string sequence (OSC, DCS, APC, PM, SOS: ESC ] ... BEL or
    /// ESC \), which is skipped; and whether its last byte was ESC.
    in_string: bool,
    string_esc: bool,
    /// Where ESC 7 saved the cursor.
    saved_cursor: (usize, usize),
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
                self.last_char = byte;
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
        // The text buffer (physical 0xb8000), through the kernel's
        // physical-memory window (the low half belongs to programs).
        buffer: unsafe { &mut *((crate::memory::PHYS_OFFSET + 0xb8000) as *mut Buffer) },
        fg: DEFAULT_FG,
        bg: DEFAULT_BG,
        bold: false,
        reverse: false,
        pending: [0; 32],
        pending_len: 0,
        last_char: b' ',
        in_string: false,
        string_esc: false,
        saved_cursor: (0, 0),
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
fn test_ansi_editing_sequences() {
    // What full-screen programs (Alpine's nano, via TERM=ansi) rely on.
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| {
        let mut w = WRITER.lock();
        // (No heap in this test kernel: compare rows as bytes.)
        let is = |w: &Writer, r: usize, want: &str| -> bool {
            (0..BUFFER_WIDTH).all(|c| w.buffer.chars[r][c].read().ascii_character == *want.as_bytes().get(c).unwrap_or(&b' '))
        };
        w.write_ansi(b"\x1b[2J\x1b[H");
        // Absolute row (d) and column (G).
        w.write_ansi(b"\x1b[25dbottom\x1b[5Gx");
        assert!(is(&w, 24, "bottxm")); // "bottom" with column 5 overwritten
        // Insert (@) and delete (P) characters, erase in place (X).
        w.write_ansi(b"\x1b[1;1Habcdef\x1b[1;3H\x1b[2@");
        assert!(is(&w, 0, "ab  cdef"));
        w.write_ansi(b"\x1b[1;3H\x1b[2P");
        assert!(is(&w, 0, "abcdef"));
        w.write_ansi(b"\x1b[1;2H\x1b[3X");
        assert!(is(&w, 0, "a   ef"));
        // Insert (L) and delete (M) lines, repeat (b).
        w.write_ansi(b"\x1b[2;1Hsecond\x1b[3;1Hthird\x1b[2;1H\x1b[L");
        assert!(is(&w, 1, "") && is(&w, 2, "second") && is(&w, 3, "third"));
        w.write_ansi(b"\x1b[2;1H\x1b[M");
        assert!(is(&w, 1, "second") && is(&w, 2, "third"));
        w.write_ansi(b"\x1b[5;1H-\x1b[4b");
        assert!(is(&w, 4, "-----"));
        // Scroll up (S).
        w.write_ansi(b"\x1b[1S");
        assert!(is(&w, 0, "second") && is(&w, 1, "third"));
        // String sequences (bash's OSC 3008 under systemd) are skipped,
        // even split across writes; ESC 7 / ESC 8; private CSI ignored.
        w.write_ansi(b"\x1b[2J\x1b[Hab\x1b]3008;start=x;cwd=/home");
        w.write_ansi(b"/someone\x1b\\cd\x1b]0;title\x07e\x1b[>4;1m\x1b[!pf\x0eg");
        assert!(is(&w, 0, "abcdefg"));
        w.write_ansi(b"\x1b[3;5H\x1b7\x1b[10;10Hx\x1b8y\x1b(Bz");
        assert!(is(&w, 2, "    yz"));
        w.write_ansi(b"\x1b[2J\x1b[H");
    });
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

pub fn update_hw_cursor(&self) {
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
            // The body of a string sequence (like the OSC 3008 status reports
            // bash sends under systemd): skipped up to BEL or ESC \, even
            // across writes.
            if self.in_string {
                if b == 0x07 || (self.string_esc && b == b'\\') {
                    self.in_string = false;
                }
                self.string_esc = b == 0x1b;
                i += 1;
                continue;
            }
            if b == 0x1b && i + 1 == bytes.len() {
                self.pending[0] = 0x1b;
                self.pending_len = 1;
                break;
            }
            if b == 0x1b && bytes.get(i + 1) != Some(&b'[') {
                let next = bytes[i + 1];
                match next {
                    // OSC, DCS, SOS, PM, APC: a string, skipped.
                    b']' | b'P' | b'X' | b'^' | b'_' => {
                        self.in_string = true;
                        self.string_esc = false;
                        i += 2;
                    }
                    // Character set selection: ESC ( B and the like (ignored).
                    b'(' | b')' | b'*' | b'+' | b'-' | b'.' | b'/' | b'#' | b'%' => {
                        if i + 2 >= bytes.len() {
                            let rest = &bytes[i..];
                            self.pending[..rest.len()].copy_from_slice(rest);
                            self.pending_len = rest.len();
                            break;
                        }
                        i += 3;
                    }
                    b'7' => {
                        self.saved_cursor = (self.row_position, self.column_position);
                        i += 2;
                    }
                    b'8' => {
                        (self.row_position, self.column_position) = self.saved_cursor;
                        i += 2;
                    }
                    // Index (down a line, scrolling at the bottom), next line,
                    // reverse index (up a line, scrolling at the top).
                    b'D' => {
                        let col = self.column_position;
                        self.new_line();
                        self.column_position = col;
                        i += 2;
                    }
                    b'E' => {
                        self.new_line();
                        self.column_position = 0;
                        i += 2;
                    }
                    b'M' => {
                        if self.row_position > 0 {
                            self.row_position -= 1;
                        } else {
                            self.shift_rows(0, 1, true);
                        }
                        i += 2;
                    }
                    // Full reset: default colors, a clear screen.
                    b'c' => {
                        self.reset_attributes();
                        self.apply_attributes();
                        self.blank(0, BUFFER_WIDTH * BUFFER_HEIGHT);
                        self.row_position = 0;
                        self.column_position = 0;
                        i += 2;
                    }
                    // Keypad modes (ESC = / ESC >) and anything else: ignored.
                    _ => i += 2,
                }
                continue;
            }
            if b == 0x1b && bytes.get(i + 1) == Some(&b'[') {
                // CSI: parameter bytes (0x30-0x3F), intermediate bytes
                // (0x20-0x2F), then a final byte (0x40-0x7E). Sequences with
                // a private prefix other than '?' (like ESC[>4;1m) or with
                // intermediates (like ESC[!p) are read whole and ignored.
                let mut params = [0u16; 8];
                let mut count = 0;
                let mut have_digit = false;
                let mut ignore = false;
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
                        b':' | b'<' | b'=' | b'>' | 0x20..=0x2f => ignore = true,
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
                if !ignore {
                    self.csi(bytes[j], &params[..n]);
                }
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
                0x20..=0x7e => self.write_byte(b),
                // Other control characters (bell, shift in/out...) aren't
                // printed, as on a terminal.
                0x00..=0x1f | 0x7f => {}
                // (Not ASCII: one block per byte, the VGA font has no UTF-8.)
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
            // Absolute column / row, relative moves, next / previous line.
            b'G' | b'`' => self.column_position = (arg(0, 1) - 1).min(BUFFER_WIDTH - 1),
            b'd' => self.row_position = (arg(0, 1) - 1).min(BUFFER_HEIGHT - 1),
            b'a' => self.column_position = (self.column_position + arg(0, 1)).min(BUFFER_WIDTH - 1),
            b'e' => self.row_position = (self.row_position + arg(0, 1)).min(BUFFER_HEIGHT - 1),
            b'E' => {
                self.row_position = (self.row_position + arg(0, 1)).min(BUFFER_HEIGHT - 1);
                self.column_position = 0;
            }
            b'F' => {
                self.row_position = self.row_position.saturating_sub(arg(0, 1));
                self.column_position = 0;
            }
            // Editing: insert / delete / erase characters and lines, scroll.
            b'@' => self.insert_chars(arg(0, 1)),
            b'P' => self.delete_chars(arg(0, 1)),
            b'X' => {
                let line_end = self.row_position * BUFFER_WIDTH + BUFFER_WIDTH;
                self.blank(pos, (pos + arg(0, 1)).min(line_end));
            }
            b'L' => self.insert_lines(arg(0, 1)),
            b'M' => self.delete_lines(arg(0, 1)),
            b'S' => self.scroll(arg(0, 1), true),
            b'T' => self.scroll(arg(0, 1), false),
            b'b' => {
                for _ in 0..arg(0, 1).min(BUFFER_WIDTH * BUFFER_HEIGHT) {
                    self.write_byte(self.last_char);
                }
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
    fn blank_cell(&self) -> ScreenChar {
        ScreenChar { ascii_character: b' ', color_code: self.color_code }
    }

    /// CSI @: insert `n` blanks at the cursor; the rest of the line moves right.
    fn insert_chars(&mut self, n: usize) {
        let (row, col) = (self.row_position, self.column_position.min(BUFFER_WIDTH - 1));
        for c in (col..BUFFER_WIDTH).rev() {
            let ch = if c >= col + n { self.buffer.chars[row][c - n].read() } else { self.blank_cell() };
            self.buffer.chars[row][c].write(ch);
        }
    }

    /// CSI P: delete `n` characters at the cursor; the rest moves left.
    fn delete_chars(&mut self, n: usize) {
        let (row, col) = (self.row_position, self.column_position.min(BUFFER_WIDTH - 1));
        for c in col..BUFFER_WIDTH {
            let ch = if c + n < BUFFER_WIDTH { self.buffer.chars[row][c + n].read() } else { self.blank_cell() };
            self.buffer.chars[row][c].write(ch);
        }
    }

    /// Move rows [from, end) down (`down`) or up by `n`, blanking what's left.
    /// (No allocation: the console also prints during boot and panics.)
    fn shift_rows(&mut self, from: usize, n: usize, down: bool) {
        for i in 0..BUFFER_HEIGHT.saturating_sub(from) {
            let r = if down { BUFFER_HEIGHT - 1 - i } else { from + i };
            let src = if down { r.checked_sub(n).filter(|&s| s >= from) } else { Some(r + n).filter(|&s| s < BUFFER_HEIGHT) };
            for c in 0..BUFFER_WIDTH {
                let ch = match src {
                    Some(s) => self.buffer.chars[s][c].read(),
                    None => self.blank_cell(),
                };
                self.buffer.chars[r][c].write(ch);
            }
        }
    }

    /// CSI L: insert `n` blank lines at the cursor's line.
    fn insert_lines(&mut self, n: usize) {
        self.shift_rows(self.row_position, n, true);
        self.column_position = 0;
    }

    /// CSI M: delete `n` lines at the cursor's line.
    fn delete_lines(&mut self, n: usize) {
        self.shift_rows(self.row_position, n, false);
        self.column_position = 0;
    }

    /// CSI S / T: scroll the whole screen up / down `n` lines (the cursor
    /// stays put).
    fn scroll(&mut self, n: usize, up: bool) {
        self.shift_rows(0, n, !up);
    }

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

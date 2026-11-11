// Copyright (c) 2026 tre4surehunter9
//! The console: a grid of character cells with VGA colors, drawn into the
//! framebuffer the bootloader set up (bootloader 0.11: a graphics mode, not
//! VGA text mode) with a 9x16 font, at as many cells as fit (213x67 at
//! 1920x1080, 142x45 at 1280x720; very big screens get the font scaled up,
//! 2x at 2560x1600). Before the framebuffer is set up (early boot), text goes into
//! the grid only. The grid stays readable for tests and for QEMU's monitor
//! (its address is logged to the serial port at boot).

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

/// The largest grid (cells); the real size depends on the framebuffer.
pub const MAX_WIDTH: usize = 240;
pub const MAX_HEIGHT: usize = 80;
/// The grid's size before the framebuffer is set up.
const EARLY_WIDTH: usize = 80;
const EARLY_HEIGHT: usize = 25;
/// A cell in pixels (the font's glyphs).
const CELL_W: usize = noto_sans_mono_bitmap::get_raster_width(FontWeight::Regular, RasterHeight::Size16);
const CELL_H: usize = 16;

use noto_sans_mono_bitmap::{get_raster, FontWeight, RasterHeight};

type Grid = [[ScreenChar; MAX_WIDTH]; MAX_HEIGHT];
static mut GRID: Grid = [[ScreenChar { ascii_character: b' ', color_code: ColorCode(0x0e) }; MAX_WIDTH]; MAX_HEIGHT];

/// The framebuffer, as the bootloader described it.
struct Fb {
    buf: *mut u8,
    len: usize,
    width: usize,
    height: usize,
    stride: usize,
    bpp: usize,
    bgr: bool,
    gray: bool,
    /// Each font pixel is drawn as scale x scale pixels (big screens, like
    /// UEFI's 2560x1600, get bigger text instead of a corner of tiny text).
    scale: usize,
}

// (Only touched with the WRITER lock held.)
unsafe impl Send for Fb {}

/// The 16 VGA colors, as RGB.
const PALETTE: [[u8; 3]; 16] = [
    [0x00, 0x00, 0x00], [0x00, 0x00, 0xaa], [0x00, 0xaa, 0x00], [0x00, 0xaa, 0xaa],
    [0xaa, 0x00, 0x00], [0xaa, 0x00, 0xaa], [0xaa, 0x55, 0x00], [0xaa, 0xaa, 0xaa],
    [0x55, 0x55, 0x55], [0x55, 0x55, 0xff], [0x55, 0xff, 0x55], [0x55, 0xff, 0xff],
    [0xff, 0x55, 0x55], [0xff, 0x55, 0xff], [0xff, 0xff, 0x55], [0xff, 0xff, 0xff],
];

pub struct Writer {
    row_position: usize,
    column_position: usize,
    color_code: ColorCode,
    grid: &'static mut Grid,
    /// The grid's size in use (cells).
    width: usize,
    height: usize,
    fb: Option<Fb>,
    /// Where the cursor is drawn (an underline), to erase it when it moves.
    cursor_at: Option<(usize, usize)>,
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
                    self.column_position = self.width - 1;
                } else {
                    return;
                }
                let row = self.row_position;
                let col = self.column_position;
                self.put(row, col, ScreenChar {
                    ascii_character: b' ',
                    color_code: self.color_code,
                });
            }
            byte => {
                if self.column_position >= self.width {
                    self.new_line();
                }
                let row = self.row_position;
                let col = self.column_position;
                let color_code = self.color_code;
                self.put(row, col, ScreenChar {
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
        if self.row_position < self.height - 1 {
            self.row_position += 1;
        } else {
            self.scroll_up_one();
        }
        self.column_position = 0;
    }

    /// Scroll the whole screen up a line: the grid, and the framebuffer's
    /// pixels in one copy (not redrawn cell by cell).
    fn scroll_up_one(&mut self) {
        self.hide_cursor();
        for row in 1..self.height {
            self.grid[row - 1] = self.grid[row];
        }
        if let Some(fb) = self.fb.as_ref().filter(|_| !GRAPHICS.load(core::sync::atomic::Ordering::Relaxed)) {
            let row_bytes = fb.stride * fb.bpp * CELL_H * fb.scale;
            let used = row_bytes * self.height;
            if used <= fb.len {
                // SAFETY: inside the framebuffer; copy handles the overlap.
                unsafe { core::ptr::copy(fb.buf.add(row_bytes), fb.buf, used - row_bytes) };
            }
        }
        self.clear_row(self.height - 1);
    }
}


// finish new line code

impl Writer {
    fn clear_row(&mut self, row: usize) {
        let blank = ScreenChar {
            ascii_character: b' ',
            color_code: self.color_code
        };
        for col in 0..self.width {
            self.put(row, col, blank);
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
        grid: unsafe { &mut *core::ptr::addr_of_mut!(GRID) },
        width: EARLY_WIDTH,
        height: EARLY_HEIGHT,
        fb: None,
        cursor_at: None,
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
            (0..w.width).all(|c| w.get(r, c).ascii_character == *want.as_bytes().get(c).unwrap_or(&b' '))
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
            let screen_char = writer.get(writer.height - 2, i);
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
    if self.column_position >= self.width {
        self.new_line();
    }
    self.update_hw_cursor();
 }

/// Show the cursor (an underline in the cell) where the next character goes.
pub fn update_hw_cursor(&mut self) {
    let at = (self.row_position.min(self.height - 1), self.column_position.min(self.width - 1));
    if self.cursor_at == Some(at) {
        return;
    }
    self.hide_cursor();
    self.cursor_at = Some(at);
    self.draw(at.0, at.1);
}

/// Erase the drawn cursor (redraw its cell plainly).
fn hide_cursor(&mut self) {
    if let Some((r, c)) = self.cursor_at.take() {
        if r < self.height && c < self.width {
            self.draw(r, c);
        }
    }
}
}

impl Writer {
    fn get(&self, row: usize, col: usize) -> ScreenChar {
        self.grid[row][col]
    }

    /// Change a cell: in the grid, and on the screen.
    fn put(&mut self, row: usize, col: usize, ch: ScreenChar) {
        if self.grid[row][col] == ch && self.fb.is_none() {
            return;
        }
        self.grid[row][col] = ch;
        self.draw(row, col);
    }

    /// Draw cell (row, col) into the framebuffer (with the cursor's
    /// underline if it's there).
    fn draw(&mut self, row: usize, col: usize) {
        if GRAPHICS.load(core::sync::atomic::Ordering::Relaxed) {
            return; // a program draws the screen (KDSETMODE KD_GRAPHICS)
        }
        let Some(fb) = &self.fb else { return };
        let ch = self.grid[row][col];
        let fg = PALETTE[(ch.color_code.0 & 0xf) as usize];
        let bg = PALETTE[(ch.color_code.0 >> 4 & 0xf) as usize];
        let cursor = self.cursor_at == Some((row, col));
        let glyph = match ch.ascii_character {
            c @ 0x21..=0x7e => get_raster(c as char, FontWeight::Regular, RasterHeight::Size16),
            _ => None,
        };
        let k = fb.scale;
        let (x0, y0) = (col * CELL_W * k, row * CELL_H * k);
        if x0 + CELL_W * k > fb.width || y0 + CELL_H * k > fb.height {
            return;
        }
        for py in 0..CELL_H * k {
            let y = py / k;
            let line = unsafe { fb.buf.add(((y0 + py) * fb.stride + x0) * fb.bpp) };
            for px_i in 0..CELL_W * k {
                let x = px_i / k;
                // How much foreground: the glyph's coverage, a solid block for
                // 0xfe (the VGA font's square), full for the cursor's line.
                let mut on: u32 = match (ch.ascii_character, &glyph) {
                    (0xfe, _) => if (2..CELL_W - 1).contains(&x) && (4..CELL_H - 3).contains(&y) { 255 } else { 0 },
                    (_, Some(g)) => g.raster().get(y).and_then(|r| r.get(x)).copied().unwrap_or(0) as u32,
                    _ => 0,
                };
                if cursor && y >= CELL_H - 2 {
                    on = 255;
                }
                let mix = |f: u8, b: u8| ((f as u32 * on + b as u32 * (255 - on)) / 255) as u8;
                let (r, g, b) = (mix(fg[0], bg[0]), mix(fg[1], bg[1]), mix(fg[2], bg[2]));
                unsafe {
                    let px = line.add(px_i * fb.bpp);
                    if fb.gray {
                        *px = ((r as u16 + g as u16 + b as u16) / 3) as u8;
                    } else if fb.bgr {
                        *px = b;
                        *px.add(1) = g;
                        *px.add(2) = r;
                    } else {
                        *px = r;
                        *px.add(1) = g;
                        *px.add(2) = b;
                    }
                }
            }
        }
    }

    /// Start drawing into the framebuffer: as many cells as fit, cleared.
    fn use_framebuffer(&mut self, fb: Fb) {
        self.width = (fb.width / (CELL_W * fb.scale)).clamp(1, MAX_WIDTH);
        self.height = (fb.height / (CELL_H * fb.scale)).clamp(1, MAX_HEIGHT);
        // Black around the cells (the right and bottom edges).
        unsafe { core::ptr::write_bytes(fb.buf, 0, fb.len) };
        self.fb = Some(fb);
        self.cursor_at = None;
        self.clear_screen_inner();
    }
}

/// Set up the console on the bootloader's framebuffer.
pub fn init_framebuffer(fb: bootloader_api::info::FrameBuffer) {
    use bootloader_api::info::PixelFormat;
    let info = fb.info();
    let buffer = fb.into_buffer();
    let fb = Fb {
        buf: buffer.as_mut_ptr(),
        len: buffer.len(),
        width: info.width,
        height: info.height,
        stride: info.stride,
        bpp: info.bytes_per_pixel,
        bgr: !matches!(info.pixel_format, PixelFormat::Rgb),
        gray: matches!(info.pixel_format, PixelFormat::U8),
        // 1x up to about 1280x720, 2x from 2560x1440, and so on.
        scale: (info.width / 1280).min(info.height / 720).max(1),
    };
    x86_64::instructions::interrupts::without_interrupts(|| WRITER.lock().use_framebuffer(fb));
}

/// A program has the screen (KDSETMODE KD_GRAPHICS, an X server): the
/// console keeps its text but doesn't draw it until text mode comes back.
static GRAPHICS: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// The process that switched to graphics (text comes back when it exits).
static GRAPHICS_OWNER: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

pub fn is_graphics() -> bool {
    GRAPHICS.load(core::sync::atomic::Ordering::Relaxed)
}

/// KDSETMODE: graphics (a program draws) or text (the console redraws
/// everything it has).
pub fn set_graphics(on: bool, owner: u64) {
    use core::sync::atomic::Ordering;
    let was = GRAPHICS.swap(on, Ordering::SeqCst);
    GRAPHICS_OWNER.store(if on { owner } else { 0 }, Ordering::SeqCst);
    if was && !on {
        x86_64::instructions::interrupts::without_interrupts(|| {
            let mut w = WRITER.lock();
            if let Some(fb) = &w.fb {
                unsafe { core::ptr::write_bytes(fb.buf, 0, fb.len) };
            }
            for row in 0..w.height {
                for col in 0..w.width {
                    w.draw(row, col);
                }
            }
        });
    }
}

/// A process exited: if it had the screen in graphics mode, text is back
/// (an X server that crashed mustn't leave the console dark).
pub fn process_exited(pid: u64) {
    use core::sync::atomic::Ordering;
    if pid != 0 && GRAPHICS_OWNER.load(Ordering::SeqCst) == pid {
        set_graphics(false, 0);
        crate::tty::set_keyboard_mode(crate::tty::K_XLATE);
    }
}

/// The framebuffer, for /dev/fb0: (kernel address, length, width, height,
/// pixels per line, bytes per pixel, BGR?).
pub fn framebuffer() -> Option<(u64, usize, usize, usize, usize, usize, bool)> {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let w = WRITER.lock();
        w.fb.as_ref().map(|fb| (fb.buf as u64, fb.len, fb.width, fb.height, fb.stride, fb.bpp, fb.bgr))
    })
}

/// The console's size in cells: (rows, columns).
pub fn size() -> (usize, usize) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let w = WRITER.lock();
        (w.height, w.width)
    })
}

/// Where the cell grid is (for tests and QEMU's monitor): its address, the
/// cells per row in memory, and the size in use. Each cell is 2 bytes, as
/// in VGA text memory: the character, then the color.
pub fn grid_info() -> (u64, usize, usize, usize) {
    let (rows, cols) = size();
    (core::ptr::addr_of!(GRID) as u64, MAX_WIDTH, cols, rows)
}

impl Writer {
/// Hide the cursor (while output is written elsewhere).
pub fn erase_cursor_inner(&mut self) {
    self.hide_cursor();
}
}

pub fn clear_screen() {
    WRITER.lock().clear_screen_inner();
}

impl Writer {
    pub fn clear_screen_inner(&mut self) {
        for row in 0..self.height {
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
        self.row_position    = row.min(self.height - 1);
        self.column_position = col.min(self.width  - 1);
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
                        self.blank(0, self.width * self.height);
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
                    while self.column_position < next.min(self.width) {
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
        let total = self.width * self.height;
        let pos = self.row_position * self.width + self.column_position;
        let set_pos = |w: &mut Writer, p: usize| {
            let p = p.min(total - 1);
            w.row_position = p / w.width;
            w.column_position = p % w.width;
        };
        match cmd {
            // Moves wrap across lines, so line editors can move over wrapped input.
            b'A' => self.row_position = self.row_position.saturating_sub(arg(0, 1)),
            b'B' => self.row_position = (self.row_position + arg(0, 1)).min(self.height - 1),
            b'C' => set_pos(self, pos + arg(0, 1)),
            b'D' => set_pos(self, pos.saturating_sub(arg(0, 1))),
            b'H' | b'f' => {
                self.row_position = (arg(0, 1) - 1).min(self.height - 1);
                self.column_position = (arg(1, 1) - 1).min(self.width - 1);
            }
            // Absolute column / row, relative moves, next / previous line.
            b'G' | b'`' => self.column_position = (arg(0, 1) - 1).min(self.width - 1),
            b'd' => self.row_position = (arg(0, 1) - 1).min(self.height - 1),
            b'a' => self.column_position = (self.column_position + arg(0, 1)).min(self.width - 1),
            b'e' => self.row_position = (self.row_position + arg(0, 1)).min(self.height - 1),
            b'E' => {
                self.row_position = (self.row_position + arg(0, 1)).min(self.height - 1);
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
                let line_end = self.row_position * self.width + self.width;
                self.blank(pos, (pos + arg(0, 1)).min(line_end));
            }
            b'L' => self.insert_lines(arg(0, 1)),
            b'M' => self.delete_lines(arg(0, 1)),
            b'S' => self.scroll(arg(0, 1), true),
            b'T' => self.scroll(arg(0, 1), false),
            b'b' => {
                for _ in 0..arg(0, 1).min(self.width * self.height) {
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
                let line = self.row_position * self.width;
                let (from, to) = match params.first().copied().unwrap_or(0) {
                    0 => (pos, line + self.width),
                    1 => (line, pos + 1),
                    _ => (line, line + self.width),
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
        let (row, col) = (self.row_position, self.column_position.min(self.width - 1));
        for c in (col..self.width).rev() {
            let ch = if c >= col + n { self.get(row, c - n) } else { self.blank_cell() };
            self.put(row, c, ch);
        }
    }

    /// CSI P: delete `n` characters at the cursor; the rest moves left.
    fn delete_chars(&mut self, n: usize) {
        let (row, col) = (self.row_position, self.column_position.min(self.width - 1));
        for c in col..self.width {
            let ch = if c + n < self.width { self.get(row, c + n) } else { self.blank_cell() };
            self.put(row, c, ch);
        }
    }

    /// Move rows [from, end) down (`down`) or up by `n`, blanking what's left.
    /// (No allocation: the console also prints during boot and panics.)
    fn shift_rows(&mut self, from: usize, n: usize, down: bool) {
        for i in 0..self.height.saturating_sub(from) {
            let r = if down { self.height - 1 - i } else { from + i };
            let src = if down { r.checked_sub(n).filter(|&s| s >= from) } else { Some(r + n).filter(|&s| s < self.height) };
            for c in 0..self.width {
                let ch = match src {
                    Some(s) => self.get(s, c),
                    None => self.blank_cell(),
                };
                self.put(r, c, ch);
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
        for p in from..to.min(self.width * self.height) {
            self.put(p / self.width, p % self.width, blank);
        }
    }

    /// Restore the default colors (when a program leaves them changed).
    pub fn reset_ansi(&mut self) {
        self.reset_attributes();
        self.apply_attributes();
    }
}

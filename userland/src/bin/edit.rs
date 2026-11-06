// Copyright (c) 2026 tre4surehunter9
//! edit: a small full-screen text editor.
//!
//!   edit FILE
//!
//! Arrows, Home/End, PageUp/PageDown move; Backspace/Delete erase; Tab
//! inserts 4 spaces. Ctrl+S saves, Ctrl+Q quits (press it twice to quit
//! without saving changes).
#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use userlib::*;

userlib::entry!(main);

struct Editor {
    path: String,
    lines: Vec<String>,
    row: usize,
    col: usize,
    /// First line and column shown on screen.
    top: usize,
    left: usize,
    rows: usize,
    cols: usize,
    dirty: bool,
    message: String,
    quit_armed: bool,
}

impl Editor {
    fn text_rows(&self) -> usize {
        self.rows - 1 // last row is the status bar
    }

    fn line_len(&self) -> usize {
        self.lines[self.row].len()
    }

    fn clamp_col(&mut self) {
        self.col = self.col.min(self.line_len());
    }

    fn scroll(&mut self) {
        let h = self.text_rows();
        if self.row < self.top {
            self.top = self.row;
        }
        if self.row >= self.top + h {
            self.top = self.row + 1 - h;
        }
        if self.col < self.left {
            self.left = self.col;
        }
        if self.col >= self.left + self.cols {
            self.left = self.col + 1 - self.cols;
        }
    }

    fn render(&mut self) {
        self.scroll();
        let mut out = String::from("\x1b[H");
        for y in 0..self.text_rows() {
            out.push_str(&format!("\x1b[{};1H", y + 1));
            match self.lines.get(self.top + y) {
                Some(line) => {
                    let visible: String = line.chars().skip(self.left).take(self.cols).collect();
                    out.push_str(&visible);
                }
                None => out.push('~'),
            }
            out.push_str("\x1b[K");
        }
        // Status bar in reverse video.
        let left = format!(
            " {}{} | Ln {}/{} Col {}",
            self.path,
            if self.dirty { " [modified]" } else { "" },
            self.row + 1,
            self.lines.len(),
            self.col + 1
        );
        let right = if self.message.is_empty() { "^S save  ^Q quit ".to_string() } else { format!("{} ", self.message) };
        let mut bar = left;
        let pad = self.cols.saturating_sub(bar.len() + right.len());
        bar.extend(core::iter::repeat(' ').take(pad));
        bar.push_str(&right);
        bar.truncate(self.cols);
        out.push_str(&format!("\x1b[{};1H\x1b[7m{}\x1b[0m", self.rows, bar));
        out.push_str(&format!("\x1b[{};{}H", self.row - self.top + 1, self.col - self.left + 1));
        let _ = write_all(STDOUT, out.as_bytes());
    }

    fn insert(&mut self, c: char) {
        let col = self.col;
        self.lines[self.row].insert(col, c);
        self.col += 1;
        self.dirty = true;
    }

    fn newline(&mut self) {
        let rest = self.lines[self.row].split_off(self.col);
        self.lines.insert(self.row + 1, rest);
        self.row += 1;
        self.col = 0;
        self.dirty = true;
    }

    fn backspace(&mut self) {
        if self.col > 0 {
            self.col -= 1;
            let col = self.col;
            self.lines[self.row].remove(col);
            self.dirty = true;
        } else if self.row > 0 {
            let line = self.lines.remove(self.row);
            self.row -= 1;
            self.col = self.line_len();
            self.lines[self.row].push_str(&line);
            self.dirty = true;
        }
    }

    fn delete(&mut self) {
        if self.col < self.line_len() {
            let col = self.col;
            self.lines[self.row].remove(col);
            self.dirty = true;
        } else if self.row + 1 < self.lines.len() {
            let next = self.lines.remove(self.row + 1);
            self.lines[self.row].push_str(&next);
            self.dirty = true;
        }
    }

    fn save(&mut self) {
        let mut text = self.lines.join("\n");
        text.push('\n');
        let fd = open(&self.path, O_WRONLY | O_CREAT | O_TRUNC);
        if fd < 0 {
            self.message = format!("Can't save: {}", strerror(-fd));
            return;
        }
        let result = write_all(fd as i32, text.as_bytes());
        close(fd as i32);
        match result {
            Ok(()) => {
                self.dirty = false;
                self.message = format!("Saved {} bytes", text.len());
            }
            Err(e) => self.message = format!("Can't save: {}", strerror(-e)),
        }
    }

    /// Handle one key. Returns false when the editor should quit.
    fn key(&mut self, key: Key) -> bool {
        let was_armed = core::mem::take(&mut self.quit_armed);
        self.message.clear();
        match key {
            Key::Ctrl('q') => {
                if self.dirty && !was_armed {
                    self.quit_armed = true;
                    self.message = "Unsaved changes! ^Q again to quit".to_string();
                } else {
                    return false;
                }
            }
            Key::Ctrl('s') => self.save(),
            Key::Enter => self.newline(),
            Key::Backspace => self.backspace(),
            Key::Delete => self.delete(),
            Key::Tab => (0..4).for_each(|_| self.insert(' ')),
            Key::Char(c) if !c.is_control() => self.insert(c),
            Key::Left => {
                if self.col > 0 {
                    self.col -= 1;
                } else if self.row > 0 {
                    self.row -= 1;
                    self.col = self.line_len();
                }
            }
            Key::Right => {
                if self.col < self.line_len() {
                    self.col += 1;
                } else if self.row + 1 < self.lines.len() {
                    self.row += 1;
                    self.col = 0;
                }
            }
            Key::Up => {
                self.row = self.row.saturating_sub(1);
                self.clamp_col();
            }
            Key::Down => {
                self.row = (self.row + 1).min(self.lines.len() - 1);
                self.clamp_col();
            }
            Key::Home | Key::Ctrl('a') => self.col = 0,
            Key::End | Key::Ctrl('e') => self.col = self.line_len(),
            Key::PageUp => {
                self.row = self.row.saturating_sub(self.text_rows());
                self.clamp_col();
            }
            Key::PageDown => {
                self.row = (self.row + self.text_rows()).min(self.lines.len() - 1);
                self.clamp_col();
            }
            _ => {}
        }
        true
    }
}

fn load(path: &str) -> Result<Vec<String>, isize> {
    let fd = open(path, O_RDONLY);
    if fd == -ENOENT {
        return Ok(vec![String::new()]); // new file
    }
    if fd < 0 {
        return Err(fd);
    }
    let mut data = Vec::new();
    let mut buf = [0u8; 512];
    loop {
        let n = read(fd as i32, &mut buf);
        if n < 0 {
            close(fd as i32);
            return Err(n);
        }
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n as usize]);
    }
    close(fd as i32);
    let text = String::from_utf8_lossy(&data);
    // Keep lines ASCII: the cursor arithmetic uses byte offsets, and the
    // console can only show ASCII anyway.
    let mut lines: Vec<String> = text
        .split('\n')
        .map(|l| {
            l.replace('\t', "    ")
                .chars()
                .map(|c| if c.is_ascii() && (!c.is_ascii_control() || c == '\r') { c } else { '?' })
                .filter(|&c| c != '\r')
                .collect()
        })
        .collect();
    if lines.len() > 1 && lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop(); // the file's final newline
    }
    Ok(lines)
}

fn main(args: Args) -> i32 {
    let Some(path) = args.get(1) else {
        eprintln!("usage: edit FILE");
        return 2;
    };
    if stat(path).is_ok_and(|s| s.is_dir()) {
        eprintln!("edit: {}: Is a directory", path);
        return 1;
    }
    let lines = match load(path) {
        Ok(lines) => lines,
        Err(e) => {
            eprintln!("edit: {}: {}", path, strerror(-e));
            return 1;
        }
    };
    let saved = match enable_raw_mode(STDIN) {
        Ok(t) => t,
        Err(_) => {
            eprintln!("edit: standard input is not a terminal");
            return 1;
        }
    };

    let (rows, cols) = window_size(STDOUT);
    let mut ed = Editor {
        path: path.to_string(),
        lines,
        row: 0,
        col: 0,
        top: 0,
        left: 0,
        rows: rows.max(2) as usize,
        cols: cols.max(10) as usize,
        dirty: false,
        message: String::new(),
        quit_armed: false,
    };
    let _ = write_all(STDOUT, b"\x1b[2J");
    let mut keys = KeyReader::new(STDIN);
    loop {
        ed.render();
        match keys.next_key() {
            Some(key) => {
                if !ed.key(key) {
                    break;
                }
            }
            None => break,
        }
    }

    tcsetattr(STDIN, &saved);
    let _ = write_all(STDOUT, b"\x1b[0m\x1b[H\x1b[2J");
    0
}

// Copyright (c) 2026 tre4surehunter9
use core::sync::atomic::{AtomicBool, Ordering};
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use crate::println;
use crate::print;
use crate::vga_buffer::WRITER;
use alloc::vec;

pub static EDITOR_ACTIVE: AtomicBool = AtomicBool::new(false);

const EDIT_ROWS: usize = 24;

pub struct Editor {
    pub lines:         Vec<String>,
    pub cursor_col:    usize,
    pub cursor_row:    usize,
    pub scroll_offset: usize,
    pub filepath:      String,
    pub dirty:         bool,
}

impl Editor {
    pub fn new(filepath: &str, content: &str) -> Self {
        let lines: Vec<String> = if content.is_empty() {
            vec!["".to_string()]
        } else {
            content.lines().map(|l| l.to_string()).collect()
        };

        Editor {
            lines,
            cursor_col:    0,
            cursor_row:    0,
            scroll_offset: 0,
            filepath:      filepath.to_string(),
            dirty:         false,
        }
    }

    pub fn insert_char(&mut self, c: char) {
        let row = self.cursor_row;
        let col = self.cursor_col;
        self.lines[row].insert(col, c);
        self.cursor_col += 1;
        self.dirty = true;
    }

    pub fn backspace(&mut self) {
        let row = self.cursor_row;
        let col = self.cursor_col;

        if col > 0 {
            self.lines[row].remove(col - 1);
            self.cursor_col -= 1;
        } else if row > 0 {
            let current_line = self.lines.remove(row);
            let prev_len = self.lines[row - 1].len();
            self.lines[row - 1].push_str(&current_line);
            self.cursor_row -= 1;
            self.cursor_col = prev_len;
            if self.cursor_row < self.scroll_offset {
                self.scroll_offset = self.cursor_row;
            }
        }
        self.dirty = true;
    }

    pub fn newline(&mut self) {
        let row = self.cursor_row;
        let col = self.cursor_col;
        let rest = self.lines[row].split_off(col);
        self.lines.insert(row + 1, rest);
        self.cursor_row += 1;
        self.cursor_col = 0;
        if self.cursor_row >= self.scroll_offset + EDIT_ROWS {
            self.scroll_offset += 1;
        }
        self.dirty = true;
    }

    pub fn move_up(&mut self) {
        if self.cursor_row > 0 {
            self.cursor_row -= 1;
            let line_len = self.lines[self.cursor_row].len();
            if self.cursor_col > line_len { self.cursor_col = line_len; }
            if self.cursor_row < self.scroll_offset {
                self.scroll_offset = self.cursor_row;
            }
        }
    }

    pub fn move_down(&mut self) {
        if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            let line_len = self.lines[self.cursor_row].len();
            if self.cursor_col > line_len { self.cursor_col = line_len; }
            if self.cursor_row >= self.scroll_offset + EDIT_ROWS {
                self.scroll_offset += 1;
            }
        }
    }

    pub fn move_left(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
        } else if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].len();
            if self.cursor_row < self.scroll_offset {
                self.scroll_offset = self.cursor_row;
            }
        }
    }

    pub fn move_right(&mut self) {
        let line_len = self.lines[self.cursor_row].len();
        if self.cursor_col < line_len {
            self.cursor_col += 1;
        } else if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = 0;
            if self.cursor_row >= self.scroll_offset + EDIT_ROWS {
                self.scroll_offset += 1;
            }
        }
    }

    pub fn render(&self) {
        crate::vga_buffer::clear_screen();

        for screen_row in 0..EDIT_ROWS {
            let file_row = self.scroll_offset + screen_row;
            if file_row < self.lines.len() {
                let line = &self.lines[file_row];
                let display = if line.len() > 80 { &line[..80] } else { line.as_str() };
                println!("{}", display);
            } else {
                println!("~");
            }
        }

        // Status bar
        let dirty_marker = if self.dirty { "*" } else { " " };
        let status = alloc::format!(
            " {}{} | Ln {}/{} Col {} | Ctrl+S save  Ctrl+Q quit",
            dirty_marker,
            self.filepath,
            self.cursor_row + 1,
            self.lines.len(),
            self.cursor_col + 1,
        );

        WRITER.lock().set_color(
            crate::vga_buffer::Color::Black,
            crate::vga_buffer::Color::LightGray,
        );
        let padded = alloc::format!("{:<80}", &status[..80.min(status.len())]);
        print!("{}", &padded[..80]);
        WRITER.lock().set_color(
            crate::vga_buffer::Color::Yellow,
            crate::vga_buffer::Color::Black,
        );

        let screen_row = self.cursor_row - self.scroll_offset;
        WRITER.lock().set_cursor_pos(screen_row, self.cursor_col);
        crate::vga_buffer::draw_cursor();
    }

    pub fn serialize(&self) -> String {
        self.lines.join("\n")
    }
}

use spin::Mutex;
use lazy_static::lazy_static;

lazy_static! {
    pub static ref EDITOR: Mutex<Option<Editor>> = Mutex::new(None);
}

pub fn open(filepath: &str) {
    let content = crate::filesystem::read_file(filepath).unwrap_or_default();
    let editor = Editor::new(filepath, &content);
    *EDITOR.lock() = Some(editor);
    EDITOR_ACTIVE.store(true, Ordering::SeqCst);
    if let Some(ed) = EDITOR.lock().as_ref() {
        ed.render();
    }
}

pub fn save() {
    let content;
    let path;
    {
        let guard = EDITOR.lock();
        if let Some(ed) = guard.as_ref() {
            content = ed.serialize();
            path = ed.filepath.clone();
        } else { return; }
    }
    if let Err(e) = crate::filesystem::write_file(&path, &content) {
        crate::println!("Save failed: {}", e);
        return;
    }
    if let Some(ed) = EDITOR.lock().as_mut() {
        ed.dirty = false;
    }
}

pub fn close() {
    *EDITOR.lock() = None;
    EDITOR_ACTIVE.store(false, Ordering::SeqCst);
    crate::vga_buffer::clear_screen();
    let cwd = crate::filesystem::CWD.lock().clone();
    crate::print!("PalladiumOS:{} > ", cwd); // same prompt as the kernel shell
    crate::vga_buffer::draw_cursor();
}

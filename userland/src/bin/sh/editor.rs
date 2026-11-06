// Copyright (c) 2026 tre4surehunter9
//! Interactive line editing on the console (raw mode): cursor movement,
//! history, and Tab completion. Commands still run in cooked mode.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use userlib::*;

use crate::builtins::BUILTINS;
use crate::read_line;

pub struct LineEditor {
    history: Vec<String>,
}

const HISTORY_MAX: usize = 100;

impl LineEditor {
    pub fn new() -> Self {
        LineEditor { history: Vec::new() }
    }

    /// Show `prompt` and read a line. None at Ctrl-D on an empty line.
    pub fn read_line(&mut self, prompt: &str) -> Option<String> {
        let Ok(saved) = enable_raw_mode(STDIN) else {
            print!("{}", prompt);
            return read_line();
        };
        let result = self.edit(prompt);
        tcsetattr(STDIN, &saved);
        if let Some(line) = &result {
            if !line.trim().is_empty() && self.history.last() != Some(line) {
                self.history.push(line.clone());
                if self.history.len() > HISTORY_MAX {
                    self.history.remove(0);
                }
            }
        }
        result
    }

    fn edit(&mut self, prompt: &str) -> Option<String> {
        let mut keys = KeyReader::new(STDIN);
        let mut buf: Vec<char> = Vec::new();
        let mut cur = 0usize;
        // Index into history while browsing it; history.len() = the new line.
        let mut hist = self.history.len();
        let mut draft: Vec<char> = Vec::new();
        print!("{}", prompt);

        loop {
            let (old_cur, old_len) = (cur, buf.len());
            let Some(key) = keys.next_key() else { return None };
            match key {
                Key::Enter => {
                    move_right(buf.len() - cur);
                    print!("\n");
                    return Some(buf.iter().collect());
                }
                Key::Ctrl('d') if buf.is_empty() => return None,
                Key::Ctrl('d') | Key::Delete => {
                    if cur < buf.len() {
                        buf.remove(cur);
                    }
                }
                Key::Ctrl('c') => {
                    move_right(buf.len() - cur);
                    print!("^C\n{}", prompt);
                    buf.clear();
                    cur = 0;
                    hist = self.history.len();
                    continue;
                }
                Key::Ctrl('l') => {
                    print!("\x1b[H\x1b[2J{}", prompt);
                    redraw(0, 0, &buf, cur);
                    continue;
                }
                Key::Backspace => {
                    if cur > 0 {
                        cur -= 1;
                        buf.remove(cur);
                    }
                }
                Key::Left => cur = cur.saturating_sub(1),
                Key::Right => cur = (cur + 1).min(buf.len()),
                Key::Home | Key::Ctrl('a') => cur = 0,
                Key::End | Key::Ctrl('e') => cur = buf.len(),
                Key::Ctrl('u') => {
                    buf.drain(..cur);
                    cur = 0;
                }
                Key::Ctrl('k') => buf.truncate(cur),
                Key::Up => {
                    if hist > 0 {
                        if hist == self.history.len() {
                            draft = buf.clone();
                        }
                        hist -= 1;
                        buf = self.history[hist].chars().collect();
                        cur = buf.len();
                    }
                }
                Key::Down => {
                    if hist < self.history.len() {
                        hist += 1;
                        buf = if hist == self.history.len() {
                            draft.clone()
                        } else {
                            self.history[hist].chars().collect()
                        };
                        cur = buf.len();
                    }
                }
                Key::Tab => {
                    if let Some(listing) = complete(&mut buf, &mut cur) {
                        // Ambiguous: list the choices, then redraw the line.
                        move_right(old_len - old_cur);
                        print!("\n{}\n{}", listing, prompt);
                        redraw(0, 0, &buf, cur);
                        continue;
                    }
                }
                Key::Char(c) if !c.is_control() => {
                    if buf.len() < 1000 {
                        buf.insert(cur, c);
                        cur += 1;
                    }
                }
                _ => continue,
            }
            redraw(old_cur, old_len, &buf, cur);
        }
    }
}

fn move_left(n: usize) {
    if n > 0 {
        print!("\x1b[{}D", n);
    }
}

fn move_right(n: usize) {
    if n > 0 {
        print!("\x1b[{}C", n);
    }
}

/// Repaint the input after an edit. The cursor was `old_cur` characters into
/// an `old_len`-character line; afterwards it's `cur` characters into `buf`.
/// Moves are relative, so this works when the line wraps.
fn redraw(old_cur: usize, old_len: usize, buf: &[char], cur: usize) {
    let mut out = String::new();
    if old_cur > 0 {
        out.push_str(&format!("\x1b[{}D", old_cur));
    }
    out.extend(buf.iter());
    let extra = old_len.saturating_sub(buf.len());
    out.extend(core::iter::repeat(' ').take(extra));
    let back = buf.len() + extra - cur;
    if back > 0 {
        out.push_str(&format!("\x1b[{}D", back));
    }
    print!("{}", out);
}

/// Tab completion of the word before the cursor: a command name (builtins
/// and programs in /bin) for the first word, otherwise a file name.
/// Completes as far as is unambiguous; if that adds nothing and there are
/// several choices, returns them for listing.
fn complete(buf: &mut Vec<char>, cur: &mut usize) -> Option<String> {
    let start = buf[..*cur].iter().rposition(|c| *c == ' ').map(|i| i + 1).unwrap_or(0);
    let word: String = buf[start..*cur].iter().collect();
    let first_word = buf[..start].iter().all(|c| *c == ' ');

    // (candidate name, is a directory)
    let mut candidates: Vec<(String, bool)> = Vec::new();
    let (dir, prefix) = match word.rfind('/') {
        Some(i) => (word[..=i].to_string(), word[i + 1..].to_string()),
        None => (String::new(), word.clone()),
    };
    if first_word && dir.is_empty() {
        for b in BUILTINS {
            if b.starts_with(prefix.as_str()) {
                candidates.push((b.to_string(), false));
            }
        }
        list_dir("/bin", &prefix, &mut candidates);
        candidates.retain(|(n, _)| !n.starts_with("test_"));
    } else {
        let d = if dir.is_empty() { ".".to_string() } else { dir.clone() };
        list_dir(&d, &prefix, &mut candidates);
    }
    candidates.sort();
    candidates.dedup();
    if candidates.is_empty() {
        return None;
    }

    // Longest common prefix of all candidates.
    let names: Vec<&str> = candidates.iter().map(|(n, _)| n.as_str()).collect();
    let mut common = names[0].to_string();
    for n in &names[1..] {
        while !n.starts_with(common.as_str()) {
            common.pop();
        }
    }
    let mut insert: String = common[prefix.len()..].to_string();
    if candidates.len() == 1 {
        insert.push(if candidates[0].1 { '/' } else { ' ' });
    }
    if insert.is_empty() {
        if candidates.len() > 1 {
            let listing: Vec<String> =
                candidates.iter().map(|(n, d)| if *d { format!("{}/", n) } else { n.clone() }).collect();
            return Some(listing.join("  "));
        }
        return None;
    }
    for c in insert.chars() {
        buf.insert(*cur, c);
        *cur += 1;
    }
    None
}

fn list_dir(dir: &str, prefix: &str, out: &mut Vec<(String, bool)>) {
    let fd = open(dir, O_RDONLY | O_DIRECTORY);
    if fd < 0 {
        return;
    }
    let _ = for_each_entry(fd as i32, |e| {
        if e.name != "." && e.name != ".." && e.name.starts_with(prefix) && (!e.name.starts_with('.') || prefix.starts_with('.')) {
            out.push((e.name.to_string(), e.d_type == DT_DIR));
        }
    });
    close(fd as i32);
}

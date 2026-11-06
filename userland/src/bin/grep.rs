// Copyright (c) 2026 tre4surehunter9
//! grep: print lines matching a pattern.
//!
//!   grep [-i] [-v] [-n] [-c] [-q] [-F] PATTERN [FILE...]
//!
//! Patterns are basic regular expressions: `.` any character, `x*` zero or
//! more x, `^` start, `$` end, `\` escapes the next character. -F matches
//! a fixed string. Exit status: 0 if a line matched, 1 if none, 2 on error.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::vec::Vec;
use userlib::io::{for_each_input, Lines, Out};
use userlib::{eprintln, Args};

userlib::entry!(main);

#[derive(Clone, Copy, PartialEq)]
enum Atom {
    Any,
    Byte(u8),
}

#[derive(Clone, Copy)]
struct Piece {
    atom: Atom,
    star: bool,
}

struct Pattern {
    anchored_start: bool,
    anchored_end: bool,
    pieces: Vec<Piece>,
    ignore_case: bool,
}

impl Pattern {
    fn new(pat: &str, fixed: bool, ignore_case: bool) -> Pattern {
        let bytes = pat.as_bytes();
        let mut pieces = Vec::new();
        let (mut start, mut end) = (false, false);
        let mut i = 0;
        if !fixed && bytes.first() == Some(&b'^') {
            start = true;
            i = 1;
        }
        let mut stop = bytes.len();
        if !fixed && stop > i && bytes[stop - 1] == b'$' && !(stop >= 2 && bytes[stop - 2] == b'\\') {
            end = true;
            stop -= 1;
        }
        while i < stop {
            let atom = match bytes[i] {
                b'.' if !fixed => Atom::Any,
                b'\\' if !fixed && i + 1 < stop => {
                    i += 1;
                    Atom::Byte(bytes[i])
                }
                b => Atom::Byte(b),
            };
            i += 1;
            let star = !fixed && i < stop && bytes[i] == b'*';
            if star {
                i += 1;
            }
            pieces.push(Piece { atom, star });
        }
        Pattern { anchored_start: start, anchored_end: end, pieces, ignore_case }
    }

    fn atom_matches(&self, atom: Atom, b: u8) -> bool {
        match atom {
            Atom::Any => true,
            Atom::Byte(c) if self.ignore_case => c.eq_ignore_ascii_case(&b),
            Atom::Byte(c) => c == b,
        }
    }

    /// Does pieces[p..] match at the start of text?
    fn match_here(&self, p: usize, text: &[u8]) -> bool {
        if p == self.pieces.len() {
            return !self.anchored_end || text.is_empty();
        }
        let piece = self.pieces[p];
        if piece.star {
            // Try the longest run first, then shorter ones.
            let mut n = 0;
            while n < text.len() && self.atom_matches(piece.atom, text[n]) {
                n += 1;
            }
            loop {
                if self.match_here(p + 1, &text[n..]) {
                    return true;
                }
                if n == 0 {
                    return false;
                }
                n -= 1;
            }
        }
        !text.is_empty() && self.atom_matches(piece.atom, text[0]) && self.match_here(p + 1, &text[1..])
    }

    fn matches(&self, line: &[u8]) -> bool {
        if self.anchored_start {
            return self.match_here(0, line);
        }
        (0..=line.len()).any(|i| self.match_here(0, &line[i..]))
    }
}

fn main(args: Args) -> i32 {
    let args: Vec<&str> = args.skip(1).collect();
    let (mut invert, mut number, mut count, mut quiet, mut ignore_case, mut fixed) = (false, false, false, false, false, false);
    let mut rest = Vec::new();
    let mut options_done = false;
    for a in &args {
        if !options_done && a.len() > 1 && a.starts_with('-') {
            if *a == "--" {
                options_done = true;
                continue;
            }
            for c in a[1..].chars() {
                match c {
                    'v' => invert = true,
                    'n' => number = true,
                    'c' => count = true,
                    'q' => quiet = true,
                    'i' => ignore_case = true,
                    'F' => fixed = true,
                    _ => {
                        eprintln!("grep: invalid option -- '{}'", c);
                        return 2;
                    }
                }
            }
        } else {
            options_done = true;
            rest.push(*a);
        }
    }
    let Some((pattern, files)) = rest.split_first() else {
        eprintln!("usage: grep [-ivncqF] PATTERN [FILE...]");
        return 2;
    };
    let pat = Pattern::new(pattern, fixed, ignore_case);
    let show_names = files.len() > 1;
    let mut out = Out::new();
    let mut any = false;

    let status = for_each_input("grep", files, |fd, name| {
        let mut lines = Lines::new(fd);
        let mut n = 0usize;
        let mut matched = 0usize;
        while let Some(Ok(line)) = lines.next_line() {
            n += 1;
            let text = line.strip_suffix(b"\n").unwrap_or(&line);
            if pat.matches(text) == invert {
                continue;
            }
            matched += 1;
            any = true;
            if quiet || count {
                continue;
            }
            if show_names {
                out.write(format!("{}:", name).as_bytes());
            }
            if number {
                out.write(format!("{}:", n).as_bytes());
            }
            out.write(text);
            out.write(b"\n");
            if out.failed {
                return; // reader is gone
            }
        }
        if count && !quiet {
            if show_names {
                out.write(format!("{}:", name).as_bytes());
            }
            out.write(format!("{}\n", matched).as_bytes());
        }
    });
    drop(out);
    if status != 0 { 2 } else if any { 0 } else { 1 }
}

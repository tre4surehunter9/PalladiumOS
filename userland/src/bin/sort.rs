// Copyright (c) 2026 tre4surehunter9
//! sort: sort lines.  sort [-r] [-n] [-u] [FILE...]
//!   -r reverse, -n numeric (by leading number), -u drop duplicates
#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use core::cmp::Ordering;
use userlib::io::{for_each_input, Lines, Out};
use userlib::{eprintln, Args};

userlib::entry!(main);

/// The number at the start of a line (after spaces), for -n. Lines without
/// one sort as 0, like GNU sort.
fn leading_number(line: &[u8]) -> f64 {
    let s = core::str::from_utf8(line).unwrap_or("").trim_start();
    let end = s
        .char_indices()
        .take_while(|&(i, c)| c.is_ascii_digit() || c == '.' || (i == 0 && c == '-'))
        .map(|(i, c)| i + c.len_utf8())
        .last()
        .unwrap_or(0);
    s[..end].parse().unwrap_or(0.0)
}

fn main(args: Args) -> i32 {
    let (mut reverse, mut numeric, mut unique) = (false, false, false);
    let mut files = Vec::new();
    for a in args.skip(1) {
        if a.len() > 1 && a.starts_with('-') {
            for c in a[1..].chars() {
                match c {
                    'r' => reverse = true,
                    'n' => numeric = true,
                    'u' => unique = true,
                    _ => {
                        eprintln!("sort: invalid option -- '{}'", c);
                        return 2;
                    }
                }
            }
        } else {
            files.push(a);
        }
    }

    let mut all: Vec<Vec<u8>> = Vec::new();
    let status = for_each_input("sort", &files, |fd, _| {
        let mut lines = Lines::new(fd);
        while let Some(Ok(mut line)) = lines.next_line() {
            if line.last() == Some(&b'\n') {
                line.pop();
            }
            all.push(line);
        }
    });

    let cmp = |a: &Vec<u8>, b: &Vec<u8>| -> Ordering {
        if numeric {
            leading_number(a).partial_cmp(&leading_number(b)).unwrap_or(Ordering::Equal).then_with(|| a.cmp(b))
        } else {
            a.cmp(b)
        }
    };
    all.sort_by(cmp);
    if reverse {
        all.reverse();
    }
    if unique {
        all.dedup_by(|a, b| cmp(a, b) == Ordering::Equal);
    }
    let mut out = Out::new();
    for line in &all {
        out.write(line);
        out.write(b"\n");
    }
    status
}

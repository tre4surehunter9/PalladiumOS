// Copyright (c) 2026 tre4surehunter9
//! uniq: collapse adjacent duplicate lines.  uniq [-c] [-d] [FILE]
//!   -c prefix lines with their count, -d only print duplicated lines
#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::vec::Vec;
use userlib::io::{for_each_input, Lines, Out};
use userlib::{eprintln, Args};

userlib::entry!(main);

fn main(args: Args) -> i32 {
    let (mut count, mut dups) = (false, false);
    let mut files = Vec::new();
    for a in args.skip(1) {
        match a {
            "-c" => count = true,
            "-d" => dups = true,
            a if a.len() > 1 && a.starts_with('-') => {
                eprintln!("uniq: invalid option '{}'", a);
                return 1;
            }
            a => files.push(a),
        }
    }
    let mut out = Out::new();
    let emit = |line: &[u8], n: usize, out: &mut Out| {
        if dups && n < 2 {
            return;
        }
        if count {
            out.write(format!("{:7} ", n).as_bytes());
        }
        out.write(line);
        out.write(b"\n");
    };
    for_each_input("uniq", &files[..files.len().min(1)], |fd, _| {
        let mut lines = Lines::new(fd);
        let mut prev: Option<(Vec<u8>, usize)> = None;
        while let Some(Ok(mut line)) = lines.next_line() {
            if line.last() == Some(&b'\n') {
                line.pop();
            }
            match &mut prev {
                Some((p, n)) if *p == line => *n += 1,
                _ => {
                    if let Some((p, n)) = prev.take() {
                        emit(&p, n, &mut out);
                    }
                    prev = Some((line, 1));
                }
            }
        }
        if let Some((p, n)) = prev {
            emit(&p, n, &mut out);
        }
    })
}

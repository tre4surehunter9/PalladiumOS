// Copyright (c) 2026 tre4surehunter9
//! wc: count lines, words and bytes.  wc [-l] [-w] [-c] [FILE...]
#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use userlib::io::{for_each_input, Out};
use userlib::{eprintln, read, Args};

userlib::entry!(main);

fn main(args: Args) -> i32 {
    let mut show = [false; 3]; // lines, words, bytes
    let mut files = Vec::new();
    for a in args.skip(1) {
        if a.len() > 1 && a.starts_with('-') {
            for c in a[1..].chars() {
                match c {
                    'l' => show[0] = true,
                    'w' => show[1] = true,
                    'c' => show[2] = true,
                    _ => {
                        eprintln!("wc: invalid option -- '{}'", c);
                        return 1;
                    }
                }
            }
        } else {
            files.push(a);
        }
    }
    if !show.contains(&true) {
        show = [true; 3];
    }

    let mut out = Out::new();
    let mut total = [0usize; 3];
    let format_counts = |counts: &[usize; 3], name: &str| {
        let mut line = String::new();
        for i in 0..3 {
            if show[i] {
                line.push_str(&format!("{:7} ", counts[i]));
            }
        }
        line.push_str(name);
        line.push('\n');
        line
    };
    let status = for_each_input("wc", &files, |fd, name| {
        let mut counts = [0usize; 3];
        let mut in_word = false;
        let mut buf = [0u8; 1024];
        loop {
            let n = read(fd, &mut buf);
            if n <= 0 {
                break;
            }
            for &b in &buf[..n as usize] {
                counts[2] += 1;
                if b == b'\n' {
                    counts[0] += 1;
                }
                let space = b.is_ascii_whitespace();
                if !space && !in_word {
                    counts[1] += 1;
                }
                in_word = !space;
            }
        }
        for i in 0..3 {
            total[i] += counts[i];
        }
        let label = if files.is_empty() { "" } else { name };
        let line = format_counts(&counts, label);
        // With no file name, drop the space left before it.
        let line = if label.is_empty() { format!("{}\n", line.trim_end()) } else { line };
        out.write(line.as_bytes());
    });
    if files.len() > 1 {
        out.write(format_counts(&total, "total").as_bytes());
    }
    status
}

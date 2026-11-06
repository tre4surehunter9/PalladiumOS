// Copyright (c) 2026 tre4surehunter9
//! dmesg: show the kernel's boot log (from /proc/kmsg).
//!   dmesg      with colors on a terminal
//!   dmesg -n   plain text (no color codes)
#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use userlib::*;

userlib::entry!(main);

/// Remove ANSI escape sequences ("\x1b[...m").
fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // skip to the end of the sequence (a letter)
            for d in chars.by_ref() {
                if d.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn main(args: Args) -> i32 {
    let Some(log) = userlib::io::read_text("/proc/kmsg") else {
        eprintln!("dmesg: can't read /proc/kmsg");
        return 1;
    };
    let colors = args.get(1) != Some("-n") && isatty(STDOUT);
    if colors {
        print!("{}", log);
    } else {
        print!("{}", plain(&log));
    }
    0
}

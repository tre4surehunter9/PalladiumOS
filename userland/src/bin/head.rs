// Copyright (c) 2026 tre4surehunter9
//! head: print the first lines of files.  head [-n N | -N] [FILE...]
#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::vec::Vec;
use userlib::io::{count_option, for_each_input, Lines, Out};
use userlib::{eprintln, Args};

userlib::entry!(main);

fn main(args: Args) -> i32 {
    let args: Vec<&str> = args.skip(1).collect();
    let (count, files) = match count_option(&args, 10) {
        Ok(v) => v,
        Err(bad) => {
            eprintln!("head: invalid number of lines: '{}'", bad);
            return 1;
        }
    };
    let mut out = Out::new();
    let headers = files.len() > 1;
    let mut first = true;
    for_each_input("head", &files, |fd, name| {
        if headers {
            out.write(format!("{}==> {} <==\n", if first { "" } else { "\n" }, name).as_bytes());
        }
        first = false;
        // Stop reading after `count` lines (so `yes | head` ends).
        let mut lines = Lines::new(fd);
        for _ in 0..count {
            match lines.next_line() {
                Some(Ok(line)) => out.write(&line),
                _ => break,
            }
        }
    })
}

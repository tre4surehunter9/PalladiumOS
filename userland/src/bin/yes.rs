// Copyright (c) 2026 tre4surehunter9
//! yes: print a line ("y" or the arguments) forever, until the reader goes
//! away (then SIGPIPE ends it).  yes [STRING...]
#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use userlib::{write_all, Args, STDOUT};

userlib::entry!(main);

fn main(args: Args) -> i32 {
    let words: Vec<&str> = args.skip(1).collect();
    let mut line = if words.is_empty() { String::from("y") } else { words.join(" ") };
    line.push('\n');
    // Write many lines at a time.
    let mut chunk = String::new();
    while chunk.len() < 2048 {
        chunk.push_str(&line);
    }
    loop {
        if write_all(STDOUT, chunk.as_bytes()).is_err() {
            return 1;
        }
    }
}

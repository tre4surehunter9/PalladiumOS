// Copyright (c) 2026 tre4surehunter9
//! mount: show what's mounted where (from /proc/mounts).
#![no_std]
#![no_main]

use userlib::io::read_text;
use userlib::{eprintln, println};

userlib::entry!(main);

fn main(args: userlib::Args) -> i32 {
    if args.count() > 1 {
        eprintln!("mount: mounting isn't supported yet (only listing)");
        return 1;
    }
    let Some(text) = read_text("/proc/mounts") else {
        eprintln!("mount: can't read /proc/mounts");
        return 1;
    };
    for line in text.lines() {
        let f: alloc::vec::Vec<&str> = line.split_whitespace().collect();
        if f.len() >= 4 {
            println!("{} on {} type {} ({})", f[0], f[1], f[2], f[3]);
        }
    }
    0
}

extern crate alloc;

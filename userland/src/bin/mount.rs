// Copyright (c) 2026 tre4surehunter9
//! mount: show what's mounted where (from /proc/mounts), or mount a
//! filesystem the kernel can make: mount -t proc|devtmpfs|tmpfs SOURCE DIR
//! (root only; disks are mounted by the kernel at boot).
#![no_std]
#![no_main]

use userlib::io::read_text;
use userlib::{eprintln, println};

userlib::entry!(main);

fn main(args: userlib::Args) -> i32 {
    let words: alloc::vec::Vec<&str> = (1..args.count()).filter_map(|i| args.get(i)).collect();
    if !words.is_empty() {
        let (fstype, dir) = match words.as_slice() {
            ["-t", t, _source, dir] => (*t, *dir),
            ["-t", t, dir] => (*t, *dir),
            _ => {
                eprintln!("usage: mount [-t proc|devtmpfs|tmpfs SOURCE DIR]");
                return 1;
            }
        };
        let r = userlib::fs::mount(fstype, dir);
        if r < 0 {
            eprintln!("mount: {}: {}", dir, userlib::fs::strerror(r));
            return 1;
        }
        return 0;
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

// Copyright (c) 2026 tre4surehunter9
//! rmdir: remove empty directories.
#![no_std]
#![no_main]

use userlib::{eprintln, rmdir, strerror};

userlib::entry!(main);

fn main(args: userlib::Args) -> i32 {
    if args.count() < 2 {
        eprintln!("rmdir: missing operand");
        return 1;
    }
    let mut status = 0;
    for path in args.skip(1) {
        let r = rmdir(path);
        if r < 0 {
            eprintln!("rmdir: failed to remove '{}': {}", path, strerror(-r));
            status = 1;
        }
    }
    status
}

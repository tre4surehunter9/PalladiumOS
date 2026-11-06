// Copyright (c) 2026 tre4surehunter9
//! readlink: print where symbolic links point.  readlink LINK...
#![no_std]
#![no_main]

use userlib::*;

userlib::entry!(main);

fn main(args: Args) -> i32 {
    let mut status = 0;
    for path in args.skip(1) {
        let mut buf = [0u8; 4096];
        match readlink(path, &mut buf) {
            Ok(target) => println!("{}", target),
            Err(_) => status = 1, // like GNU readlink: quiet for non-links
        }
    }
    status
}

// Copyright (c) 2026 tre4surehunter9
//! touch: create empty files (existing files are left unchanged).
#![no_std]
#![no_main]

use userlib::{close, eprintln, open, strerror, O_CREAT, O_WRONLY};

userlib::entry!(main);

fn main(args: userlib::Args) -> i32 {
    if args.count() < 2 {
        eprintln!("touch: missing file operand");
        return 1;
    }
    let mut status = 0;
    for path in args.skip(1) {
        let fd = open(path, O_WRONLY | O_CREAT);
        if fd < 0 {
            eprintln!("touch: cannot touch '{}': {}", path, strerror(-fd));
            status = 1;
        } else {
            close(fd as i32);
        }
    }
    status
}

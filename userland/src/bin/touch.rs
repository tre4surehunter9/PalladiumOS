// Copyright (c) 2026 tre4surehunter9
//! touch: create empty files, or set existing files' modification time to now.
#![no_std]
#![no_main]

use userlib::{close, eprintln, open, stat, strerror, touch_now, O_CREAT, O_WRONLY};

userlib::entry!(main);

fn main(args: userlib::Args) -> i32 {
    if args.count() < 2 {
        eprintln!("touch: missing file operand");
        return 1;
    }
    let mut status = 0;
    for path in args.skip(1) {
        // Existing files just get a new modification time.
        if stat(path).is_ok() {
            let r = touch_now(path);
            if r < 0 {
                eprintln!("touch: cannot touch '{}': {}", path, strerror(-r));
                status = 1;
            }
            continue;
        }
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

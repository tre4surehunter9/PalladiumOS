// Copyright (c) 2026 tre4surehunter9
//! cat: concatenate files to standard output.
//! With no arguments, or "-", reads standard input until end-of-file (Ctrl-D).
#![no_std]
#![no_main]

use userlib::{close, eprintln, open, read, strerror, write_all, O_RDONLY, STDIN, STDOUT};

userlib::entry!(main);

fn copy(fd: i32) -> Result<(), isize> {
    let mut buf = [0u8; 512];
    loop {
        let n = read(fd, &mut buf);
        if n < 0 {
            return Err(n);
        }
        if n == 0 {
            return Ok(());
        }
        write_all(STDOUT, &buf[..n as usize])?;
    }
}

fn main(args: userlib::Args) -> i32 {
    if args.count() < 2 {
        return match copy(STDIN) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("cat: {}", strerror(-e));
                1
            }
        };
    }

    let mut status = 0;
    for path in args.skip(1) {
        let fd = if path == "-" { STDIN as isize } else { open(path, O_RDONLY) };
        if fd < 0 {
            eprintln!("cat: {}: {}", path, strerror(-fd));
            status = 1;
            continue;
        }
        if let Err(e) = copy(fd as i32) {
            eprintln!("cat: {}: {}", path, strerror(-e));
            status = 1;
        }
        if fd as i32 != STDIN {
            close(fd as i32);
        }
    }
    status
}

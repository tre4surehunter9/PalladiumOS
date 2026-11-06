// Copyright (c) 2026 tre4surehunter9
//! tee: copy standard input to standard output and to files.
//!   tee [-a] [FILE...]   (-a appends instead of overwriting)
#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use userlib::*;

userlib::entry!(main);

fn main(args: Args) -> i32 {
    let mut append = false;
    let mut fds: Vec<i32> = Vec::new();
    let mut status = 0;
    for a in args.skip(1) {
        if a == "-a" {
            append = true;
            continue;
        }
        let mode = if append { O_APPEND } else { O_TRUNC };
        let fd = open(a, O_WRONLY | O_CREAT | mode);
        if fd < 0 {
            eprintln!("tee: {}: {}", a, strerror(-fd));
            status = 1;
        } else {
            fds.push(fd as i32);
        }
    }
    let mut buf = [0u8; 1024];
    loop {
        let n = read(STDIN, &mut buf);
        if n <= 0 {
            break;
        }
        let data = &buf[..n as usize];
        if write_all(STDOUT, data).is_err() {
            status = 1;
        }
        for &fd in &fds {
            if write_all(fd, data).is_err() {
                status = 1;
            }
        }
    }
    for fd in fds {
        close(fd);
    }
    status
}

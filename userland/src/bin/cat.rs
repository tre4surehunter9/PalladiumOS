// Copyright (c) 2026 tre4surehunter9
//! cat: copy standard input to standard output until end-of-file (Ctrl-D).
//! Reading files arrives with the file syscalls (milestone B).
#![no_std]
#![no_main]

use userlib::{eprintln, read, write, STDIN, STDOUT};

userlib::entry!(main);

fn main(args: userlib::Args) -> i32 {
    if args.count() > 1 {
        eprintln!("cat: file arguments are not supported yet (no open syscall)");
        return 1;
    }
    let mut buf = [0u8; 256];
    loop {
        let n = read(STDIN, &mut buf);
        if n == 0 {
            return 0;
        }
        if n < 0 {
            eprintln!("cat: read error {}", n);
            return 1;
        }
        write(STDOUT, &buf[..n as usize]);
    }
}

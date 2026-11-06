// Copyright (c) 2026 tre4surehunter9
//! Test program: read one line from stdin, check it says "hi\n", then
//! check the next read is end-of-file. Exit 0 on success.
#![no_std]
#![no_main]

use userlib::{read, STDIN};

userlib::entry!(main);

fn main(_args: userlib::Args) -> i32 {
    let mut buf = [0u8; 16];
    let n = read(STDIN, &mut buf);
    if n != 3 {
        return 10 + n as i32;
    }
    if &buf[..3] != b"hi\n" {
        return 2;
    }
    if read(STDIN, &mut buf) != 0 {
        return 3; // expected EOF (Ctrl-D)
    }
    0
}

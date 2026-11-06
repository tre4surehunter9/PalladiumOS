// Copyright (c) 2026 tre4surehunter9
//! clear: clear the screen (with the standard ANSI escape sequence).
#![no_std]
#![no_main]

userlib::entry!(main);

fn main(_args: userlib::Args) -> i32 {
    userlib::write(userlib::STDOUT, b"\x1b[H\x1b[2J");
    0
}

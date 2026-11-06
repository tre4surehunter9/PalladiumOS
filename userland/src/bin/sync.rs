// Copyright (c) 2026 tre4surehunter9
//! sync: make sure everything written has reached the disk.
#![no_std]
#![no_main]

userlib::entry!(main);

fn main(_args: userlib::Args) -> i32 {
    userlib::sync();
    0
}

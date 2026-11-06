// Copyright (c) 2026 tre4surehunter9
//! Test program: panic. userlib's panic handler exits with status 101.
#![no_std]
#![no_main]

userlib::entry!(main);

fn main(_args: userlib::Args) -> i32 {
    panic!("test panic");
}

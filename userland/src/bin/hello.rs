// Copyright (c) 2026 tre4surehunter9
//! hello: prints a greeting from ring 3.
#![no_std]
#![no_main]

use userlib::println;

userlib::entry!(main);

fn main(_args: userlib::Args) -> i32 {
    println!("Hello from Rust in ring 3! (pid {})", userlib::getpid());
    0
}

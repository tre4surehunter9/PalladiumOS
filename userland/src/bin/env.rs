// Copyright (c) 2026 tre4surehunter9
//! env: print the environment, one NAME=value per line.
#![no_std]
#![no_main]

use userlib::{environ, println};

userlib::entry!(main);

fn main(_args: userlib::Args) -> i32 {
    for var in environ() {
        println!("{}", var);
    }
    0
}

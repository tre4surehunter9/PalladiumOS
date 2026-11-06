// Copyright (c) 2026 tre4surehunter9
//! pwd: print the working directory.
#![no_std]
#![no_main]

use userlib::{eprintln, getcwd, println, strerror};

userlib::entry!(main);

fn main(_args: userlib::Args) -> i32 {
    let mut buf = [0u8; 4096];
    match getcwd(&mut buf) {
        Ok(cwd) => {
            println!("{}", cwd);
            0
        }
        Err(e) => {
            eprintln!("pwd: {}", strerror(-e));
            1
        }
    }
}

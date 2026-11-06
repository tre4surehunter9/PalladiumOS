// Copyright (c) 2026 tre4surehunter9
//! about: information about PalladiumOS.
#![no_std]
#![no_main]

use userlib::{field_str, println};

userlib::entry!(main);

fn main(_args: userlib::Args) -> i32 {
    let u = userlib::uname().ok();
    let release = u.as_ref().map(|u| field_str(&u.release)).unwrap_or("?");
    let machine = u.as_ref().map(|u| field_str(&u.machine)).unwrap_or("?");
    println!("PalladiumOS v{} ({}) - A kernel in Rust", release, machine);
    println!("Based on Philipp Oppermann's 'Writing an OS in Rust'");
    println!("https://github.com/tre4surehunter9/PalladiumOS/");
    0
}

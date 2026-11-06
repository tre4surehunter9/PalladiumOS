// Copyright (c) 2026 tre4surehunter9
//! uptime: how long the system has been running.
#![no_std]
#![no_main]

use userlib::{monotonic_time, println};

userlib::entry!(main);

fn main(_args: userlib::Args) -> i32 {
    let (secs, _) = monotonic_time();
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        println!("up {}:{:02}:{:02}", h, m, s);
    } else {
        println!("up {} min {} sec", m, s);
    }
    0
}

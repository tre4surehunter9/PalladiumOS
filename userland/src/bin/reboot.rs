// Copyright (c) 2026 tre4surehunter9
//! reboot: restart the machine.
#![no_std]
#![no_main]

use userlib::{eprintln, reboot, strerror, REBOOT_RESTART};

userlib::entry!(main);

fn main(_args: userlib::Args) -> i32 {
    let r = reboot(REBOOT_RESTART);
    eprintln!("reboot: {}", strerror(-r));
    1
}

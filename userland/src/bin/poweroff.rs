// Copyright (c) 2026 tre4surehunter9
//! poweroff: turn the machine off (works in QEMU).
#![no_std]
#![no_main]

use userlib::{eprintln, reboot, strerror, REBOOT_POWER_OFF};

userlib::entry!(main);

fn main(_args: userlib::Args) -> i32 {
    let r = reboot(REBOOT_POWER_OFF);
    eprintln!("poweroff: {}", strerror(-r));
    1
}

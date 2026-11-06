// Copyright (c) 2026 tre4surehunter9
//! whoami: print your (effective) user name.
#![no_std]
#![no_main]

userlib::entry!(main);

fn main(_args: userlib::Args) -> i32 {
    userlib::println!("{}", userlib::users::user_name(userlib::geteuid()));
    0
}

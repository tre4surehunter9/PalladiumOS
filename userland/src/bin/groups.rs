// Copyright (c) 2026 tre4surehunter9
//! groups: print the groups you (or USER) are in.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use userlib::users::{group_name, groups_of, user_by_name};
use userlib::*;

userlib::entry!(main);

fn main(args: Args) -> i32 {
    let mut gids: Vec<u32> = match args.get(1) {
        Some(name) => match user_by_name(name) {
            Some(u) => core::iter::once(u.gid).chain(groups_of(name)).collect(),
            None => {
                eprintln!("groups: '{}': no such user", name);
                return 1;
            }
        },
        None => core::iter::once(getegid()).chain(getgroups()).collect(),
    };
    gids.dedup();
    let mut seen = Vec::new();
    let names: Vec<String> = gids.into_iter().filter(|g| !seen.contains(g) && { seen.push(*g); true }).map(group_name).collect();
    println!("{}", names.join(" "));
    0
}

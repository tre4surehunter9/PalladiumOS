// Copyright (c) 2026 tre4surehunter9
//! id: print user and group IDs.
//!   id [USER]   -u: only the uid, -g: only the gid, -n: names with -u/-g
#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use userlib::users::{group_name, groups_of, user_by_name, user_name};
use userlib::*;

userlib::entry!(main);

fn main(args: Args) -> i32 {
    let (mut only_u, mut only_g, mut names) = (false, false, false);
    let mut who = None;
    for a in args.skip(1) {
        match a {
            "-u" => only_u = true,
            "-g" => only_g = true,
            "-n" => names = true,
            n => who = Some(n),
        }
    }
    let (uid, gid, groups): (u32, u32, Vec<u32>) = match who {
        Some(name) => match user_by_name(name) {
            Some(u) => {
                let mut g = groups_of(&u.name);
                if !g.contains(&u.gid) {
                    g.insert(0, u.gid);
                }
                (u.uid, u.gid, g)
            }
            None => {
                eprintln!("id: '{}': no such user", name);
                return 1;
            }
        },
        None => {
            let mut g = getgroups();
            if !g.contains(&getegid()) {
                g.insert(0, getegid());
            }
            (geteuid(), getegid(), g)
        }
    };
    if only_u {
        println!("{}", if names { user_name(uid) } else { format!("{}", uid) });
        return 0;
    }
    if only_g {
        println!("{}", if names { group_name(gid) } else { format!("{}", gid) });
        return 0;
    }
    let list: Vec<String> = groups.iter().map(|g| format!("{}({})", g, group_name(*g))).collect();
    println!("uid={}({}) gid={}({}) groups={}", uid, user_name(uid), gid, group_name(gid), list.join(","));
    0
}

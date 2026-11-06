// Copyright (c) 2026 tre4surehunter9
//! chgrp: change a file's group (same code as chown).
//!   chown [-h] USER[:GROUP] FILE...   (also UID, :GROUP)
//!   chgrp [-h] GROUP FILE...
//! -h changes a symlink itself. Only root may give files to another user;
//! owners may change the group to one of their own groups.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use userlib::users::{group_by_name, user_by_name};
use userlib::*;

userlib::entry!(main);

fn parse_user(s: &str) -> Option<u32> {
    s.parse().ok().or_else(|| user_by_name(s).map(|u| u.uid))
}

fn parse_group(s: &str) -> Option<u32> {
    s.parse().ok().or_else(|| group_by_name(s).map(|g| g.gid))
}

fn main(args: Args) -> i32 {
    let chgrp = args.get(0).is_some_and(|a| a.ends_with("chgrp"));
    let prog = if chgrp { "chgrp" } else { "chown" };
    let no_follow = args.skip(1).any(|a| a == "-h");
    let rest: Vec<&str> = args.skip(1).filter(|a| *a != "-h").collect();
    if rest.len() < 2 {
        eprintln!("usage: {} {} FILE...", prog, if chgrp { "GROUP" } else { "USER[:GROUP]" });
        return 2;
    }
    let spec = rest[0];
    const KEEP: u32 = u32::MAX;
    let (uid, gid) = if chgrp {
        match parse_group(spec) {
            Some(g) => (KEEP, g),
            None => {
                eprintln!("chgrp: invalid group: '{}'", spec);
                return 1;
            }
        }
    } else {
        let (u, g) = spec.split_once(':').map_or((spec, None), |(u, g)| (u, Some(g)));
        let uid = if u.is_empty() {
            KEEP
        } else {
            match parse_user(u) {
                Some(id) => id,
                None => {
                    eprintln!("chown: invalid user: '{}'", u);
                    return 1;
                }
            }
        };
        let gid = match g {
            None | Some("") => KEEP,
            Some(g) => match parse_group(g) {
                Some(id) => id,
                None => {
                    eprintln!("chown: invalid group: '{}'", g);
                    return 1;
                }
            },
        };
        (uid, gid)
    };
    let mut status = 0;
    for path in &rest[1..] {
        let r = if no_follow { lchown(path, uid, gid) } else { chown(path, uid, gid) };
        if r < 0 {
            eprintln!("{}: changing ownership of '{}': {}", prog, path, strerror(-r));
            status = 1;
        }
    }
    status
}

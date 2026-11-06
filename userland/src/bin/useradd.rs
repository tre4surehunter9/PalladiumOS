// Copyright (c) 2026 tre4surehunter9
//! useradd: create a user (root only).
//!   useradd [-u UID] [-G GROUP,...] NAME
//! Adds the user and a group of the same name, and creates /home/NAME.
//! Set a password afterwards with `passwd NAME`.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use userlib::users::{append_line, group_by_name, groups, set_shadow_hash, user_by_name, users, write_file_atomic};
use userlib::*;

userlib::entry!(main);

fn main(args: Args) -> i32 {
    let rest: Vec<&str> = args.skip(1).collect();
    let (mut uid, mut extra, mut name) = (None, Vec::new(), None);
    let mut i = 0;
    while i < rest.len() {
        match rest[i] {
            "-u" => {
                i += 1;
                uid = rest.get(i).and_then(|u| u.parse::<u32>().ok());
            }
            "-G" => {
                i += 1;
                extra = rest.get(i).map(|g| g.split(',').map(String::from).collect()).unwrap_or_default();
            }
            n => name = Some(n),
        }
        i += 1;
    }
    let Some(name) = name else {
        eprintln!("usage: useradd [-u UID] [-G GROUP,...] NAME");
        return 2;
    };
    if geteuid() != 0 {
        eprintln!("useradd: permission denied (are you root?)");
        return 1;
    }
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        eprintln!("useradd: invalid user name '{}'", name);
        return 1;
    }
    if user_by_name(name).is_some() {
        eprintln!("useradd: user '{}' already exists", name);
        return 1;
    }
    let uid = uid.unwrap_or_else(|| users().iter().map(|u| u.uid).filter(|&u| u >= 1000).max().map_or(1000, |m| m + 1));
    if users().iter().any(|u| u.uid == uid) {
        eprintln!("useradd: UID {} is not unique", uid);
        return 1;
    }
    let gid = if group_by_name(name).is_some() || groups().iter().any(|g| g.gid == uid) {
        groups().iter().map(|g| g.gid).max().unwrap_or(1000) + 1
    } else {
        uid
    };
    let home = format!("/home/{}", name);
    let steps = [
        append_line("/etc/passwd", &format!("{}:x:{}:{}::{}:/bin/sh", name, uid, gid, home)),
        append_line("/etc/group", &format!("{}:x:{}:", name, gid)),
        set_shadow_hash(name, ""),
    ];
    if let Some(Err(e)) = steps.into_iter().find(|r| r.is_err()) {
        eprintln!("useradd: {}", strerror(-e));
        return 1;
    }
    // Supplementary groups: add the user to each group's member list.
    if !extra.is_empty() {
        let text = userlib::io::read_text("/etc/group").unwrap_or_default();
        let updated: String = text
            .lines()
            .map(|l| {
                let f: Vec<&str> = l.split(':').collect();
                if f.len() >= 4 && extra.iter().any(|g| g == f[0]) {
                    let sep = if f[3].is_empty() { "" } else { "," };
                    format!("{}:{}:{}:{}{}{}\n", f[0], f[1], f[2], f[3], sep, name)
                } else {
                    format!("{}\n", l)
                }
            })
            .collect();
        let _ = write_file_atomic("/etc/group", updated.as_bytes(), 0o644);
    }
    if mkdir("/home") < 0 && stat("/home").is_err() {
        eprintln!("useradd: can't create /home");
    }
    if mkdir(&home) == 0 {
        chown(&home, uid, gid);
        chmod(&home, 0o755);
    }
    println!("useradd: created user {} (uid {}), home {}", name, uid, home);
    0
}

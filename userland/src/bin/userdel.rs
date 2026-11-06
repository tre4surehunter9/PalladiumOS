// Copyright (c) 2026 tre4surehunter9
//! userdel: remove a user (root only).  userdel [-r] NAME
//! -r also removes the home directory.
#![no_std]
#![no_main]

use userlib::users::{remove_entry, user_by_name};
use userlib::*;

userlib::entry!(main);

fn main(args: Args) -> i32 {
    let remove_home = args.skip(1).any(|a| a == "-r");
    let Some(name) = args.skip(1).find(|a| !a.starts_with('-')) else {
        eprintln!("usage: userdel [-r] NAME");
        return 2;
    };
    if geteuid() != 0 {
        eprintln!("userdel: permission denied (are you root?)");
        return 1;
    }
    let Some(user) = user_by_name(name) else {
        eprintln!("userdel: user '{}' does not exist", name);
        return 1;
    };
    if user.uid == 0 {
        eprintln!("userdel: refusing to remove root");
        return 1;
    }
    for (path, mode) in [("/etc/passwd", 0o644), ("/etc/shadow", 0o600), ("/etc/group", 0o644)] {
        if let Err(e) = remove_entry(path, name, mode) {
            eprintln!("userdel: {}: {}", path, strerror(-e));
            return 1;
        }
    }
    if remove_home {
        let _ = spawn_and_wait("/bin/rm", &["rm", "-r", &user.home]);
    }
    0
}

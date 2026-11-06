// Copyright (c) 2026 tre4surehunter9
//! passwd: change a password.
//!   passwd           your own (asks for the current one first)
//!   passwd USER      someone else's (root only)
//!   passwd -d USER   remove the password (root only)
//! Installed setuid root, since /etc/shadow is only writable by root.
#![no_std]
#![no_main]

use userlib::users::{check_password, hash_password, read_password, set_shadow_hash, shadow_hash, user_by_name, user_by_uid};
use userlib::*;

userlib::entry!(main);

fn main(args: Args) -> i32 {
    let me = getuid(); // the real user (passwd itself runs as root)
    let delete = args.get(1) == Some("-d");
    let target = if delete { args.get(2) } else { args.get(1) };
    let user = match target {
        Some(name) => user_by_name(name),
        None => user_by_uid(me),
    };
    let Some(user) = user else {
        eprintln!("passwd: unknown user");
        return 1;
    };
    if me != 0 && user.uid != me {
        eprintln!("passwd: you may not change the password for {}", user.name);
        return 1;
    }
    if delete {
        if me != 0 {
            eprintln!("passwd: only root can remove passwords");
            return 1;
        }
        return match set_shadow_hash(&user.name, "") {
            Ok(()) => {
                println!("passwd: password for {} removed", user.name);
                0
            }
            Err(e) => {
                eprintln!("passwd: {}", strerror(-e));
                1
            }
        };
    }
    println!("Changing password for {}.", user.name);
    if me != 0 {
        let current = shadow_hash(&user.name).unwrap_or_default();
        if !current.is_empty() {
            let ok = read_password("Current password: ").is_some_and(|p| check_password(&current, &p));
            if !ok {
                sleep_ms(1000);
                eprintln!("passwd: Authentication token manipulation error");
                return 1;
            }
        }
    }
    let Some(new) = read_password("New password: ") else { return 1 };
    let Some(again) = read_password("Retype new password: ") else { return 1 };
    if new != again {
        eprintln!("passwd: Sorry, passwords do not match.");
        return 1;
    }
    match set_shadow_hash(&user.name, &hash_password(&new)) {
        Ok(()) => {
            println!("passwd: password updated successfully");
            0
        }
        Err(e) => {
            eprintln!("passwd: can't update /etc/shadow: {}", strerror(-e));
            1
        }
    }
}

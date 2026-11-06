// Copyright (c) 2026 tre4surehunter9
//! su: run a shell as another user (root by default).
//!   su [-] [USER] [-c COMMAND]
//! Asks for that user's password, unless you're root. "-" starts a login
//! shell: the user's home directory and a clean environment. Installed
//! setuid root (it needs to change user IDs).
#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use userlib::users::{become_user, check_password, login_env, read_password, shadow_hash, user_by_name};
use userlib::*;

userlib::entry!(main);

fn main(args: Args) -> i32 {
    let mut login = false;
    let mut name = "root";
    let mut command = None;
    let rest: Vec<&str> = args.skip(1).collect();
    let mut i = 0;
    while i < rest.len() {
        match rest[i] {
            "-" | "-l" | "--login" => login = true,
            "-c" => {
                i += 1;
                command = rest.get(i).copied();
            }
            n => name = n,
        }
        i += 1;
    }
    let Some(user) = user_by_name(name) else {
        eprintln!("su: user {} does not exist", name);
        return 1;
    };
    // Who's asking: the real uid (su runs as root because it's setuid).
    if getuid() != 0 {
        let hash = shadow_hash(name).unwrap_or_default();
        if !hash.is_empty() {
            let ok = read_password("Password: ").is_some_and(|p| check_password(&hash, &p));
            if !ok {
                sleep_ms(1000);
                eprintln!("su: Authentication failure");
                return 1;
            }
        }
    }
    if let Err(e) = become_user(&user) {
        eprintln!("su: can't switch to {}: {}", name, strerror(-e));
        return 1;
    }
    let env: Vec<String> = if login {
        if chdir(&user.home) < 0 {
            chdir("/");
        }
        login_env(&user)
    } else {
        // Keep the environment, but say who we are now.
        environ()
            .filter(|kv| !kv.starts_with("USER=") && !kv.starts_with("LOGNAME=") && !kv.starts_with("HOME="))
            .map(String::from)
            .chain([alloc::format!("USER={}", user.name), alloc::format!("LOGNAME={}", user.name), alloc::format!("HOME={}", user.home)])
            .collect()
    };
    let envp: Vec<&str> = env.iter().map(String::as_str).collect();
    let err = match command {
        Some(c) => execve(&user.shell, &["sh", "-c", c], &envp),
        None => execve(&user.shell, &["sh"], &envp),
    };
    eprintln!("su: can't run {}: {}", user.shell, strerror(-err));
    1
}

// Copyright (c) 2026 tre4surehunter9
//! login: ask for a user name and password, then start that user's shell.
//!
//! Started by the kernel at boot (as root). After a successful login it
//! switches to the user and execs their shell; when the shell exits, the
//! kernel starts login again. Ctrl-D at the prompt exits with status 99,
//! which tells the kernel to open its built-in kernel shell instead.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use userlib::users::{become_user, check_password, login_env, read_password, shadow_hash, user_by_name};
use userlib::*;

userlib::entry!(main);

/// Exit status meaning "the user wants the kernel shell".
const TO_KERNEL_SHELL: i32 = 99;

fn read_name() -> Option<String> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let n = read(STDIN, &mut byte);
        if n <= 0 {
            return None;
        }
        if byte[0] == b'\n' {
            return Some(String::from_utf8_lossy(&line).trim().into());
        }
        line.push(byte[0]);
    }
}

fn main(_args: Args) -> i32 {
    if geteuid() != 0 {
        eprintln!("login: must be run as root");
        return 1;
    }
    println!();
    println!("PalladiumOS 0.4.0 (palladium)");
    println!("Log in as 'user' or 'root' (no passwords yet: set them with passwd).");
    println!("Ctrl-D here opens the kernel shell.");
    loop {
        print!("\npalladium login: ");
        let Some(name) = read_name() else {
            println!();
            return TO_KERNEL_SHELL;
        };
        if name.is_empty() {
            continue;
        }
        let user = user_by_name(&name);
        let hash = shadow_hash(&name);
        let password_ok = match (&user, &hash) {
            (Some(_), Some(h)) if h.is_empty() => true,
            (Some(_), Some(h)) => read_password("Password: ").is_some_and(|p| check_password(h, &p)),
            _ => {
                // Ask anyway, so a wrong name looks the same as a wrong password.
                let _ = read_password("Password: ");
                false
            }
        };
        let Some(user) = user.filter(|_| password_ok) else {
            sleep_ms(1000); // slow down guessing
            println!("Login incorrect");
            continue;
        };
        if let Err(e) = become_user(&user) {
            eprintln!("login: can't switch to {}: {}", user.name, strerror(-e));
            continue;
        }
        if chdir(&user.home) < 0 {
            println!("No home directory {}, using /", user.home);
            chdir("/");
        }
        if let Some(motd) = userlib::io::read_text("/etc/motd") {
            print!("{}", motd);
        }
        let env = login_env(&user);
        let envp: Vec<&str> = env.iter().map(String::as_str).collect();
        let err = execve(&user.shell, &["sh"], &envp);
        eprintln!("login: can't run {}: {}", user.shell, strerror(-err));
        return 1;
    }
}

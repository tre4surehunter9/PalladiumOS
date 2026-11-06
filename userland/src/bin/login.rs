// Copyright (c) 2026 tre4surehunter9
//! login: ask for a user name and password, then start that user's shell.
//!
//! Started by the kernel at boot (as root). After a successful login it
//! runs the user's shell as a child (as that user) and waits; when the
//! shell exits, it asks again. Ctrl-D at the prompt (with root's password,
//! if root has one) exits with status 99, which tells the kernel to open
//! its built-in kernel shell. login is the only process that can do that.
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

/// Ctrl-D at the prompt: the kernel shell runs as root, so it needs root's
/// password. (With no root password set, anyone can log in as root anyway.)
fn kernel_shell_allowed() -> bool {
    match shadow_hash("root") {
        Some(hash) if !hash.is_empty() => {
            let ok = read_password("Root password for the kernel shell: ").is_some_and(|p| check_password(&hash, &p));
            if !ok {
                sleep_ms(1000);
                println!("Login incorrect");
            }
            ok
        }
        _ => true,
    }
}

/// Run `user`'s shell as a child and wait for it. login stays root and
/// keeps control: only login itself decides to open the kernel shell (an
/// `exit 99` from the user's shell is just a logout).
fn session(user: &userlib::users::User, saved_tty: Option<Termios>) {
    let pid = fork();
    if pid < 0 {
        eprintln!("login: fork: {}", strerror(-pid));
        return;
    }
    if pid == 0 {
        // The session's own process group, owning the terminal; signals
        // back to their defaults (login ignores them).
        setpgid(0, 0);
        tcsetpgrp(STDIN, getpid());
        for sig in [SIGINT, SIGQUIT, SIGTSTP] {
            set_signal(sig, SIG_DFL);
        }
        if let Err(e) = become_user(user) {
            eprintln!("login: can't switch to {}: {}", user.name, strerror(-e));
            exit(1);
        }
        if chdir(&user.home) < 0 {
            println!("No home directory {}, using /", user.home);
            chdir("/");
        }
        if let Some(motd) = userlib::io::read_text("/etc/motd") {
            print!("{}", motd);
        }
        let env = login_env(user);
        let envp: Vec<&str> = env.iter().map(String::as_str).collect();
        let err = execve(&user.shell, &["sh"], &envp);
        eprintln!("login: can't run {}: {}", user.shell, strerror(-err));
        exit(1);
    }
    let mut status = 0;
    while waitpid(pid as i32, &mut status, 0) == -EINTR as isize {}
    // Logged out: the terminal is login's again, in its normal mode.
    tcsetpgrp(STDIN, getpgrp());
    if let Some(t) = saved_tty {
        tcsetattr(STDIN, &t);
    }
}

fn main(_args: Args) -> i32 {
    if geteuid() != 0 {
        eprintln!("login: must be run as root");
        return 1;
    }
    // Ctrl-C and friends are for the session, not for login.
    for sig in [SIGINT, SIGQUIT, SIGTSTP] {
        set_signal(sig, SIG_IGN);
    }
    let saved_tty = tcgetattr(STDIN).ok();
    let no_root_password = shadow_hash("root").is_none_or(|h| h.is_empty());
    println!();
    println!("PalladiumOS 0.4.0 (palladium)");
    if no_root_password {
        println!("Log in as 'user' or 'root' (no passwords yet: set them with passwd).");
        println!("Ctrl-D here opens the kernel shell.");
    } else {
        println!("Ctrl-D here opens the kernel shell (root's password needed).");
    }
    loop {
        print!("\npalladium login: ");
        let Some(name) = read_name() else {
            println!();
            if kernel_shell_allowed() {
                return TO_KERNEL_SHELL;
            }
            continue;
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
        session(&user, saved_tty);
    }
}

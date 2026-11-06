// Copyright (c) 2026 tre4surehunter9
//! Test program for users and permissions. Run as root: it sets up files,
//! then forks a child that becomes uid 1000 and checks what it may do.
//! Exits 0, or the line number of the first failed check.
//!   test_perm all   - the checks
//!   test_perm ids   - print "ruid euid" (used to test setuid execution)
#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use userlib::*;

userlib::entry!(main);

macro_rules! check {
    ($cond:expr) => {
        if !$cond {
            eprintln!("test_perm: check failed at line {}: {}", line!(), stringify!($cond));
            return line!() as i32;
        }
    };
}

fn write_file(path: &str, data: &[u8]) -> isize {
    let fd = open(path, O_CREAT | O_WRONLY | O_TRUNC);
    if fd < 0 {
        return fd;
    }
    let r = write_all(fd as i32, data);
    close(fd as i32);
    r.err().unwrap_or(0)
}

fn main(args: Args) -> i32 {
    match args.get(1) {
        Some("all") => all(),
        Some("ids") => {
            println!("{} {}", getuid(), geteuid());
            0
        }
        _ => 98,
    }
}

fn all() -> i32 {
    check!(getuid() == 0 && geteuid() == 0);
    spawn_and_wait("/bin/rm", &["rm", "-rf", "/pt"]).ok();
    check!(mkdir("/pt") == 0);
    // Files set up by root:
    check!(write_file("/pt/secret", b"s") == 0 && chmod("/pt/secret", 0o600) == 0);
    check!(write_file("/pt/public", b"p") == 0 && chmod("/pt/public", 0o644) == 0);
    check!(write_file("/pt/group", b"g") == 0 && chmod("/pt/group", 0o660) == 0 && chown("/pt/group", 0, 10) == 0);
    check!(mkdir("/pt/locked") == 0 && chmod("/pt/locked", 0o700) == 0);
    check!(write_file("/pt/locked/inside", b"i") == 0);
    check!(mkdir("/pt/shared") == 0 && chmod("/pt/shared", 0o1777) == 0); // like /tmp
    check!(write_file("/pt/shared/roots", b"r") == 0);
    check!(mkdir("/pt/mine") == 0 && chown("/pt/mine", 1000, 1000) == 0);
    check!(stat("/pt/mine").is_ok_and(|s| s.st_uid == 1000 && s.st_gid == 1000));
    // A setuid-root copy of this program, to test setuid execution.
    check!(spawn_and_wait("/bin/cp", &["cp", "/bin/test_perm", "/pt/suid"]).is_ok());
    check!(chmod("/pt/suid", 0o4755) == 0);
    // Root can read anything, but not run a file without an execute bit.
    check!(open("/pt/secret", O_RDONLY) >= 0);
    check!(access("/pt/secret", X_OK) == -EACCES);

    let pid = fork();
    if pid == 0 {
        exit(as_user());
    }
    let mut status = 0;
    check!(waitpid(pid as i32, &mut status, 0) == pid);
    if wexitstatus(status) != 0 {
        eprintln!("test_perm: the unprivileged part failed at line {}", wexitstatus(status));
        return 200;
    }
    // Clean up as root.
    check!(spawn_and_wait("/bin/rm", &["rm", "-r", "/pt"]).is_ok_and(|s| wexitstatus(s) == 0));
    0
}

/// The checks done as uid 1000 (gid 1000, also in group 10).
fn as_user() -> i32 {
    check!(setgroups(&[1000, 10]) == 0);
    check!(setresgid(1000, 1000, 1000) == 0);
    check!(setresuid(1000, 1000, 1000) == 0);
    check!(getuid() == 1000 && geteuid() == 1000 && getgid() == 1000);
    // No way back: an ordinary user can't become root again.
    check!(setuid(0) == -EPERM as isize);
    check!(setresuid(0, 0, 0) == -EPERM as isize);
    check!(setgroups(&[0]) == -EPERM as isize);

    // ---- reading and writing ----
    check!(open("/pt/secret", O_RDONLY) == -EACCES);
    check!(open("/pt/public", O_RDONLY) >= 0);
    check!(open("/pt/public", O_WRONLY) == -EACCES);
    check!(open("/pt/public", O_RDONLY | O_TRUNC) == -EACCES); // truncating needs write
    check!(open("/pt/group", O_RDWR) >= 0); // via group 10
    check!(access("/pt/secret", R_OK) == -EACCES);
    check!(access("/pt/public", R_OK) == 0 && access("/pt/public", W_OK) == -EACCES);

    // ---- directories ----
    check!(stat("/pt/locked/inside").err() == Some(-EACCES)); // no search permission
    check!(open("/pt/locked", O_RDONLY | O_DIRECTORY) == -EACCES); // can't list it
    check!(chdir("/pt/locked") == -EACCES);
    check!(write_file("/pt/new", b"x") == -EACCES); // /pt is root's, 755
    check!(mkdir("/pt/newdir") == -EACCES);
    check!(unlink("/pt/public") == -EACCES);
    check!(rename("/pt/public", "/pt/mine/stolen") == -EACCES);
    check!(symlink("x", "/pt/link") == -EACCES);
    // In our own directory, everything works, and new files are ours.
    check!(write_file("/pt/mine/f", b"mine") == 0);
    check!(stat("/pt/mine/f").is_ok_and(|s| s.st_uid == 1000 && s.st_gid == 1000 && s.st_mode & 0o777 == 0o644));
    check!(mkdir("/pt/mine/d") == 0 && rename("/pt/mine/d", "/pt/mine/e") == 0);

    // ---- the sticky directory ----
    check!(write_file("/pt/shared/users", b"u") == 0);
    check!(unlink("/pt/shared/roots") == -EPERM); // someone else's file
    check!(rename("/pt/shared/roots", "/pt/shared/x") == -EPERM);
    check!(unlink("/pt/shared/users") == 0); // our own file is fine

    // ---- chmod, chown, utimensat ----
    check!(chmod("/pt/public", 0o777) == -EPERM); // not ours
    check!(chmod("/pt/mine/f", 0o600) == 0);
    check!(chown("/pt/mine/f", 0, u32::MAX) == -EPERM); // can't give files away
    check!(chown("/pt/mine/f", u32::MAX, 10) == 0); // to a group we're in
    check!(chown("/pt/mine/f", u32::MAX, 0) == -EPERM); // not in group root
    check!(stat("/pt/mine/f").is_ok_and(|s| s.st_gid == 10));
    check!(touch_now("/pt/mine/f") == 0);
    check!(touch_now("/pt/public") == -EACCES); // not ours, not writable

    // ---- executing ----
    check!(write_file("/pt/mine/script", b"#!/bin/sh\nexit 3\n") == 0);
    match spawn_and_wait("/pt/mine/script", &["script"]) {
        Ok(s) => check!(wexitstatus(s) == 126 || wexitstatus(s) == 127), // no x bit
        Err(_) => return line!() as i32,
    }
    check!(chmod("/pt/mine/script", 0o700) == 0);
    match spawn_and_wait("/pt/mine/script", &["script"]) {
        Ok(s) => check!(wexitstatus(s) == 3),
        Err(_) => return line!() as i32,
    }
    // A setuid-root program runs with euid 0 (the real uid stays ours).
    let Ok((r, w)) = pipe() else { return line!() as i32 };
    let pid = fork();
    if pid == 0 {
        dup2(w, STDOUT);
        close(r);
        close(w);
        execve("/pt/suid", &["suid", "ids"], &[]);
        exit(1);
    }
    close(w);
    let mut buf = [0u8; 32];
    let n = read(r, &mut buf);
    close(r);
    let mut status = 0;
    waitpid(pid as i32, &mut status, 0);
    check!(n > 0 && &buf[..n as usize] == b"1000 0\n");

    // ---- signals ----
    // An unprivileged process may signal its own processes, not root's.
    let mine = fork();
    if mine == 0 {
        sleep_ms(10_000);
        exit(0);
    }
    check!(kill(mine as i32, SIGTERM) == 0);
    waitpid(mine as i32, &mut status, 0);
    check!(kill(getppid(), 0) == -EPERM as isize); // our parent runs as root

    // ---- umask ----
    umask(0o077);
    check!(write_file("/pt/mine/private", b"p") == 0);
    check!(stat("/pt/mine/private").is_ok_and(|s| s.st_mode & 0o777 == 0o600));
    let _ = format!("");
    0
}

// Copyright (c) 2026 tre4surehunter9
//! Test program for /proc and terminal read timeouts. Exits 0, or the
//! line number of the first failed check.
//!   test_procfs all      - /proc checks
//!   test_procfs timeout  - a raw read with VMIN 0 / VTIME 3 must time out
#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use userlib::io::read_text;
use userlib::procinfo::{all, meminfo, parse_stat, read_proc};
use userlib::*;

userlib::entry!(main);

macro_rules! check {
    ($cond:expr) => {
        if !$cond {
            eprintln!("test_procfs: check failed at line {}: {}", line!(), stringify!($cond));
            return line!() as i32;
        }
    };
}

fn main(args: Args) -> i32 {
    match args.get(1) {
        Some("all") => all_checks(),
        Some("timeout") => timeout(),
        Some("child") => {
            sleep_ms(5000);
            0
        }
        _ => 98,
    }
}

fn all_checks() -> i32 {
    let me = getpid() as u32;
    let mut buf = [0u8; 256];

    // ---- /proc/self ----
    check!(readlink("/proc/self", &mut buf) == Ok(format!("{}", me).as_str()));
    let status = read_text("/proc/self/status").unwrap_or_default();
    check!(status.contains("Name:\ttest_procfs\n"));
    check!(status.contains(&format!("Pid:\t{}\n", me)));
    check!(status.contains(&format!("PPid:\t{}\n", getppid())));
    check!(status.contains("State:\tR (running)"));
    let parsed = parse_stat(&read_text("/proc/self/stat").unwrap_or_default());
    check!(parsed.as_ref().is_some_and(|s| s.pid == me && s.name == "test_procfs" && s.state == 'R'));
    check!(parsed.as_ref().is_some_and(|s| s.rss_kb > 0));
    check!(read_text("/proc/self/cmdline") == Some(alloc::string::String::from("test_procfs\0all\0")));
    check!(readlink("/proc/self/exe", &mut buf).is_ok_and(|e| e.ends_with("test_procfs")));
    chdir("/bin");
    check!(readlink("/proc/self/cwd", &mut buf) == Ok("/bin"));
    chdir("/");

    // ---- /proc/self/fd ----
    let fd = open("/proc/self/status", O_RDONLY) as i32;
    check!(fd == 3);
    check!(readlink("/proc/self/fd/3", &mut buf) == Ok(format!("/proc/{}/status", me).as_str())
        || readlink("/proc/self/fd/3", &mut buf) == Ok("/proc/self/status"));
    check!(readlink("/proc/self/fd/0", &mut buf) == Ok("/dev/tty"));
    close(fd);
    check!(lstat("/proc/self/fd/3").err() == Some(-ENOENT));
    let Ok((r, w)) = pipe() else { return line!() as i32 };
    check!(readlink(&format!("/proc/self/fd/{}", r), &mut buf).is_ok_and(|t| t.starts_with("pipe:[")));
    close(r);
    close(w);

    // ---- another process ----
    let pid = fork();
    if pid == 0 {
        execve("/bin/test_procfs", &["test_procfs", "child"], &[]);
        exit(1);
    }
    sleep_ms(200);
    let child = read_proc(pid as u32);
    check!(child.as_ref().is_some_and(|c| c.ppid == me && c.state == 'S' && c.cmdline == "test_procfs child"));
    check!(all().iter().any(|p| p.pid == pid as u32));
    kill(pid as i32, SIGKILL);
    let mut status = 0;
    waitpid(pid as i32, &mut status, 0);
    check!(read_proc(pid as u32).is_none()); // gone once reaped
    check!(stat(&format!("/proc/{}", pid)).err() == Some(-ENOENT));

    // ---- system files ----
    check!(meminfo("MemTotal") > 1024 && meminfo("MemFree") <= meminfo("MemTotal"));
    let up: f64 = read_text("/proc/uptime").unwrap_or_default().split(' ').next().and_then(|s| s.parse().ok()).unwrap_or(-1.0);
    check!(up >= 0.0);
    check!(read_text("/proc/mounts").is_some_and(|m| m.contains(" /proc proc ")));
    check!(read_text("/proc/version").is_some_and(|v| v.starts_with("PalladiumOS")));
    check!(read_text("/proc/cpuinfo").is_some_and(|c| c.contains("model name")));
    check!(read_text("/proc/loadavg").is_some_and(|l| l.split(' ').count() == 5));

    // ---- read-only ----
    check!(open("/proc/version", O_WRONLY) == -EACCES || open("/proc/version", O_WRONLY) == -EISDIR || write(open("/proc/version", O_WRONLY) as i32, b"x") < 0);
    check!(mkdir("/proc/new") == -EPERM);
    check!(open("/proc/nope", O_RDONLY) == -ENOENT);
    check!(open("/proc/99999/status", O_RDONLY) == -ENOENT);
    0
}

fn timeout() -> i32 {
    let Ok(saved) = tcgetattr(STDIN) else { return 1 };
    let mut raw = saved;
    raw.c_lflag &= !(ICANON | ECHO | ISIG);
    raw.c_cc[5] = 3; // VTIME: 0.3 s
    raw.c_cc[6] = 0; // VMIN
    tcsetattr(STDIN, &raw);
    let start = monotonic_time();
    let mut b = [0u8; 4];
    let n = read(STDIN, &mut b);
    let end = monotonic_time();
    tcsetattr(STDIN, &saved);
    let ms = (end.0 * 1000 + end.1 / 1_000_000) - (start.0 * 1000 + start.1 / 1_000_000);
    check!(n == 0); // nothing typed: timed out
    check!(ms >= 250 && ms < 2000);
    check!(tcgetattr(STDIN).map(|t| t.c_cc[6]) == Ok(1)); // VMIN back to 1
    0
}

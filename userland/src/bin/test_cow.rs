// Copyright (c) 2026 tre4surehunter9
//! Test program for copy-on-write fork. Exits 0, or the line number of the
//! first failed check.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec;
use userlib::*;

userlib::entry!(main);

macro_rules! check {
    ($cond:expr) => {
        if !$cond {
            eprintln!("test_cow: check failed at line {}: {}", line!(), stringify!($cond));
            return line!() as i32;
        }
    };
}

/// Free memory in KiB, from /proc/meminfo.
fn free_kb() -> u64 {
    let text = userlib::io::read_text("/proc/meminfo").unwrap_or_default();
    text.lines()
        .find(|l| l.starts_with("MemFree:"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

const SIZE: usize = 4 * 1024 * 1024;

fn main(_args: Args) -> i32 {
    // 4 MiB of heap, every page touched.
    let mut buf = vec![0u8; SIZE];
    for (i, b) in buf.iter_mut().enumerate() {
        *b = (i % 251) as u8;
    }
    let Ok((r, w)) = pipe() else { return line!() as i32 };
    let before = free_kb();
    let pid = fork();
    if pid == 0 {
        // ---- the child ----
        close(w);
        // The fork shared the 4 MiB instead of copying it.
        let after_fork = free_kb();
        if before.saturating_sub(after_fork) > 1024 {
            eprintln!("test_cow: fork used {} KiB", before - after_fork);
            exit(10);
        }
        // Same data as the parent...
        if buf.iter().enumerate().any(|(i, &b)| b != (i % 251) as u8) {
            exit(11);
        }
        // ...until it writes: then each written page is copied.
        for i in (0..SIZE).step_by(4096) {
            buf[i] = 0xEE;
        }
        let after_write = free_kb();
        if before.saturating_sub(after_write) < 3 * 1024 {
            eprintln!("test_cow: writing 4 MiB used only {} KiB", before - after_write);
            exit(12);
        }
        // A read() into a page that's still shared (the kernel writes it).
        let mut small = vec![7u8; 8192];
        let fresh = fork(); // share `small` with a grandchild, then read into it
        if fresh == 0 {
            exit(if small[0] == 7 { 0 } else { 1 });
        }
        let n = read(r, &mut small[100..110]);
        let mut st = 0;
        waitpid(fresh as i32, &mut st, 0);
        if n != 5 || &small[100..105] != b"hello" || wexitstatus(st) != 0 {
            exit(13);
        }
        exit(0);
    }
    // ---- the parent ----
    close(r);
    write(w, b"hello");
    close(w);
    let mut status = 0;
    check!(waitpid(pid as i32, &mut status, 0) == pid);
    check!(wifexited(status));
    check!(wexitstatus(status) == 0);
    // The child's writes didn't touch our copy.
    check!(buf.iter().enumerate().all(|(i, &b)| b == (i % 251) as u8));
    // And we can still write ours (the frames are ours alone again).
    for i in (0..SIZE).step_by(4096) {
        buf[i] = 1;
    }
    check!(buf[4096] == 1 && buf[1] == 1);
    // All the child's pages came back.
    let end = free_kb();
    check!(before.saturating_sub(end) < 512);
    // Many forks of a big process are cheap and correct.
    for round in 0..20 {
        let pid = fork();
        if pid == 0 {
            exit(if buf[4096 * round] == 1 { 0 } else { 1 });
        }
        check!(waitpid(pid as i32, &mut status, 0) == pid && wexitstatus(status) == 0);
    }
    0
}

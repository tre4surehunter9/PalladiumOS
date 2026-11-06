// Copyright (c) 2026 tre4surehunter9
//! Test program for brk/heap, dup/dup2 and the environment. Exits 0 on
//! success, or the line number of the first failed check.
//!   test_misc heap | dup | env
#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use userlib::*;

userlib::entry!(main);

macro_rules! check {
    ($cond:expr) => {
        if !$cond {
            eprintln!("test_misc: check failed at line {}: {}", line!(), stringify!($cond));
            return line!() as i32;
        }
    };
}

const HEAP_BASE: u64 = 0x1000_0000_0000;

fn main(args: Args) -> i32 {
    match args.get(1) {
        Some("heap") => heap(),
        Some("dup") => dup_test(),
        Some("env") => env_test(),
        _ => 98,
    }
}

fn heap() -> i32 {
    // ---- raw brk ----
    let start = brk(0);
    check!(start >= HEAP_BASE);
    check!(brk(start + 10_000) == start + 10_000);
    unsafe {
        let p = start as *mut u8;
        p.write(1);
        p.add(9_999).write(2); // the whole range is usable
        check!(p.read() == 1 && p.add(9_999).read() == 2);
    }
    check!(brk(start) == start); // shrink back
    check!(brk(1) == start); // below the base: refused, unchanged
    check!(brk(HEAP_BASE + (1 << 40)) == start); // too big: refused
    // Memory that was released and handed out again starts zeroed.
    check!(brk(start + 4096) == start + 4096);
    check!(unsafe { (start as *const u8).read() } == 0);
    check!(brk(start) == start);

    // ---- the global allocator ----
    let mut v: Vec<u64> = Vec::new();
    for i in 0..100_000 {
        v.push(i);
    }
    check!(v.iter().sum::<u64>() == 99_999 * 100_000 / 2);
    let big = Box::new([7u8; 300_000]); // bigger than one growth step
    check!(big.iter().all(|&b| b == 7));
    drop(big);
    let mut map = BTreeMap::new();
    for i in 0..1000 {
        map.insert(i, alloc::format!("value {}", i));
    }
    check!(map.get(&500).map(String::as_str) == Some("value 500"));
    // Freed memory is reused rather than growing forever.
    let before = brk(0);
    for _ in 0..1000 {
        let s: String = core::iter::repeat('x').take(1000).collect();
        core::hint::black_box(&s);
    }
    check!(brk(0) == before);

    // ---- the heap is copied by fork, not shared ----
    let mut shared = Box::new(1u32);
    let pid = fork();
    if pid == 0 {
        *shared = 2;
        let v: Vec<u8> = Vec::with_capacity(100_000); // child heap can grow on its own
        core::hint::black_box(&v);
        exit(if *shared == 2 { 0 } else { 1 });
    }
    let mut status = 0;
    check!(waitpid(pid as i32, &mut status, 0) == pid && wexitstatus(status) == 0);
    check!(*shared == 1);
    *shared = 3;
    check!(*shared == 3);
    0
}

fn dup_test() -> i32 {
    let fd = open("/dup_test.txt", O_CREAT | O_WRONLY | O_TRUNC) as i32;
    check!(fd == 3);
    let copy = dup(fd) as i32;
    check!(copy == 4);
    check!(write(copy, b"via dup ") == 8);
    close(copy);

    // Redirect stdout into the file, then put it back.
    let saved = dup(STDOUT) as i32;
    check!(saved >= 0);
    check!(dup2(fd, STDOUT) == STDOUT as isize);
    print!("via stdout");
    check!(dup2(saved, STDOUT) == STDOUT as isize);
    close(saved);
    close(fd);
    check!(fstat(STDOUT).is_ok_and(|s| s.st_mode & S_IFMT == S_IFCHR)); // console again

    let mut buf = [0u8; 64];
    let fd = open("/dup_test.txt", O_RDONLY) as i32;
    let n = read(fd, &mut buf);
    close(fd);
    // dup'ed fds have separate offsets here (not shared as in POSIX), so the
    // second write lands at offset 0 of its own copy... check what we got:
    check!(n > 0);
    check!(&buf[..n as usize] == b"via stdout" || &buf[..n as usize] == b"via dup via stdout");

    check!(dup(99) == -EBADF as isize);
    check!(dup2(99, 5) == -EBADF as isize);
    check!(dup2(STDOUT, 1000) == -EBADF as isize);
    check!(dup2(STDOUT, STDOUT) == STDOUT as isize);
    check!(unlink("/dup_test.txt") == 0);
    0
}

fn env_test() -> i32 {
    // Run with FOO=bar in the environment.
    check!(getenv("FOO") == Some("bar"));
    check!(getenv("PATH").is_some());
    check!(getenv("NOT_SET").is_none());
    check!(getenv("FO").is_none()); // prefix must match the whole name
    check!(environ().any(|kv| kv == "FOO=bar"));
    0
}

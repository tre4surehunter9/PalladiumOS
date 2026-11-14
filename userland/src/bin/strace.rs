// Copyright (c) 2026 tre4surehunter9
//! strace: show the system calls a program makes, one line each, as it
//! makes them: `strace ls /tmp`, `strace -o trace.txt cmd args`.
//!
//! The kernel does the tracing (src/strace.rs): this asks it to trace this
//! process (and its children) to stderr or the -o file, then becomes the
//! program (execve: the first line is that execve).
#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use userlib::{eprintln, environ};

userlib::entry!(main);

const SYS_PRCTL: u64 = 157;
/// prctl(PR_PALLADIUM_TRACE, fd): the kernel's tracing.
const PR_PALLADIUM_TRACE: u64 = 0x5041_4C54;

fn main(args: userlib::Args) -> i32 {
    let mut i = 1;
    let mut out_fd = 2; // stderr
    if args.get(1) == Some("-o") {
        let Some(path) = args.get(2) else {
            eprintln!("strace: -o needs a file");
            return 1;
        };
        // (Close-on-exec: gone from the program, without a close() of
        // its own in the trace; the kernel keeps its own handle.)
        let fd = userlib::open(path, userlib::O_WRONLY | userlib::O_CREAT | userlib::O_TRUNC | userlib::O_CLOEXEC);
        if fd < 0 {
            eprintln!("strace: can't open {}: error {}", path, -fd);
            return 1;
        }
        out_fd = fd as u64;
        i = 3;
    }
    let Some(cmd) = args.get(i) else {
        eprintln!("usage: strace [-o FILE] COMMAND [ARGS...]");
        return 1;
    };
    let argv: Vec<&str> = (i..args.count()).filter_map(|n| args.get(n)).collect();
    let envp: Vec<&str> = environ().collect();
    // The program: a path, or found in $PATH (before tracing starts, as
    // Linux's strace does: no failed execve of other directories shown).
    let program = if cmd.contains('/') {
        String::from(cmd)
    } else {
        let path = environ().find_map(|v| v.strip_prefix("PATH=")).unwrap_or("/bin:/usr/bin");
        let found = path.split(':').map(|dir| {
            let mut full = String::from(dir);
            full.push('/');
            full.push_str(cmd);
            full
        }).find(|full| {
            let fd = userlib::open(full, userlib::O_RDONLY);
            if fd >= 0 {
                let _ = userlib::close(fd as i32);
            }
            fd >= 0
        });
        match found {
            Some(f) => f,
            None => {
                eprintln!("strace: {}: command not found", cmd);
                return 127;
            }
        }
    };
    let r = unsafe { userlib::sys::syscall3(SYS_PRCTL, PR_PALLADIUM_TRACE, out_fd, 0) };
    if r < 0 {
        eprintln!("strace: the kernel can't trace (error {})", -r);
        return 1;
    }
    let e = userlib::execve(&program, &argv, &envp);
    eprintln!("strace: {}: error {}", program, -e);
    127
}

// Copyright (c) 2026 tre4surehunter9
//! ps: list processes (from /proc).
#![no_std]
#![no_main]

use userlib::procinfo::{all, format_time};
use userlib::{eprintln, println};

userlib::entry!(main);

fn main(_args: userlib::Args) -> i32 {
    let procs = all();
    if procs.is_empty() {
        eprintln!("ps: no processes found (is /proc mounted?)");
        return 1;
    }
    println!("  PID  PPID S     TIME    RSS CMD");
    for p in procs {
        println!("{:5} {:5} {} {:>8} {:5}K {}", p.pid, p.ppid, p.state, format_time(p.utime), p.rss_kb, p.cmdline);
    }
    0
}

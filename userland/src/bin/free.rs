// Copyright (c) 2026 tre4surehunter9
//! free: memory use (from /proc/meminfo).  free [-h | -m | -k]
#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use userlib::procinfo::meminfo;
use userlib::println;

userlib::entry!(main);

fn main(args: userlib::Args) -> i32 {
    let unit = args.get(1).unwrap_or("-k");
    let show = |kb: u64| -> String {
        match unit {
            "-m" => format!("{}", kb / 1024),
            "-h" if kb >= 1024 * 1024 => format!("{:.1}G", kb as f64 / 1048576.0),
            "-h" if kb >= 1024 => format!("{:.1}M", kb as f64 / 1024.0),
            "-h" => format!("{}K", kb),
            _ => format!("{}", kb),
        }
    };
    let total = meminfo("MemTotal");
    let free = meminfo("MemFree");
    let available = meminfo("MemAvailable");
    println!("{:>15} {:>11} {:>11} {:>11}", "total", "used", "free", "available");
    println!("Mem: {:>10} {:>11} {:>11} {:>11}", show(total), show(total - free), show(free), show(available));
    0
}

// Copyright (c) 2026 tre4surehunter9
//! df: disk space of the filesystems.  df [-h] [PATH...]
#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use userlib::*;

userlib::entry!(main);

fn size(bytes: u64, human: bool) -> String {
    if !human {
        return format!("{}", bytes / 1024);
    }
    let units = ["B", "K", "M", "G"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u + 1 < units.len() {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 { format!("{}B", bytes) } else { format!("{:.1}{}", v, units[u]) }
}

fn main(args: Args) -> i32 {
    let human = args.skip(1).any(|a| a == "-h");
    let mut paths: Vec<&str> = args.skip(1).filter(|a| !a.starts_with('-')).collect();
    if paths.is_empty() {
        paths = alloc::vec!["/", "/bin", "/tmp", "/dev"];
    }
    println!("{:<8} {:>10} {:>10} {:>10} {:>5}  Mounted on", "Type", if human { "Size" } else { "1K-blocks" }, "Used", "Avail", "Use%");
    let mut status = 0;
    for path in paths {
        let st = match statfs(path) {
            Ok(st) => st,
            Err(e) => {
                eprintln!("df: {}: {}", path, strerror(-e));
                status = 1;
                continue;
            }
        };
        let fs_type = match st.f_type {
            0xEF53 => "ext2",
            0x01021994 => "memfs",
            _ => "devfs",
        };
        let bs = st.f_bsize.max(1) as u64;
        let (total, free) = (st.f_blocks * bs, st.f_bfree * bs);
        let used = total.saturating_sub(free);
        // In-memory filesystems have no fixed size: just show what's used.
        let sized = fs_type == "ext2";
        let pct = if sized && total > 0 { format!("{}%", (used * 100).div_ceil(total)) } else { String::from("-") };
        let (total_s, free_s) = if sized { (size(total, human), size(free, human)) } else { (String::from("-"), String::from("-")) };
        println!("{:<8} {:>10} {:>10} {:>10} {:>5}  {}", fs_type, total_s, size(used, human), free_s, pct, path);
    }
    status
}

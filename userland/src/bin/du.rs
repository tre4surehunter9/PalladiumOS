// Copyright (c) 2026 tre4surehunter9
//! du: disk usage of files and directories.
//!   du [-s] [-h] [PATH...]   -s: only totals, -h: human-readable sizes
#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use userlib::*;

userlib::entry!(main);

fn size(blocks512: u64, human: bool) -> String {
    let bytes = blocks512 * 512;
    if !human {
        return format!("{}", bytes.div_ceil(1024));
    }
    if bytes < 1024 {
        return format!("{}", bytes);
    }
    let units = ["K", "M", "G"];
    let mut v = bytes as f64 / 1024.0;
    let mut u = 0;
    while v >= 1024.0 && u + 1 < units.len() {
        v /= 1024.0;
        u += 1;
    }
    format!("{:.1}{}", v, units[u])
}

/// Total 512-byte blocks used under `path`. Prints each directory inside
/// it (not `path` itself) unless `summary`.
fn usage(path: &str, summary: bool, human: bool) -> u64 {
    let Ok(st) = lstat(path) else {
        eprintln!("du: cannot access '{}'", path);
        return 0;
    };
    // Files in memory don't report blocks: fall back to their size.
    let own = if st.st_blocks > 0 { st.st_blocks as u64 } else { (st.st_size as u64).div_ceil(512) };
    if !st.is_dir() {
        return own;
    }
    let mut total = own;
    let fd = open(path, O_RDONLY | O_DIRECTORY);
    if fd >= 0 {
        let mut names = Vec::new();
        let _ = for_each_entry(fd as i32, |e| {
            if e.name != "." && e.name != ".." {
                names.push((e.name.to_string(), e.d_type == DT_DIR));
            }
        });
        close(fd as i32);
        for (n, is_dir) in names {
            let child = if path.ends_with('/') { format!("{}{}", path, n) } else { format!("{}/{}", path, n) };
            let used = usage(&child, summary, human);
            if is_dir && !summary {
                println!("{}\t{}", size(used, human), child);
            }
            total += used;
        }
    }
    total
}

fn main(args: Args) -> i32 {
    let summary = args.skip(1).any(|a| a.starts_with('-') && a.contains('s'));
    let human = args.skip(1).any(|a| a.starts_with('-') && a.contains('h'));
    let mut paths: Vec<&str> = args.skip(1).filter(|a| !a.starts_with('-')).collect();
    if paths.is_empty() {
        paths.push(".");
    }
    for p in paths {
        let total = usage(p, summary, human);
        println!("{}\t{}", size(total, human), p);
    }
    0
}

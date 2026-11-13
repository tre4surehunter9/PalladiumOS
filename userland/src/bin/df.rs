// Copyright (c) 2026 tre4surehunter9
//! df: disk space of the filesystems.  df [-h] [PATH...]
//! Without paths: everything mounted (from /proc/mounts), but /proc.
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

/// The mounts: (device, mount point), from /proc/mounts.
fn mounts() -> Vec<(String, String)> {
    let fd = open("/proc/mounts", O_RDONLY);
    if fd < 0 {
        return Vec::new();
    }
    let mut text = Vec::new();
    let mut buf = [0u8; 1024];
    loop {
        let n = read(fd as i32, &mut buf);
        if n <= 0 {
            break;
        }
        text.extend_from_slice(&buf[..n as usize]);
    }
    close(fd as i32);
    String::from_utf8_lossy(&text)
        .lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            Some((String::from(f.next()?), String::from(f.next()?)))
        })
        .collect()
}

fn main(args: Args) -> i32 {
    let human = args.skip(1).any(|a| a == "-h");
    let given: Vec<&str> = args.skip(1).filter(|a| !a.starts_with('-')).collect();
    let table = mounts();
    let paths: Vec<String> = if given.is_empty() {
        table.iter().filter(|(_, m)| m != "/proc").map(|(_, m)| m.clone()).collect()
    } else {
        given.iter().map(|p| String::from(*p)).collect()
    };
    // The device of the mount a path is on (the longest mount point that's a prefix).
    let device_of = |path: &str| -> String {
        table
            .iter()
            .filter(|(_, m)| path == m || path.starts_with(m.as_str()) && (m == "/" || path[m.len()..].starts_with('/')))
            .max_by_key(|(_, m)| m.len())
            .map_or_else(|| String::from("-"), |(d, _)| d.clone())
    };
    println!(
        "{:<12} {:<8} {:>10} {:>10} {:>10} {:>5}  Mounted on",
        "Filesystem",
        "Type",
        if human { "Size" } else { "1K-blocks" },
        "Used",
        "Avail",
        "Use%"
    );
    let mut status = 0;
    for path in paths.iter().map(String::as_str) {
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
            0x01021997 => "9p",
            0x62656572 => "sysfs",
            _ => "devfs",
        };
        let bs = st.f_bsize.max(1) as u64;
        let (total, free) = (st.f_blocks * bs, st.f_bfree * bs);
        let used = total.saturating_sub(free);
        // In-memory filesystems have no fixed size: just show what's used.
        let sized = fs_type == "ext2" || fs_type == "9p";
        let pct = if sized && total > 0 { format!("{}%", (used * 100).div_ceil(total)) } else { String::from("-") };
        let (total_s, free_s) = if sized { (size(total, human), size(free, human)) } else { (String::from("-"), String::from("-")) };
        println!("{:<12} {:<8} {:>10} {:>10} {:>10} {:>5}  {}", device_of(path), fs_type, total_s, size(used, human), free_s, pct, path);
    }
    status
}

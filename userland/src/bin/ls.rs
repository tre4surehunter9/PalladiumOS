// Copyright (c) 2026 tre4surehunter9
//! ls: list directory contents.
//!   -a  include entries starting with '.' (including . and ..)
//!   -l  long format: type, size and name
#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use userlib::{close, eprintln, for_each_entry, open, print, println, stat, strerror, O_DIRECTORY, O_RDONLY, DT_DIR};

userlib::entry!(main);

/// "2026-11-06 07:02" (or blank padding for files without a time).
fn when(t: i64) -> alloc::string::String {
    if t <= 0 {
        return alloc::string::String::from("                ");
    }
    let (y, mo, d, h, mi, _) = userlib::civil_time(t as u64);
    alloc::format!("{:04}-{:02}-{:02} {:02}:{:02}", y, mo, d, h, mi)
}

struct Options {
    all: bool,
    long: bool,
}

/// Join dir and name into `buf` and return it as &str (for stat in -l mode).
fn join<'a>(buf: &'a mut [u8; 512], dir: &str, name: &str) -> &'a str {
    let mut len = 0;
    for part in [dir.as_bytes(), b"/", name.as_bytes()] {
        let n = part.len().min(buf.len() - len);
        buf[len..len + n].copy_from_slice(&part[..n]);
        len += n;
    }
    core::str::from_utf8(&buf[..len]).unwrap_or("")
}

fn list(path: &str, opts: &Options) -> i32 {
    // A file argument is just printed, like POSIX ls.
    match stat(path) {
        Ok(st) if !st.is_dir() => {
            if opts.long {
                println!("-  {:>8}  {}", st.st_size, path);
            } else {
                println!("{}", path);
            }
            return 0;
        }
        Ok(_) => {}
        Err(e) => {
            eprintln!("ls: cannot access '{}': {}", path, strerror(-e));
            return 2;
        }
    }

    let fd = open(path, O_RDONLY | O_DIRECTORY);
    if fd < 0 {
        eprintln!("ls: cannot open '{}': {}", path, strerror(-fd));
        return 2;
    }
    let fd = fd as i32;
    // Collect, then sort by name like POSIX ls.
    let mut entries: Vec<(String, bool)> = Vec::new();
    let result = for_each_entry(fd, |entry| {
        if opts.all || !entry.name.starts_with('.') {
            entries.push((entry.name.to_string(), entry.d_type == DT_DIR));
        }
    });
    close(fd);
    entries.sort();
    for (name, is_dir) in &entries {
        if opts.long {
            let mut buf = [0u8; 512];
            let st = stat(join(&mut buf, path, name));
            let size = st.as_ref().map(|s| s.st_size).unwrap_or(0);
            let mtime = st.map(|s| s.st_mtime).unwrap_or(0);
            println!("{}  {:>8}  {}  {}", if *is_dir { 'd' } else { '-' }, size, when(mtime), name);
        } else if *is_dir {
            println!("{}/", name);
        } else {
            println!("{}", name);
        }
    }
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("ls: {}: {}", path, strerror(-e));
            2
        }
    }
}

fn main(args: userlib::Args) -> i32 {
    let mut opts = Options { all: false, long: false };
    let mut paths = 0;
    for arg in args.skip(1) {
        if let Some(flags) = arg.strip_prefix('-').filter(|f| !f.is_empty()) {
            for c in flags.chars() {
                match c {
                    'a' => opts.all = true,
                    'l' => opts.long = true,
                    _ => {
                        eprintln!("ls: invalid option -- '{}'", c);
                        return 2;
                    }
                }
            }
        } else {
            paths += 1;
        }
    }

    if paths == 0 {
        return list(".", &opts);
    }
    let mut status = 0;
    for path in args.skip(1).filter(|a| !a.starts_with('-') || *a == "-") {
        if paths > 1 {
            print!("{}:\n", path);
        }
        status = status.max(list(path, &opts));
    }
    status
}

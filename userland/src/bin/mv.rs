// Copyright (c) 2026 tre4surehunter9
//! mv: move or rename files.
//!   mv [-f] SOURCE DEST         rename SOURCE to DEST
//!   mv [-f] SOURCE... DIRECTORY move into DIRECTORY
//! Across filesystems (e.g. /tmp to /), it copies and then removes.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use userlib::*;

userlib::entry!(main);

fn basename(p: &str) -> &str {
    p.trim_end_matches('/').rsplit('/').next().unwrap_or(p)
}

fn main(args: Args) -> i32 {
    let args: Vec<&str> = args.skip(1).filter(|a| *a != "-f").collect();
    if args.len() < 2 {
        eprintln!("usage: mv SOURCE... DEST");
        return 1;
    }
    let (sources, dest) = args.split_at(args.len() - 1);
    let dest = dest[0];
    let dest_is_dir = stat(dest).is_ok_and(|s| s.is_dir());
    if sources.len() > 1 && !dest_is_dir {
        eprintln!("mv: target '{}' is not a directory", dest);
        return 1;
    }
    let mut status = 0;
    for src in sources {
        let target: String = if dest_is_dir { format!("{}/{}", dest.trim_end_matches('/'), basename(src)) } else { dest.into() };
        let mut r = rename(src, &target);
        if r == -EXDEV {
            // Different filesystems: copy, then remove the original.
            r = match spawn_and_wait("/bin/cp", &["cp", "-r", src, &target]) {
                Ok(s) if wexitstatus(s) == 0 => match spawn_and_wait("/bin/rm", &["rm", "-r", src]) {
                    Ok(s) if wexitstatus(s) == 0 => 0,
                    _ => -EIO,
                },
                _ => -EIO,
            };
        }
        if r < 0 {
            eprintln!("mv: cannot move '{}' to '{}': {}", src, target, strerror(-r));
            status = 1;
        }
    }
    status
}

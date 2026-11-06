// Copyright (c) 2026 tre4surehunter9
//! ln: make links.
//!   ln [-s] [-f] TARGET LINK      LINK becomes a link to TARGET
//!   ln [-s] [-f] TARGET... DIR    links in DIR
//! Without -s it's a hard link (another name for the same file); with -s
//! a symbolic link (a small file holding the path TARGET).
#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use userlib::*;

userlib::entry!(main);

fn main(args: Args) -> i32 {
    let symbolic = args.skip(1).any(|a| a.starts_with('-') && a.contains('s'));
    let force = args.skip(1).any(|a| a.starts_with('-') && a.contains('f'));
    let paths: Vec<&str> = args.skip(1).filter(|a| !a.starts_with('-')).collect();
    if paths.len() < 2 {
        eprintln!("usage: ln [-s] [-f] TARGET LINK");
        return 1;
    }
    let (targets, dest) = paths.split_at(paths.len() - 1);
    let dest = dest[0];
    let into_dir = stat(dest).is_ok_and(|s| s.is_dir());
    let mut status = 0;
    for target in targets {
        let name: String = if into_dir {
            format!("{}/{}", dest.trim_end_matches('/'), target.trim_end_matches('/').rsplit('/').next().unwrap_or(target))
        } else {
            dest.into()
        };
        if force {
            unlink(&name);
        }
        let r = if symbolic { symlink(target, &name) } else { link(target, &name) };
        if r < 0 {
            eprintln!("ln: failed to create link '{}' -> '{}': {}", name, target, strerror(-r));
            status = 1;
        }
    }
    status
}

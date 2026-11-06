// Copyright (c) 2026 tre4surehunter9
//! mkdir: create directories. -p creates missing parents and ignores
//! directories that already exist.
#![no_std]
#![no_main]

use userlib::{eprintln, mkdir, stat, strerror, EEXIST};

userlib::entry!(main);

fn mkdir_parents(path: &str) -> isize {
    // Create every prefix ending at a '/', then the full path.
    for (i, c) in path.char_indices() {
        if c == '/' && i > 0 {
            let r = mkdir(&path[..i]);
            if r < 0 && r != -EEXIST {
                return r;
            }
        }
    }
    match mkdir(path) {
        r if r == -EEXIST && stat(path).is_ok_and(|s| s.is_dir()) => 0,
        r => r,
    }
}

fn main(args: userlib::Args) -> i32 {
    let parents = args.skip(1).any(|a| a == "-p");
    let mut status = 0;
    let mut any = false;
    for path in args.skip(1).filter(|a| *a != "-p") {
        any = true;
        let r = if parents { mkdir_parents(path) } else { mkdir(path) };
        if r < 0 {
            eprintln!("mkdir: cannot create directory '{}': {}", path, strerror(-r));
            status = 1;
        }
    }
    if !any {
        eprintln!("mkdir: missing operand");
        return 1;
    }
    status
}

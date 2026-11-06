// Copyright (c) 2026 tre4surehunter9
//! rm: remove files. -r removes directories and their contents,
//! -f ignores missing files.
#![no_std]
#![no_main]

use userlib::{close, eprintln, for_each_entry, open, rmdir, stat, strerror, unlink, ENOENT, O_DIRECTORY, O_RDONLY};

userlib::entry!(main);

const MAX_NAME: usize = 255;

/// Remove `path` and everything under it.
fn remove_tree(path: &str) -> isize {
    let st = match stat(path) {
        Ok(st) => st,
        Err(e) => return e,
    };
    if !st.is_dir() {
        return unlink(path);
    }

    // Repeatedly take the first entry, remove it, and start over. Removing
    // while iterating would shift the directory position, and this keeps
    // stack use small at every level of recursion.
    loop {
        let mut name = [0u8; MAX_NAME];
        let mut len = 0;
        let fd = open(path, O_RDONLY | O_DIRECTORY);
        if fd < 0 {
            return fd;
        }
        let r = for_each_entry(fd as i32, |e| {
            if len == 0 && e.name != "." && e.name != ".." && e.name.len() <= MAX_NAME {
                name[..e.name.len()].copy_from_slice(e.name.as_bytes());
                len = e.name.len();
            }
        });
        close(fd as i32);
        if let Err(e) = r {
            return e;
        }
        if len == 0 {
            break; // empty
        }
        let mut buf = [0u8; 1024];
        let child = join(&mut buf, path, core::str::from_utf8(&name[..len]).unwrap_or(""));
        let r = remove_tree(child);
        if r < 0 {
            return r;
        }
    }
    rmdir(path)
}

fn join<'a>(buf: &'a mut [u8; 1024], dir: &str, name: &str) -> &'a str {
    let mut len = 0;
    for part in [dir.as_bytes(), b"/", name.as_bytes()] {
        let n = part.len().min(buf.len() - len);
        buf[len..len + n].copy_from_slice(&part[..n]);
        len += n;
    }
    core::str::from_utf8(&buf[..len]).unwrap_or("")
}

fn main(args: userlib::Args) -> i32 {
    let mut recursive = false;
    let mut force = false;
    for arg in args.skip(1).filter(|a| a.starts_with('-') && a.len() > 1) {
        for c in arg[1..].chars() {
            match c {
                'r' | 'R' => recursive = true,
                'f' => force = true,
                _ => {
                    eprintln!("rm: invalid option -- '{}'", c);
                    return 1;
                }
            }
        }
    }

    let mut status = 0;
    let mut any = false;
    for path in args.skip(1).filter(|a| !a.starts_with('-') || a.len() == 1) {
        any = true;
        let r = if recursive { remove_tree(path) } else { unlink(path) };
        if r < 0 && !(force && r == -ENOENT) {
            eprintln!("rm: cannot remove '{}': {}", path, strerror(-r));
            status = 1;
        }
    }
    if !any && !force {
        eprintln!("rm: missing operand");
        return 1;
    }
    status
}

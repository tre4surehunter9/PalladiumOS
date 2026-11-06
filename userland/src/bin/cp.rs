// Copyright (c) 2026 tre4surehunter9
//! cp: copy files.
//!   cp [-r] SOURCE DEST         copy to DEST
//!   cp [-r] SOURCE... DIRECTORY copy into DIRECTORY
//! -r copies directories and everything in them. Permissions are kept.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use userlib::*;

userlib::entry!(main);

fn basename(p: &str) -> &str {
    p.trim_end_matches('/').rsplit('/').next().unwrap_or(p)
}

fn copy_file(src: &str, dst: &str, mode: u32) -> Result<(), isize> {
    let input = open(src, O_RDONLY);
    if input < 0 {
        return Err(input);
    }
    let output = open(dst, O_WRONLY | O_CREAT | O_TRUNC);
    if output < 0 {
        close(input as i32);
        return Err(output);
    }
    let mut buf = [0u8; 4096];
    let result = loop {
        let n = read(input as i32, &mut buf);
        if n < 0 {
            break Err(n);
        }
        if n == 0 {
            break Ok(());
        }
        if let Err(e) = write_all(output as i32, &buf[..n as usize]) {
            break Err(e);
        }
    };
    close(input as i32);
    close(output as i32);
    chmod(dst, mode & 0o7777);
    result
}

fn copy(src: &str, dst: &str, recursive: bool) -> Result<(), (String, isize)> {
    let st = lstat(src).map_err(|e| (src.to_string(), e))?;
    if st.is_symlink() {
        let mut buf = [0u8; 4096];
        let target = readlink(src, &mut buf).map_err(|e| (src.to_string(), e))?;
        let r = symlink(target, dst);
        return if r < 0 { Err((dst.to_string(), r)) } else { Ok(()) };
    }
    if !st.is_dir() {
        return copy_file(src, dst, st.st_mode).map_err(|e| (dst.to_string(), e));
    }
    if !recursive {
        return Err((src.to_string(), -EISDIR));
    }
    if format!("{}/", dst).starts_with(&format!("{}/", src.trim_end_matches('/'))) {
        return Err((dst.to_string(), -EINVAL)); // copying a directory into itself
    }
    let r = mkdir(dst);
    if r < 0 && !(r == -EEXIST && stat(dst).is_ok_and(|s| s.is_dir())) {
        return Err((dst.to_string(), r));
    }
    let fd = open(src, O_RDONLY | O_DIRECTORY);
    if fd < 0 {
        return Err((src.to_string(), fd));
    }
    let mut names = Vec::new();
    let _ = for_each_entry(fd as i32, |e| {
        if e.name != "." && e.name != ".." {
            names.push(e.name.to_string());
        }
    });
    close(fd as i32);
    for name in names {
        copy(&format!("{}/{}", src, name), &format!("{}/{}", dst, name), true)?;
    }
    chmod(dst, st.st_mode & 0o7777);
    Ok(())
}

fn main(args: Args) -> i32 {
    let recursive = args.skip(1).any(|a| a == "-r" || a == "-R" || a == "-a");
    let paths: Vec<&str> = args.skip(1).filter(|a| !a.starts_with('-')).collect();
    if paths.len() < 2 {
        eprintln!("usage: cp [-r] SOURCE... DEST");
        return 1;
    }
    let (sources, dest) = paths.split_at(paths.len() - 1);
    let dest = dest[0];
    let dest_is_dir = stat(dest).is_ok_and(|s| s.is_dir());
    if sources.len() > 1 && !dest_is_dir {
        eprintln!("cp: target '{}' is not a directory", dest);
        return 1;
    }
    let mut status = 0;
    for src in sources {
        let target = if dest_is_dir { format!("{}/{}", dest.trim_end_matches('/'), basename(src)) } else { dest.to_string() };
        if let Err((path, e)) = copy(src, &target, recursive) {
            if e == -EISDIR {
                eprintln!("cp: -r not specified; omitting directory '{}'", src);
            } else {
                eprintln!("cp: {}: {}", path, strerror(-e));
            }
            status = 1;
        }
    }
    status
}

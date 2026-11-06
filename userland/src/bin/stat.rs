// Copyright (c) 2026 tre4surehunter9
//! stat: show a file's details (type, size, permissions, inode, links, time).
#![no_std]
#![no_main]

use userlib::*;

userlib::entry!(main);

fn main(args: Args) -> i32 {
    let mut status = 0;
    for path in args.skip(1) {
        let st = match lstat(path) {
            Ok(st) => st,
            Err(e) => {
                eprintln!("stat: cannot stat '{}': {}", path, strerror(-e));
                status = 1;
                continue;
            }
        };
        let kind = match st.st_mode & S_IFMT {
            S_IFDIR => "directory",
            S_IFREG => if st.st_size == 0 { "regular empty file" } else { "regular file" },
            S_IFLNK => "symbolic link",
            S_IFCHR => "character device",
            S_IFIFO => "fifo",
            _ => "unknown",
        };
        print!("  File: {}", path);
        if st.is_symlink() {
            let mut buf = [0u8; 4096];
            if let Ok(t) = readlink(path, &mut buf) {
                print!(" -> {}", t);
            }
        }
        println!();
        println!("  Size: {:<10} Blocks: {:<6} {}", st.st_size, st.st_blocks, kind);
        println!("Device: {:<9} Inode: {:<7} Links: {}", st.st_dev, st.st_ino, st.st_nlink);
        println!("Access: ({:04o})  Uid: {}  Gid: {}", st.st_mode & 0o7777, st.st_uid, st.st_gid);
        let (y, mo, d, h, mi, s) = civil_time(st.st_mtime.max(0) as u64);
        println!("Modify: {:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC", y, mo, d, h, mi, s);
    }
    status
}

// Copyright (c) 2026 tre4surehunter9
//! Test program for rename, links, symlinks, chmod, umask, access, statfs,
//! utimensat and #! scripts. Exits 0, or the line of the first failed check.
//! Works under /tl (created and removed).
#![no_std]
#![no_main]

extern crate alloc;

use userlib::*;

userlib::entry!(main);

macro_rules! check {
    ($cond:expr) => {
        if !$cond {
            eprintln!("test_links: check failed at line {}: {}", line!(), stringify!($cond));
            return line!() as i32;
        }
    };
}

fn write_file(path: &str, data: &[u8]) {
    let fd = open(path, O_CREAT | O_WRONLY | O_TRUNC) as i32;
    write_all(fd, data).ok();
    close(fd);
}

fn read_file<'a>(path: &str, buf: &'a mut [u8]) -> &'a [u8] {
    let fd = open(path, O_RDONLY);
    if fd < 0 {
        return &[];
    }
    let n = read(fd as i32, buf);
    close(fd as i32);
    &buf[..n.max(0) as usize]
}

fn main(_args: Args) -> i32 {
    let mut buf = [0u8; 256];
    spawn_and_wait("/bin/rm", &["rm", "-rf", "/tl"]).ok();
    check!(mkdir("/tl") == 0);

    // ---- rename ----
    write_file("/tl/a", b"A");
    check!(rename("/tl/a", "/tl/b") == 0);
    check!(stat("/tl/a").err() == Some(-ENOENT));
    check!(read_file("/tl/b", &mut buf) == b"A");
    write_file("/tl/c", b"C");
    check!(rename("/tl/b", "/tl/c") == 0); // replaces c
    check!(read_file("/tl/c", &mut buf) == b"A");
    check!(rename("/tl/missing", "/tl/x") == -ENOENT);
    check!(mkdir("/tl/d1") == 0);
    check!(mkdir("/tl/d1/sub") == 0);
    check!(rename("/tl/d1", "/tl/d2") == 0); // a directory
    check!(stat("/tl/d2/sub").is_ok_and(|s| s.is_dir()));
    check!(rename("/tl/d2", "/tl/d2/sub/inside") == -EINVAL); // into itself
    check!(rename("/tl/c", "/tl/d2") == -EISDIR); // file over directory
    check!(rename("/tl/d2", "/tl/c") == -ENOTDIR); // directory over file
    check!(mkdir("/tl/full") == 0);
    write_file("/tl/full/x", b"");
    check!(rename("/tl/d2", "/tl/full") == -ENOTEMPTY);
    check!(mkdir("/tl/empty") == 0);
    check!(rename("/tl/d2", "/tl/empty") == 0); // over an empty directory
    check!(stat("/tl/empty/sub").is_ok());
    // moving a directory to another parent fixes its ".."
    check!(rename("/tl/empty/sub", "/tl/full/sub") == 0);
    check!(chdir("/tl/full/sub") == 0 && chdir("..") == 0);
    check!(getcwd(&mut buf) == Ok("/tl/full"));
    check!(chdir("/") == 0);
    check!(rename("/tl/c", "/tl/c") == 0); // same name: nothing happens
    // across filesystems: EXDEV (mv falls back to copying)
    if stat("/tmp").is_ok() {
        write_file("/tmp/tl-x", b"x");
        let r = rename("/tmp/tl-x", "/tl/x");
        let separate = statfs("/tmp").map(|f| f.f_type) != statfs("/tl").map(|f| f.f_type);
        check!(if separate { r == -EXDEV } else { r == 0 });
        unlink("/tmp/tl-x");
        unlink("/tl/x");
    }

    // ---- hard links ----
    write_file("/tl/orig", b"shared");
    let r = link("/tl/orig", "/tl/hard");
    // (the in-memory filesystem doesn't support hard links: EPERM)
    check!(r == 0 || r == -EPERM);
    if r == 0 {
        let (s1, s2) = (stat("/tl/orig").unwrap_or_default(), stat("/tl/hard").unwrap_or_default());
        check!(s1.st_ino == s2.st_ino && s1.st_nlink == 2);
        write_file("/tl/hard", b"changed");
        check!(read_file("/tl/orig", &mut buf) == b"changed");
        check!(unlink("/tl/orig") == 0);
        check!(stat("/tl/hard").is_ok_and(|s| s.st_nlink == 1));
        check!(read_file("/tl/hard", &mut buf) == b"changed");
        check!(link("/tl/hard", "/tl/hard") == -EEXIST);
    }
    check!(link("/tl/full", "/tl/dirlink") == -EPERM); // no links to directories
    check!(link("/tl/missing", "/tl/l") == -ENOENT);

    // ---- symlinks ----
    write_file("/tl/target", b"T");
    check!(symlink("target", "/tl/rel") == 0); // relative to the link's directory
    check!(symlink("/tl/target", "/tl/abs") == 0);
    check!(symlink("rel", "/tl/chain") == 0); // a link to a link
    check!(read_file("/tl/rel", &mut buf) == b"T");
    check!(read_file("/tl/abs", &mut buf) == b"T");
    check!(read_file("/tl/chain", &mut buf) == b"T");
    let mut lbuf = [0u8; 64];
    check!(readlink("/tl/rel", &mut lbuf) == Ok("target"));
    check!(readlink("/tl/target", &mut lbuf).err() == Some(-EINVAL)); // not a link
    check!(lstat("/tl/rel").is_ok_and(|s| s.is_symlink() && s.st_size == 6));
    check!(stat("/tl/rel").is_ok_and(|s| s.is_file()));
    check!(symlink("full", "/tl/dlink") == 0); // a link to a directory
    check!(stat("/tl/dlink/x").is_ok()); // followed in the middle of a path
    check!(chdir("/tl/dlink") == 0 && chdir("/") == 0);
    check!(symlink("nowhere", "/tl/dangling") == 0);
    check!(stat("/tl/dangling").err() == Some(-ENOENT));
    check!(lstat("/tl/dangling").is_ok());
    check!(symlink("loop2", "/tl/loop1") == 0 && symlink("loop1", "/tl/loop2") == 0);
    check!(stat("/tl/loop1").err() == Some(-ELOOP));
    check!(open("/tl/rel", O_RDONLY | 0o400000) == -ELOOP); // O_NOFOLLOW
    check!(symlink("x", "/tl/rel") == -EEXIST);
    // a long target is stored in a data block instead of the inode
    let long = "/tl/full/../full/../full/../full/../full/../full/../full/../full/x";
    check!(symlink(long, "/tl/long") == 0);
    let mut big = [0u8; 256];
    check!(readlink("/tl/long", &mut big) == Ok(long));
    check!(stat("/tl/long").is_ok());
    // unlink removes the link, not the target; rename moves the link itself
    check!(unlink("/tl/abs") == 0 && stat("/tl/target").is_ok());
    check!(rename("/tl/rel", "/tl/rel2") == 0 && lstat("/tl/rel2").is_ok_and(|s| s.is_symlink()));

    // ---- chmod, umask, access ----
    write_file("/tl/m", b"m");
    check!(stat("/tl/m").is_ok_and(|s| s.st_mode & 0o777 == 0o644)); // 0644 minus umask 022
    check!(chmod("/tl/m", 0o751) == 0);
    check!(stat("/tl/m").is_ok_and(|s| s.st_mode & 0o7777 == 0o751 && s.is_file()));
    check!(access("/tl/m", X_OK) == 0);
    check!(chmod("/tl/m", 0o644) == 0);
    check!(access("/tl/m", X_OK) == -EACCES as isize);
    check!(access("/tl/m", R_OK | W_OK) == 0); // root can read and write anything
    check!(access("/tl/nope", F_OK) == -ENOENT);
    let old = umask(0o077);
    check!(old == 0o022);
    write_file("/tl/private", b"p");
    check!(mkdir("/tl/pdir") == 0);
    check!(stat("/tl/private").is_ok_and(|s| s.st_mode & 0o777 == 0o600));
    check!(stat("/tl/pdir").is_ok_and(|s| s.st_mode & 0o777 == 0o700));
    umask(old);
    check!(chmod("/tl/missing", 0o644) == -ENOENT);

    // ---- execute permission and #! scripts ----
    write_file("/tl/script", b"#!/bin/sh\nexit 42\n");
    match spawn_and_wait("/tl/script", &["script"]) {
        Ok(s) => check!(wexitstatus(s) == 126 || wexitstatus(s) == 127), // not executable yet
        Err(_) => return line!() as i32,
    }
    check!(execve("/tl/script", &["script"], &[]) == -EACCES);
    chmod("/tl/script", 0o755);
    match spawn_and_wait("/tl/script", &["script"]) {
        Ok(s) => check!(wexitstatus(s) == 42),
        Err(_) => return line!() as i32,
    }
    write_file("/tl/args", b"#!/bin/sh -e\nexit $#\n"); // an argument on the #! line
    chmod("/tl/args", 0o755);
    match spawn_and_wait("/tl/args", &["args", "one", "two", "three"]) {
        Ok(s) => check!(wexitstatus(s) == 3),
        Err(_) => return line!() as i32,
    }
    write_file("/tl/badinterp", b"#!/no/such/shell\n");
    chmod("/tl/badinterp", 0o755);
    check!(execve("/tl/badinterp", &["x"], &[]) == -ENOENT);

    // ---- statfs, utimensat ----
    let fs = statfs("/tl");
    check!(fs.is_ok_and(|f| f.f_bsize > 0 && f.f_namelen == 255));
    check!(statfs("/tl/nope").err() == Some(-ENOENT));
    check!(touch_now("/tl/m") == 0);
    let now = unix_time() as i64;
    check!(stat("/tl/m").is_ok_and(|s| (s.st_mtime - now).abs() <= 2));
    check!(touch_now("/tl/missing") == -ENOENT);

    // ---- clean up ----
    match spawn_and_wait("/bin/rm", &["rm", "-r", "/tl"]) {
        Ok(s) => check!(wexitstatus(s) == 0),
        Err(_) => return line!() as i32,
    }
    check!(lstat("/tl").err() == Some(-ENOENT));
    0
}

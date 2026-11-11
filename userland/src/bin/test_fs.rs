// Copyright (c) 2026 tre4surehunter9
//! Test program for the file syscalls.
//!   test_fs all   - run every check; exit 0, or the line number of the first failed check
//!   test_fs leak  - open a file and exit without closing it; exit status = the fd
//!   test_fs chdir - chdir to /, exit 0
//!   test_fs write <path> <text> - create/overwrite <path> with <text>
#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use userlib::*;

userlib::entry!(main);

macro_rules! check {
    ($cond:expr) => {
        if !$cond {
            eprintln!("test_fs: check failed at line {}: {}", line!(), stringify!($cond));
            return line!() as i32;
        }
    };
}

fn read_all<'a>(fd: i32, buf: &'a mut [u8]) -> &'a [u8] {
    let mut len = 0;
    loop {
        let n = read(fd, &mut buf[len..]);
        if n <= 0 {
            return &buf[..len];
        }
        len += n as usize;
    }
}

fn main(args: Args) -> i32 {
    match args.get(1) {
        Some("all") => all(),
        Some("leak") => open("/leak.txt", O_CREAT | O_WRONLY) as i32,
        Some("chdir") => chdir("/") as i32,
        Some("write") => {
            let (Some(path), Some(text)) = (args.get(2), args.get(3)) else { return 99 };
            let fd = open(path, O_CREAT | O_WRONLY | O_TRUNC);
            if fd < 0 {
                return -fd as i32;
            }
            write(fd as i32, text.as_bytes());
            close(fd as i32);
            0
        }
        _ => 98,
    }
}

fn all() -> i32 {
    let mut buf = [0u8; 256];

    // Start clean (a previous failed run may have left things behind).
    let _ = unlink("/t/sub/f");
    let _ = rmdir("/t/sub");
    let _ = unlink("/t/a.txt");
    let _ = unlink("/t/b.txt");
    let _ = rmdir("/t");

    // ---- mkdir / rmdir ----
    check!(mkdir("/t") == 0);
    check!(mkdir("/t") == -EEXIST);
    check!(mkdir("/nope/x") == -ENOENT);
    check!(mkdir("/") == -EEXIST);

    // ---- open / write / read / close ----
    check!(open("/t/a.txt", O_RDONLY) == -ENOENT);
    let fd = open("/t/a.txt", O_CREAT | O_WRONLY) as i32;
    check!(fd == 3); // lowest free fd
    check!(write(fd, b"hello world") == 11);
    check!(read(fd, &mut buf) == -EBADF); // write-only
    check!(close(fd) == 0);
    check!(close(fd) == -EBADF);
    check!(close(1000) == -EBADF);

    let fd = open("/t/a.txt", O_RDONLY) as i32;
    check!(fd == 3);
    check!(read_all(fd, &mut buf) == b"hello world");
    check!(read(fd, &mut buf) == 0); // EOF
    check!(write(fd, b"x") == -EBADF); // read-only

    // ---- lseek ----
    check!(lseek(fd, 6, SEEK_SET) == 6);
    check!(read(fd, &mut buf[..5]) == 5);
    check!(&buf[..5] == b"world");
    check!(lseek(fd, 0, SEEK_END) == 11);
    check!(lseek(fd, -5, SEEK_CUR) == 6);
    check!(lseek(fd, -100, SEEK_SET) == -EINVAL);
    check!(lseek(fd, 0, 7) == -EINVAL);
    check!(lseek(STDOUT, 0, SEEK_SET) == -ESPIPE);
    close(fd);

    // ---- O_EXCL, O_APPEND, O_TRUNC, holes ----
    check!(open("/t/a.txt", O_CREAT | O_EXCL | O_WRONLY) == -EEXIST);
    let fd = open("/t/a.txt", O_WRONLY | O_APPEND) as i32;
    check!(write(fd, b"!") == 1);
    close(fd);
    check!(stat("/t/a.txt").map(|s| s.st_size) == Ok(12));

    let fd = open("/t/a.txt", O_RDWR | O_TRUNC) as i32;
    check!(fstat(fd).map(|s| s.st_size) == Ok(0));
    check!(lseek(fd, 4, SEEK_SET) == 4);
    check!(write(fd, b"z") == 1);
    check!(lseek(fd, 0, SEEK_SET) == 0);
    check!(read_all(fd, &mut buf) == b"\0\0\0\0z"); // hole reads as zeros
    close(fd);

    // ---- stat / fstat ----
    let st = stat("/t/a.txt");
    check!(st.as_ref().is_ok_and(|s| s.is_file() && s.st_size == 5));
    check!(stat("/t").is_ok_and(|s| s.is_dir()));
    check!(stat("/t/missing").err() == Some(-ENOENT));
    check!(fstat(STDOUT).is_ok_and(|s| s.st_mode & S_IFMT == S_IFCHR));
    check!(fstat(77).err() == Some(-EBADF));

    // ---- directories as files ----
    check!(open("/t", O_WRONLY) == -EISDIR);
    check!(open("/t/a.txt", O_RDONLY | O_DIRECTORY) == -ENOTDIR);
    let dfd = open("/t", O_RDONLY | O_DIRECTORY) as i32;
    check!(dfd >= 0);
    check!(read(dfd, &mut buf) == -EISDIR);
    check!(mkdir("/t/a.txt/x") == -ENOTDIR);

    // ---- getdents64 ----
    check!(mkdir("/t/sub") == 0);
    let (mut dot, mut dotdot, mut file, mut sub, mut count) = (false, false, false, false, 0);
    let r = for_each_entry(dfd, |e| {
        count += 1;
        match e.name {
            "." => dot = e.d_type == DT_DIR,
            ".." => dotdot = e.d_type == DT_DIR,
            "a.txt" => file = e.d_type == DT_REG,
            "sub" => sub = e.d_type == DT_DIR,
            _ => {}
        }
    });
    check!(r.is_ok());
    check!(dot && dotdot && file && sub && count == 4);
    check!(getdents64(dfd, &mut buf) == 0); // already at the end
    check!(lseek(dfd, 0, SEEK_SET) == 0); // rewind
    check!(getdents64(dfd, &mut buf[..8]) == -EINVAL); // too small for one entry
    check!(getdents64(dfd, &mut buf) > 0);
    check!(getdents64(3 + 40, &mut buf) == -EBADF);
    check!(getdents64(STDOUT, &mut buf) == -ENOTDIR);

    // ---- openat ----
    let fd = openat(dfd, "a.txt", O_RDONLY) as i32;
    check!(fd >= 0);
    close(fd);
    let fd = openat(AT_FDCWD, "/t/a.txt", O_RDONLY) as i32;
    check!(fd >= 0);
    close(fd);
    check!(openat(STDOUT, "a.txt", O_RDONLY) == -ENOTDIR);
    close(dfd);

    // ---- cwd ----
    check!(getcwd(&mut buf) == Ok("/"));
    check!(chdir("/t/sub") == 0);
    check!(getcwd(&mut buf) == Ok("/t/sub"));
    check!(chdir("..") == 0);
    check!(getcwd(&mut buf) == Ok("/t"));
    check!(getcwd(&mut buf[..2]).err() == Some(-ERANGE));
    check!(chdir("a.txt") == -ENOTDIR);
    check!(chdir("missing") == -ENOENT);
    // relative paths, "." and ".."
    let fd = open("./sub/../a.txt", O_RDONLY) as i32;
    check!(fd >= 0);
    close(fd);
    let fd = open("sub/f", O_CREAT | O_WRONLY) as i32;
    check!(fd >= 0);
    close(fd);
    check!(stat("/t/sub/f").is_ok());
    check!(chdir("/") == 0);

    // ---- unlink / rmdir ----
    check!(rmdir("/t/sub") == -ENOTEMPTY);
    check!(rmdir("/t/a.txt") == -ENOTDIR);
    check!(unlink("/t/sub") == -EISDIR);
    check!(unlink("/t/missing") == -ENOENT);
    check!(unlink("/t/sub/f") == 0);
    check!(rmdir("/t/sub") == 0);
    check!(rmdir("/") == -EBUSY);

    // An unlinked file stays readable through an open fd (POSIX).
    let fd = open("/t/a.txt", O_RDONLY) as i32;
    check!(unlink("/t/a.txt") == 0);
    check!(open("/t/a.txt", O_RDONLY) == -ENOENT);
    check!(read_all(fd, &mut buf).len() == 5);
    close(fd);

    // ---- limits and bad input ----
    let mut fds = Vec::new();
    loop {
        let fd = open("/t", O_RDONLY);
        if fd < 0 {
            check!(fd == -EMFILE);
            break;
        }
        fds.push(fd as i32);
    }
    let opened = fds.len();
    check!(opened == 1024 - 3); // 0/1/2 are taken
    for &fd in &fds[..opened] {
        close(fd);
    }
    // kernel pointer as a path -> EFAULT
    check!(unsafe { sys::syscall3(sys::OPEN, 0xffff_8000_0000_0000, O_RDONLY, 0) } == -EFAULT as i64);
    check!(open("", O_RDONLY) == -ENOENT);
    check!(open("/t/x", 3) == -EINVAL); // bad access mode

    check!(rmdir("/t") == 0);
    0
}

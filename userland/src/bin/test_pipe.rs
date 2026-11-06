// Copyright (c) 2026 tre4surehunter9
//! Test program for pipes, shared open files, close-on-exec and fcntl.
//! Exits 0 on success, or the line number of the first failed check.
//!   test_pipe all          - the checks
//!   test_pipe fdcheck N    - exit 0 if fd N is closed, 1 if open (for exec tests)
#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;
use userlib::*;

userlib::entry!(main);

macro_rules! check {
    ($cond:expr) => {
        if !$cond {
            eprintln!("test_pipe: check failed at line {}: {}", line!(), stringify!($cond));
            return line!() as i32;
        }
    };
}

fn main(args: Args) -> i32 {
    match args.get(1) {
        Some("all") => all(),
        Some("fdcheck") => {
            let fd: i32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(-1);
            if fstat(fd).is_err() { 0 } else { 1 }
        }
        _ => 98,
    }
}

fn all() -> i32 {
    let mut buf = [0u8; 64];
    let mut status = 0;

    // ---- basics ----
    let Ok((r, w)) = pipe() else { return line!() as i32 };
    check!(r == 3 && w == 4);
    check!(write(w, b"hello") == 5);
    check!(read(r, &mut buf) == 5 && &buf[..5] == b"hello");
    check!(read(w, &mut buf) == -EBADF); // wrong end
    check!(write(r, b"x") == -EBADF);
    check!(lseek(r, 0, SEEK_SET) == -ESPIPE);
    check!(fstat(r).is_ok_and(|s| s.st_mode & S_IFMT == 0o010000)); // S_IFIFO
    check!(!isatty(r));
    // end-of-file once the write end is closed
    check!(write(w, b"last") == 4);
    close(w);
    check!(read(r, &mut buf) == 4);
    check!(read(r, &mut buf) == 0);
    close(r);

    // ---- writing with no readers: EPIPE (SIGPIPE is ignored here) ----
    set_signal(SIGPIPE, SIG_IGN);
    let Ok((r, w)) = pipe() else { return line!() as i32 };
    close(r);
    check!(write(w, b"x") == -EPIPE);
    close(w);
    set_signal(SIGPIPE, SIG_DFL);
    // ...and with the default action, SIGPIPE kills the writer
    let Ok((r, w)) = pipe() else { return line!() as i32 };
    close(r);
    let pid = fork();
    if pid == 0 {
        write(w, b"x");
        exit(0); // not reached
    }
    close(w);
    check!(waitpid(pid as i32, &mut status, 0) == pid && wifsignaled(status) && wtermsig(status) == SIGPIPE);

    // ---- a big transfer through a pipe between processes ----
    // 100 KiB is several times the pipe's capacity, so both sides block.
    const TOTAL: usize = 100 * 1024;
    let Ok((r, w)) = pipe() else { return line!() as i32 };
    let pid = fork();
    if pid == 0 {
        close(r);
        let chunk: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
        let mut sent = 0;
        while sent < TOTAL {
            let n = (TOTAL - sent).min(chunk.len());
            if write_all(w, &chunk[..n]).is_err() {
                exit(1);
            }
            sent += n;
        }
        exit(0);
    }
    close(w);
    let mut received = 0usize;
    let mut ok = true;
    let mut big = vec![0u8; 3000];
    loop {
        let n = read(r, &mut big);
        if n <= 0 {
            break;
        }
        for (i, &b) in big[..n as usize].iter().enumerate() {
            if b != (((received + i) % 1000) % 251) as u8 {
                ok = false;
            }
        }
        received += n as usize;
    }
    close(r);
    check!(ok && received == TOTAL);
    check!(waitpid(pid as i32, &mut status, 0) == pid && wexitstatus(status) == 0);

    // ---- a reader blocked on an empty pipe wakes when the writer exits ----
    let Ok((r, w)) = pipe() else { return line!() as i32 };
    let pid = fork();
    if pid == 0 {
        close(w);
        let n = read(r, &mut buf); // blocks until the parent closes its end
        exit(if n == 0 { 0 } else { 1 });
    }
    close(r);
    sleep_ms(100);
    close(w);
    check!(waitpid(pid as i32, &mut status, 0) == pid && wexitstatus(status) == 0);

    // ---- fork shares the file offset (POSIX open file descriptions) ----
    let fd = open("/pipe_off.txt", O_CREAT | O_WRONLY | O_TRUNC) as i32;
    check!(fd >= 0);
    check!(write(fd, b"one ") == 4);
    let pid = fork();
    if pid == 0 {
        write(fd, b"two "); // continues after "one ", doesn't overwrite it
        exit(0);
    }
    waitpid(pid as i32, &mut status, 0);
    check!(write(fd, b"three") == 5); // and the parent continues after "two "
    close(fd);
    let fd = open("/pipe_off.txt", O_RDONLY) as i32;
    let n = read(fd, &mut buf);
    close(fd);
    check!(&buf[..n as usize] == b"one two three");

    // ---- dup shares the offset too ----
    let fd = open("/pipe_off.txt", O_RDONLY) as i32;
    let copy = dup(fd) as i32;
    check!(read(fd, &mut buf[..4]) == 4);
    check!(read(copy, &mut buf[..3]) == 3 && &buf[..3] == b"two");
    check!(lseek(fd, 0, SEEK_CUR) == 7);
    close(fd);
    close(copy);
    unlink("/pipe_off.txt");

    // ---- close-on-exec ----
    let Ok((r, w)) = pipe2(O_CLOEXEC) else { return line!() as i32 };
    check!(fcntl(r, F_GETFD, 0) == FD_CLOEXEC as isize);
    let plain = dup(r) as i32; // dup clears close-on-exec
    check!(fcntl(plain, F_GETFD, 0) == 0);
    let fdstr = |fd: i32| alloc::format!("{}", fd);
    // After exec, the close-on-exec fd is gone but the plain one survives.
    let rs = fdstr(r);
    match spawn_and_wait("/bin/test_pipe", &["test_pipe", "fdcheck", &rs]) {
        Ok(s) => check!(wexitstatus(s) == 0),
        Err(_) => return line!() as i32,
    }
    let ps = fdstr(plain);
    match spawn_and_wait("/bin/test_pipe", &["test_pipe", "fdcheck", &ps]) {
        Ok(s) => check!(wexitstatus(s) == 1),
        Err(_) => return line!() as i32,
    }
    check!(fcntl(plain, F_SETFD, FD_CLOEXEC) == 0 && fcntl(plain, F_GETFD, 0) == 1);
    close(plain);
    close(r);
    close(w);

    // ---- fcntl ----
    check!(fcntl(STDOUT, F_GETFL, 0) == O_RDWR as isize);
    let d = fcntl(STDOUT, F_DUPFD, 10) as i32;
    check!(d == 10);
    close(d);
    check!(fcntl(99, F_GETFD, 0) == -EBADF as isize);
    check!(fcntl(STDOUT, 12345, 0) == -EINVAL as isize);

    // ---- pipe edge cases ----
    check!(pipe2(0o4000).err() == Some(-EINVAL)); // O_NONBLOCK isn't supported
    check!(unsafe { sys::syscall3(sys::PIPE2, 0xffff_8000_0000_0000, 0, 0) } == -EFAULT as i64);
    let mut ends = Vec::new();
    loop {
        match pipe() {
            Ok((r, w)) => ends.extend([r, w]),
            Err(e) => {
                check!(e == -EMFILE);
                break;
            }
        }
    }
    for fd in ends {
        close(fd);
    }
    0
}

// Copyright (c) 2026 tre4surehunter9
//! Test program for processes. Exits 0 on success, or the line number of
//! the first failed check.
//!   test_proc all         - fork/exec/wait checks
//!   test_proc exit <n>    - exit with status n
//!   test_proc spin <n>    - busy-loop n million iterations, then exit 0
//!   test_proc preempt     - needs preemption to finish (see below)
//!   test_proc orphan      - fork a child that outlives this process
//!   test_proc forkbomb    - fork until fork fails, then reap everything
#![no_std]
#![no_main]

use userlib::*;

userlib::entry!(main);

macro_rules! check {
    ($cond:expr) => {
        if !$cond {
            eprintln!("test_proc: check failed at line {}: {}", line!(), stringify!($cond));
            return line!() as i32;
        }
    };
}

static mut COUNTER: u64 = 0;

fn parse(s: Option<&str>) -> u64 {
    s.and_then(|s| s.parse().ok()).unwrap_or(0)
}

fn spin(millions: u64) {
    for i in 0..millions * 1_000_000 {
        core::hint::black_box(i);
    }
}

fn main(args: Args) -> i32 {
    match args.get(1) {
        Some("all") => all(),
        Some("exit") => parse(args.get(2)) as i32,
        Some("spin") => {
            spin(parse(args.get(2)));
            0
        }
        Some("preempt") => preempt(),
        Some("orphan") => {
            if fork() == 0 {
                spin(5);
                exit(0);
            }
            0 // exit without waiting: the child is handed to the kernel
        }
        Some("forkbomb") => forkbomb(),
        _ => 98,
    }
}

fn all() -> i32 {
    let me = getpid();
    check!(me > 0);

    // ---- fork returns twice ----
    let pid = fork();
    check!(pid >= 0);
    if pid == 0 {
        // child: has its own pid, and the parent's pid as ppid
        if getppid() != me || getpid() == me {
            exit(1);
        }
        exit(42);
    }
    let mut status = 0;
    check!(waitpid(pid as i32, &mut status, 0) == pid);
    check!(wifexited(status) && wexitstatus(status) == 42);
    // no children left
    check!(waitpid(-1, &mut status, 0) == -ECHILD as isize);
    check!(waitpid(pid as i32, &mut status, WNOHANG) == -ECHILD as isize);

    // ---- separate memory ----
    unsafe { COUNTER = 1 };
    let pid = fork();
    if pid == 0 {
        unsafe { COUNTER = 99 };
        let local = unsafe { COUNTER };
        exit(if local == 99 { 0 } else { 1 });
    }
    check!(waitpid(pid as i32, &mut status, 0) == pid && wexitstatus(status) == 0);
    check!(unsafe { COUNTER } == 1); // the child's write didn't reach us

    // ---- stack memory is copied too ----
    let mut on_stack = [7u8; 64];
    let pid = fork();
    if pid == 0 {
        on_stack[0] = 1;
        exit(on_stack.iter().map(|&b| b as i32).sum::<i32>() - 7 * 63);
    }
    on_stack[1] = 2;
    check!(waitpid(pid as i32, &mut status, 0) == pid && wexitstatus(status) == 1);
    check!(on_stack[0] == 7 && on_stack[1] == 2);

    // ---- several children, wait for any ----
    let mut seen = [false; 3];
    let mut pids = [0isize; 3];
    for (i, slot) in pids.iter_mut().enumerate() {
        let pid = fork();
        if pid == 0 {
            exit(10 + i as i32);
        }
        *slot = pid;
    }
    for _ in 0..3 {
        let pid = waitpid(-1, &mut status, 0);
        let Some(i) = pids.iter().position(|&p| p == pid) else { return line!() as i32 };
        check!(wexitstatus(status) == 10 + i as i32);
        seen[i] = true;
    }
    check!(seen == [true; 3]);

    // ---- execve ----
    match spawn_and_wait("/bin/test_args", &["test_args", "one", "two words"]) {
        Ok(status) => check!(wifexited(status) && wexitstatus(status) == 0),
        Err(_) => return line!() as i32,
    }
    match spawn_and_wait("/bin/test_proc", &["test_proc", "exit", "77"]) {
        Ok(status) => check!(wexitstatus(status) == 77),
        Err(_) => return line!() as i32,
    }
    // exec failures return an error and leave the process intact
    check!(execve("/bin/no-such-program", &["x"], &[]) == -ENOENT);
    check!(execve("/bin", &["x"], &[]) == -EACCES);
    let fd = open("/not_a_program.txt", O_CREAT | O_WRONLY | O_TRUNC) as i32;
    write(fd, b"echo this is text, not machine code");
    close(fd);
    // Not executable: refused before looking at the contents.
    check!(execve("/not_a_program.txt", &["x"], &[]) == -EACCES);
    // Executable but not a program (no ELF header, no #!): ENOEXEC.
    chmod("/not_a_program.txt", 0o755);
    check!(execve("/not_a_program.txt", &["x"], &[]) == -ENOEXEC);
    unlink("/not_a_program.txt");
    // spawn of a missing program: the child exits 127
    match spawn_and_wait("/bin/nope", &["nope"]) {
        Ok(status) => check!(wexitstatus(status) == 127),
        Err(_) => return line!() as i32,
    }

    // ---- a crashing child is reported as killed by SIGSEGV ----
    match spawn_and_wait("/bin/test_stack", &["test_stack"]) {
        Ok(status) => check!(wifsignaled(status) && wtermsig(status) == 11),
        Err(_) => return line!() as i32,
    }

    // ---- files are inherited across fork and exec ----
    let fd = open("/proc_test.txt", O_CREAT | O_WRONLY | O_TRUNC) as i32;
    check!(fd >= 0);
    let pid = fork();
    if pid == 0 {
        // child writes through the inherited fd
        exit(if write(fd, b"from child") == 10 { 0 } else { 1 });
    }
    check!(waitpid(pid as i32, &mut status, 0) == pid && wexitstatus(status) == 0);
    close(fd);
    check!(stat("/proc_test.txt").map(|s| s.st_size) == Ok(10));
    check!(unlink("/proc_test.txt") == 0);

    // ---- a child's chdir doesn't move the parent ----
    let pid = fork();
    if pid == 0 {
        chdir("/bin");
        exit(0);
    }
    waitpid(pid as i32, &mut status, 0);
    let mut buf = [0u8; 64];
    check!(getcwd(&mut buf) == Ok("/"));

    // ---- WNOHANG: poll until a busy child finishes ----
    let pid = fork();
    if pid == 0 {
        // Long enough (several timer ticks) that it can't finish within a
        // single time slice before the parent's first poll.
        spin(300);
        exit(5);
    }
    let mut polls = 0u64;
    loop {
        let r = waitpid(pid as i32, &mut status, WNOHANG);
        if r == pid {
            break;
        }
        check!(r == 0);
        polls += 1;
        sched_yield();
    }
    check!(wexitstatus(status) == 5);
    check!(polls > 0); // it really was still running at first
    0
}

/// The parent never blocks (only WNOHANG polls, no yields), so the child
/// can only make progress if the timer preempts the parent.
fn preempt() -> i32 {
    let pid = fork();
    if pid == 0 {
        spin(3);
        exit(9);
    }
    let mut status = 0;
    loop {
        let r = waitpid(pid as i32, &mut status, WNOHANG);
        if r == pid {
            return if wexitstatus(status) == 9 { 0 } else { 2 };
        }
        if r < 0 {
            return 3;
        }
        spin(1);
    }
}

fn forkbomb() -> i32 {
    let mut children = 0;
    loop {
        let pid = fork();
        if pid == 0 {
            // Children wait to be reaped; spinning keeps them alive a bit.
            spin(1);
            exit(0);
        }
        if pid < 0 {
            check!(pid == -EAGAIN);
            break;
        }
        children += 1;
        if children > 100 {
            return line!() as i32; // the limit never kicked in
        }
    }
    let mut status = 0;
    let mut reaped = 0;
    while waitpid(-1, &mut status, 0) > 0 {
        reaped += 1;
    }
    check!(reaped == children);
    0
}

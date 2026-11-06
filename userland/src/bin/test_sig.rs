// Copyright (c) 2026 tre4surehunter9
//! Test program for signals, process groups, sleep and the clock.
//! Exits 0 on success, or the line number of the first failed check.
#![no_std]
#![no_main]

use userlib::*;

userlib::entry!(main);

macro_rules! check {
    ($cond:expr) => {
        if !$cond {
            eprintln!("test_sig: check failed at line {}: {}", line!(), stringify!($cond));
            return line!() as i32;
        }
    };
}

fn elapsed_ms(start: (u64, u64)) -> u64 {
    let (s, ns) = monotonic_time();
    (s * 1000 + ns / 1_000_000).saturating_sub(start.0 * 1000 + start.1 / 1_000_000)
}

fn main(_args: Args) -> i32 {
    let me = getpid();

    // ---- kill basics ----
    check!(kill(me, 0) == 0);
    check!(kill(30000, 0) == -ESRCH);
    check!(kill(me, 99) == -EINVAL);
    check!(set_signal(SIGKILL, SIG_IGN) == -EINVAL);
    check!(set_signal(0, SIG_IGN) == -EINVAL);

    // ---- clock and sleep ----
    let t0 = monotonic_time();
    check!(sleep_ms(300) == 0);
    let ms = elapsed_ms(t0);
    check!(ms >= 250 && ms < 3000);
    check!(sleep_ms(0) == 0);

    // ---- SIGTERM kills a sleeping child, and cuts its sleep short ----
    let t0 = monotonic_time();
    let pid = fork();
    if pid == 0 {
        sleep_ms(30_000);
        exit(1);
    }
    sleep_ms(100);
    check!(kill(pid as i32, SIGTERM) == 0);
    let mut status = 0;
    check!(waitpid(pid as i32, &mut status, 0) == pid);
    check!(wifsignaled(status) && wtermsig(status) == SIGTERM);
    check!(elapsed_ms(t0) < 5000);

    // ---- a busy child is killed too (via the timer interrupt) ----
    let pid = fork();
    if pid == 0 {
        loop {
            core::hint::spin_loop();
        }
    }
    check!(kill(pid as i32, SIGKILL) == 0);
    check!(waitpid(pid as i32, &mut status, 0) == pid && wtermsig(status) == SIGKILL);

    // ---- SIG_IGN: the signal is dropped; SIGKILL can't be ignored ----
    let pid = fork();
    if pid == 0 {
        set_signal(SIGTERM, SIG_IGN);
        kill(getpid(), SIGTERM);
        sleep_ms(50);
        exit(7); // still alive
    }
    check!(waitpid(pid as i32, &mut status, 0) == pid && wifexited(status) && wexitstatus(status) == 7);
    check!(set_signal(SIGTERM, SIG_DFL) == 0);

    // ---- ignored signals stay ignored across exec (POSIX) ----
    let pid = fork();
    if pid == 0 {
        set_signal(SIGINT, SIG_IGN);
        execve("/bin/sleep", &["sleep", "0.3"], &[]);
        exit(126);
    }
    sleep_ms(100);
    kill(pid as i32, SIGINT);
    check!(waitpid(pid as i32, &mut status, 0) == pid && wifexited(status) && wexitstatus(status) == 0);

    // ---- process groups ----
    check!(getpgrp() > 0);
    let pid = fork();
    if pid == 0 {
        setpgid(0, 0);
        sleep_ms(30_000);
        exit(1);
    }
    setpgid(pid as i32, pid as i32);
    sleep_ms(50);
    check!(kill(-(pid as i32), SIGTERM) == 0); // signal the whole group
    check!(waitpid(pid as i32, &mut status, 0) == pid && wtermsig(status) == SIGTERM);
    check!(kill(-30000, SIGTERM) == -ESRCH);
    check!(setpgid(30000, 0) == -ESRCH);

    // ---- a signal interrupts a blocked wait: a child waiting for its own
    // child is killed while it waits ----
    let pid = fork();
    if pid == 0 {
        let grandchild = fork();
        if grandchild == 0 {
            sleep_ms(30_000);
            exit(0);
        }
        let mut s = 0;
        waitpid(grandchild as i32, &mut s, 0); // blocked here when killed
        exit(1);
    }
    sleep_ms(100);
    check!(kill(pid as i32, SIGTERM) == 0);
    check!(waitpid(pid as i32, &mut status, 0) == pid && wtermsig(status) == SIGTERM);
    // the grandchild was orphaned to the kernel; stop it so it doesn't linger
    kill(-1, SIGKILL);
    0
}

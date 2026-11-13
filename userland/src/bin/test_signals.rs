// Copyright (c) 2026 tre4surehunter9
//! Test program for signal handlers, masks, alarms, stop/continue.
//! Exits 0, or the line number of the first failed check.
#![no_std]
#![no_main]

extern crate alloc;

use core::sync::atomic::{AtomicI32, AtomicU32, Ordering::SeqCst};
use userlib::*;

userlib::entry!(main);

macro_rules! check {
    ($cond:expr) => {
        if !$cond {
            eprintln!("test_signals: check failed at line {}: {}", line!(), stringify!($cond));
            return line!() as i32;
        }
    };
}

static COUNT: AtomicU32 = AtomicU32::new(0);
static LAST: AtomicI32 = AtomicI32::new(0);
static DEPTH: AtomicU32 = AtomicU32::new(0);
static MAX_DEPTH: AtomicU32 = AtomicU32::new(0);
static MASK_SEEN: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

extern "C" fn count(sig: i32) {
    COUNT.fetch_add(1, SeqCst);
    LAST.store(sig, SeqCst);
}

extern "C" fn record_mask(sig: i32) {
    MASK_SEEN.store(blocked_signals(), SeqCst);
    count(sig);
}

/// Raises its own signal again inside the handler (twice in total).
extern "C" fn reentrant(sig: i32) {
    let d = DEPTH.fetch_add(1, SeqCst) + 1;
    MAX_DEPTH.fetch_max(d, SeqCst);
    if COUNT.fetch_add(1, SeqCst) == 0 {
        raise(sig);
    }
    DEPTH.fetch_sub(1, SeqCst);
}

fn reset() {
    COUNT.store(0, SeqCst);
    LAST.store(0, SeqCst);
    DEPTH.store(0, SeqCst);
    MAX_DEPTH.store(0, SeqCst);
}

fn handle(sig: i32, handler: Handler, flags: u64) -> Result<SigAction, isize> {
    sigaction(sig, &SigAction { handler: handler as usize as u64, flags, restorer: 0, mask: 0 })
}

fn main(args: Args) -> i32 {
    match args.get(1) {
        Some("all") => all(),
        Some("check-exec") => check_exec(),
        _ => 98,
    }
}

/// After execve: handlers are reset, ignored signals and the mask are kept.
fn check_exec() -> i32 {
    check!(get_sigaction(SIGUSR1).is_ok_and(|a| a.handler == SIG_DFL));
    check!(get_sigaction(SIGUSR2).is_ok_and(|a| a.handler == SIG_IGN));
    check!(blocked_signals() & sigmask(SIGHUP) != 0);
    0
}

fn all() -> i32 {
    // ---- a handler runs, gets the signal number, can be read back ----
    check!(signal(SIGUSR1, count) == 0);
    check!(raise(SIGUSR1) == 0);
    check!(COUNT.load(SeqCst) == 1 && LAST.load(SeqCst) == SIGUSR1);
    check!(get_sigaction(SIGUSR1).is_ok_and(|a| a.handler == count as *const () as u64 && a.flags & SA_RESTART != 0));
    raise(SIGUSR1);
    check!(COUNT.load(SeqCst) == 2); // still installed
    // SIGKILL and SIGSTOP can't be caught.
    check!(handle(SIGKILL, count, 0) == Err(-EINVAL as isize));
    check!(handle(SIGSTOP, count, 0) == Err(-EINVAL as isize));

    // ---- SA_RESETHAND: once only ----
    reset();
    check!(handle(SIGUSR2, count, SA_RESETHAND).is_ok());
    raise(SIGUSR2);
    check!(COUNT.load(SeqCst) == 1);
    check!(get_sigaction(SIGUSR2).is_ok_and(|a| a.handler == SIG_DFL));

    // ---- blocking: the signal waits until unblocked ----
    reset();
    let old = sigprocmask(SIG_BLOCK, sigmask(SIGUSR1));
    check!(old & sigmask(SIGUSR1) == 0);
    raise(SIGUSR1);
    raise(SIGUSR1); // (standard signals don't queue: still one)
    check!(COUNT.load(SeqCst) == 0);
    check!(sigpending() == sigmask(SIGUSR1));
    sigprocmask(SIG_UNBLOCK, sigmask(SIGUSR1)); // delivered on the way out
    check!(COUNT.load(SeqCst) == 1);
    check!(sigpending() == 0);
    // SIGKILL can't be blocked
    sigprocmask(SIG_BLOCK, sigmask(SIGKILL) | sigmask(SIGSTOP));
    check!(blocked_signals() & (sigmask(SIGKILL) | sigmask(SIGSTOP)) == 0);

    // ---- while a handler runs, its signal (and sa_mask) are blocked ----
    reset();
    check!(sigaction(SIGUSR1, &SigAction { handler: record_mask as *const () as u64, flags: 0, restorer: 0, mask: sigmask(SIGHUP) }).is_ok());
    raise(SIGUSR1);
    let seen = MASK_SEEN.load(SeqCst);
    check!(seen & sigmask(SIGUSR1) != 0 && seen & sigmask(SIGHUP) != 0);
    check!(blocked_signals() & (sigmask(SIGUSR1) | sigmask(SIGHUP)) == 0); // restored after
    // a signal raised inside its own handler runs after it returns...
    reset();
    check!(handle(SIGUSR2, reentrant, 0).is_ok());
    raise(SIGUSR2);
    check!(COUNT.load(SeqCst) == 2 && MAX_DEPTH.load(SeqCst) == 1);
    // ...or nested, with SA_NODEFER
    reset();
    check!(handle(SIGUSR2, reentrant, SA_NODEFER).is_ok());
    raise(SIGUSR2);
    check!(COUNT.load(SeqCst) == 2 && MAX_DEPTH.load(SeqCst) == 2);

    // ---- a busy program is interrupted without losing its registers ----
    let pid = fork();
    if pid == 0 {
        exit(busy_child());
    }
    sleep_ms(150);
    check!(kill(pid as i32, SIGUSR1) == 0);
    let mut status = 0;
    check!(waitpid(pid as i32, &mut status, 0) == pid);
    check!(wifexited(status) && wexitstatus(status) == 0);

    // ---- alarm and pause ----
    reset();
    check!(signal(SIGALRM, count) == 0);
    check!(alarm(1) == 0);
    let (s0, _) = monotonic_time();
    check!(pause() == -EINTR as isize);
    let (s1, _) = monotonic_time();
    check!(COUNT.load(SeqCst) == 1 && LAST.load(SeqCst) == SIGALRM);
    check!(s1 - s0 <= 2);
    check!(alarm(5) == 0);
    let left = alarm(0); // cancel, returning what was left
    check!(left == 5 || left == 4);
    // a repeating timer
    reset();
    check!(set_alarm_ms(100, 100) == 0);
    while COUNT.load(SeqCst) < 3 {
        pause();
    }
    set_alarm_ms(0, 0);

    // ---- EINTR, and SA_RESTART ----
    let Ok((r, w)) = pipe() else { return line!() as i32 };
    let mut byte = [0u8; 1];
    reset();
    check!(handle(SIGALRM, count, 0).is_ok()); // no SA_RESTART
    set_alarm_ms(100, 0);
    check!(read(r, &mut byte) == -EINTR as isize);
    check!(COUNT.load(SeqCst) == 1);
    reset();
    check!(handle(SIGALRM, count, SA_RESTART).is_ok());
    let writer = fork();
    if writer == 0 {
        sleep_ms(400);
        write(w, b"x");
        exit(0);
    }
    set_alarm_ms(100, 0);
    check!(read(r, &mut byte) == 1 && byte[0] == b'x'); // restarted after the handler
    check!(COUNT.load(SeqCst) == 1);
    waitpid(writer as i32, &mut status, 0);
    // sleeps always report EINTR (even with SA_RESTART)
    set_alarm_ms(100, 0);
    check!(sleep_ms(2000) == -EINTR as isize);
    close(r);
    close(w);

    // ---- sigsuspend: wait with a temporary mask ----
    reset();
    check!(signal(SIGUSR1, count) == 0);
    sigprocmask(SIG_BLOCK, sigmask(SIGUSR1));
    let me = getpid();
    let sender = fork();
    if sender == 0 {
        sleep_ms(100);
        kill(me, SIGUSR1);
        exit(0);
    }
    check!(sigsuspend(0) == -EINTR as isize);
    check!(COUNT.load(SeqCst) == 1);
    check!(blocked_signals() & sigmask(SIGUSR1) != 0); // the old mask is back
    sigprocmask(SIG_UNBLOCK, sigmask(SIGUSR1));
    waitpid(sender as i32, &mut status, 0);

    // ---- SIGCHLD ----
    reset();
    check!(signal(SIGCHLD, count) == 0);
    let child = fork();
    if child == 0 {
        exit(0);
    }
    check!(waitpid(child as i32, &mut status, 0) == child);
    check!(COUNT.load(SeqCst) >= 1 && LAST.load(SeqCst) == SIGCHLD);
    set_signal(SIGCHLD, SIG_DFL);

    // ---- stop and continue ----
    let child = fork();
    if child == 0 {
        // Its own process group, like a shell job: SIGTSTP doesn't stop
        // processes in orphaned groups (ours was started by the kernel).
        setpgid(0, 0);
        loop {
            sleep_ms(50);
        }
    }
    let c = child as i32;
    setpgid(c, c);
    check!(kill(c, SIGSTOP) == 0);
    check!(waitpid(c, &mut status, WUNTRACED) == child);
    check!(wifstopped(status) && wstopsig(status) == SIGSTOP);
    let proc_status = userlib::io::read_text(&alloc::format!("/proc/{}/status", c)).unwrap_or_default();
    check!(proc_status.contains("T (stopped)"));
    check!(waitpid(c, &mut status, WNOHANG | WUNTRACED) == 0); // reported once
    check!(kill(c, SIGCONT) == 0);
    check!(waitpid(c, &mut status, WCONTINUED) == child && wifcontinued(status));
    check!(kill(c, SIGTSTP) == 0); // Ctrl-Z's signal stops it too
    check!(waitpid(c, &mut status, WUNTRACED) == child && wstopsig(status) == SIGTSTP);
    check!(kill(c, SIGKILL) == 0); // a stopped process can still be killed
    check!(waitpid(c, &mut status, 0) == child && wifsignaled(status) && wtermsig(status) == SIGKILL);
    // A sleep that's stopped and continued still sleeps its full time.
    let (t0, n0) = monotonic_time();
    let child = fork();
    if child == 0 {
        exit(if sleep_ms(600) == 0 { 0 } else { 1 });
    }
    sleep_ms(100);
    kill(child as i32, SIGSTOP);
    sleep_ms(100);
    kill(child as i32, SIGCONT);
    check!(waitpid(child as i32, &mut status, 0) == child && wexitstatus(status) == 0);
    let (t1, n1) = monotonic_time();
    check!((t1 * 1000 + n1 / 1_000_000) - (t0 * 1000 + n0 / 1_000_000) >= 550);

    // ---- execve resets handlers, keeps SIG_IGN and the mask ----
    check!(signal(SIGUSR1, count) == 0);
    check!(set_signal(SIGUSR2, SIG_IGN) == 0);
    sigprocmask(SIG_BLOCK, sigmask(SIGHUP));
    match spawn_and_wait("/bin/test_signals", &["test_signals", "check-exec"]) {
        Ok(s) => check!(wexitstatus(s) == 0),
        Err(_) => return line!() as i32,
    }
    sigprocmask(SIG_UNBLOCK, sigmask(SIGHUP));
    set_signal(SIGUSR2, SIG_DFL);
    // /proc shows caught and ignored signals
    let st = userlib::io::read_text("/proc/self/status").unwrap_or_default();
    check!(st.lines().any(|l| l.starts_with("SigCgt:") && l.ends_with(&alloc::format!("{:016x}", sigmask(SIGUSR1) | sigmask(SIGALRM)))));

    // ---- the default action still applies when there's no handler ----
    let child = fork();
    if child == 0 {
        set_signal(SIGUSR1, SIG_DFL);
        loop {
            sleep_ms(50);
        }
    }
    sleep_ms(50);
    kill(child as i32, SIGUSR1);
    check!(waitpid(child as i32, &mut status, 0) == child && wtermsig(status) == SIGUSR1);
    0
}

static GOT: AtomicU32 = AtomicU32::new(0);

extern "C" fn got(_sig: i32) {
    GOT.store(1, SeqCst);
}

/// Spin, computing, until SIGUSR1's handler sets GOT; the signal arrives
/// in the middle of the loop (from the timer), so it checks the
/// computation kept every register.
fn busy_child() -> i32 {
    signal(SIGUSR1, got);
    let (mut a, mut b, mut c, mut d) = (1u64, 2u64, 3u64, 4u64);
    let mut n = 0u64;
    while GOT.load(SeqCst) == 0 {
        a = a.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        b = b.rotate_left(7) ^ a;
        c = c.wrapping_add(b) ^ (a >> 3);
        d = d.wrapping_sub(c).rotate_right(11);
        n += 1;
    }
    // Recompute: the same number of steps must give the same values.
    let (mut a2, mut b2, mut c2, mut d2) = (1u64, 2u64, 3u64, 4u64);
    for _ in 0..n {
        a2 = a2.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        b2 = b2.rotate_left(7) ^ a2;
        c2 = c2.wrapping_add(b2) ^ (a2 >> 3);
        d2 = d2.wrapping_sub(c2).rotate_right(11);
    }
    if (a, b, c, d) == (a2, b2, c2, d2) && n > 1000 { 0 } else { 1 }
}

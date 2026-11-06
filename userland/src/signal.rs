// Copyright (c) 2026 tre4surehunter9
//! Signals, process groups, sleeping and the clock.

use crate::sys::{self, syscall3};

pub const SIGHUP: i32 = 1;
pub const SIGINT: i32 = 2;
pub const SIGQUIT: i32 = 3;
pub const SIGKILL: i32 = 9;
pub const SIGUSR1: i32 = 10;
pub const SIGSEGV: i32 = 11;
pub const SIGUSR2: i32 = 12;
pub const SIGPIPE: i32 = 13;
pub const SIGALRM: i32 = 14;
pub const SIGTERM: i32 = 15;
pub const SIGCHLD: i32 = 17;
pub const SIGCONT: i32 = 18;
pub const SIGSTOP: i32 = 19;
pub const SIGTSTP: i32 = 20;
pub const SIGTTIN: i32 = 21;
pub const SIGTTOU: i32 = 22;
pub const SIGWINCH: i32 = 28;

/// (number, name) of the common signals, for kill -l and messages.
pub const SIGNAL_NAMES: &[(i32, &str)] = &[
    (1, "HUP"), (2, "INT"), (3, "QUIT"), (4, "ILL"), (5, "TRAP"), (6, "ABRT"), (7, "BUS"),
    (8, "FPE"), (9, "KILL"), (10, "USR1"), (11, "SEGV"), (12, "USR2"), (13, "PIPE"),
    (14, "ALRM"), (15, "TERM"), (16, "STKFLT"), (17, "CHLD"), (18, "CONT"), (19, "STOP"), (20, "TSTP"),
    (21, "TTIN"), (22, "TTOU"), (23, "URG"), (24, "XCPU"), (25, "XFSZ"), (26, "VTALRM"), (27, "PROF"),
    (28, "WINCH"), (29, "IO"), (30, "PWR"), (31, "SYS"),
];

/// A signal number from a name ("INT", "SIGINT", "int") or a number.
pub fn signal_number(spec: &str) -> Option<i32> {
    let name = spec.strip_prefix("SIG").or_else(|| spec.strip_prefix("sig")).unwrap_or(spec);
    spec.parse::<i32>().ok().filter(|n| (0..=64).contains(n)).or_else(|| {
        SIGNAL_NAMES.iter().find(|(_, n)| n.eq_ignore_ascii_case(name)).map(|(n, _)| *n)
    })
}

pub fn signal_name(sig: i32) -> Option<&'static str> {
    SIGNAL_NAMES.iter().find(|(n, _)| *n == sig).map(|(_, name)| *name)
}

/// kill(2): send `sig` to process `pid` (or a group if pid < 0).
pub fn kill(pid: i32, sig: i32) -> isize {
    unsafe { syscall3(sys::KILL, pid as i64 as u64, sig as u64, 0) as isize }
}

pub const SIG_DFL: u64 = 0;
pub const SIG_IGN: u64 = 1;

/// Set a signal to SIG_DFL or SIG_IGN.
pub fn set_signal(sig: i32, disposition: u64) -> isize {
    match sigaction(sig, &SigAction { handler: disposition, flags: 0, restorer: 0, mask: 0 }) {
        Ok(_) => 0,
        Err(e) => e,
    }
}

// ---- handlers ----

/// sigaction flags
pub const SA_NOCLDSTOP: u64 = 1;
pub const SA_SIGINFO: u64 = 4;
pub const SA_RESTORER: u64 = 0x0400_0000;
/// Restart read/write/wait... instead of failing with EINTR after the handler.
pub const SA_RESTART: u64 = 0x1000_0000;
/// Don't block the signal while its own handler runs.
pub const SA_NODEFER: u64 = 0x4000_0000;
/// Back to SIG_DFL once the handler has run.
pub const SA_RESETHAND: u64 = 0x8000_0000;

/// A signal handler: called with the signal number.
pub type Handler = extern "C" fn(i32);

/// struct sigaction as the kernel takes it.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SigAction {
    /// SIG_DFL, SIG_IGN or a handler's address.
    pub handler: u64,
    pub flags: u64,
    pub restorer: u64,
    /// Signals blocked while the handler runs (see `sigmask`).
    pub mask: u64,
}

// Where handlers return to: it asks the kernel to restore the interrupted
// registers (rt_sigreturn never returns here).
core::arch::global_asm!(
    ".global __userlib_restore_rt",
    "__userlib_restore_rt:",
    "mov rax, 15",
    "int 0x80",
    "ud2",
);

unsafe extern "C" {
    fn __userlib_restore_rt();
}

/// The bit for `sig` in a signal set.
pub const fn sigmask(sig: i32) -> u64 {
    1 << (sig - 1)
}

/// sigaction(2): set what `sig` does; returns the previous action. A
/// handler gets the restorer filled in automatically.
pub fn sigaction(sig: i32, act: &SigAction) -> Result<SigAction, isize> {
    let mut act = *act;
    if act.handler > SIG_IGN {
        act.flags |= SA_RESTORER;
        act.restorer = __userlib_restore_rt as *const () as u64;
    }
    let mut old = SigAction::default();
    let r = unsafe {
        crate::sys::syscall4(sys::RT_SIGACTION, sig as u64, &act as *const SigAction as u64, &mut old as *mut SigAction as u64, 8)
    };
    if r < 0 { Err(r as isize) } else { Ok(old) }
}

/// The current action for `sig`, without changing it.
pub fn get_sigaction(sig: i32) -> Result<SigAction, isize> {
    let mut old = SigAction::default();
    let r = unsafe { crate::sys::syscall4(sys::RT_SIGACTION, sig as u64, 0, &mut old as *mut SigAction as u64, 8) };
    if r < 0 { Err(r as isize) } else { Ok(old) }
}

/// signal(2), BSD style: run `handler` for `sig`, restarting interrupted
/// syscalls. Returns 0 or -errno.
pub fn signal(sig: i32, handler: Handler) -> isize {
    let act = SigAction { handler: handler as usize as u64, flags: SA_RESTART, restorer: 0, mask: 0 };
    match sigaction(sig, &act) {
        Ok(_) => 0,
        Err(e) => e,
    }
}

pub const SIG_BLOCK: u64 = 0;
pub const SIG_UNBLOCK: u64 = 1;
pub const SIG_SETMASK: u64 = 2;

/// sigprocmask(2): block (SIG_BLOCK), unblock (SIG_UNBLOCK) or replace
/// (SIG_SETMASK) the blocked set. Returns the previous set.
pub fn sigprocmask(how: u64, set: u64) -> u64 {
    let mut old = 0u64;
    unsafe { crate::sys::syscall4(sys::RT_SIGPROCMASK, how, &set as *const u64 as u64, &mut old as *mut u64 as u64, 8) };
    old
}

/// The blocked set, unchanged.
pub fn blocked_signals() -> u64 {
    let mut old = 0u64;
    unsafe { crate::sys::syscall4(sys::RT_SIGPROCMASK, SIG_BLOCK, 0, &mut old as *mut u64 as u64, 8) };
    old
}

/// sigpending(2): signals sent but blocked.
pub fn sigpending() -> u64 {
    let mut set = 0u64;
    unsafe { syscall3(sys::RT_SIGPENDING, &mut set as *mut u64 as u64, 8, 0) };
    set
}

/// sigsuspend(2): wait for a signal with `mask` blocked meanwhile (the
/// current mask comes back afterwards). Returns -EINTR.
pub fn sigsuspend(mask: u64) -> isize {
    unsafe { syscall3(sys::RT_SIGSUSPEND, &mask as *const u64 as u64, 8, 0) as isize }
}

/// pause(2): wait for a signal. Returns -EINTR.
pub fn pause() -> isize {
    unsafe { syscall3(sys::PAUSE, 0, 0, 0) as isize }
}

/// alarm(2): SIGALRM in `secs` seconds (0 cancels). Returns the seconds
/// that were left on the previous alarm.
pub fn alarm(secs: u32) -> u32 {
    unsafe { syscall3(sys::ALARM, secs as u64, 0, 0) as u32 }
}

/// setitimer(ITIMER_REAL): SIGALRM after `ms`, then every `interval_ms`
/// (0 = once). ms = 0 cancels.
pub fn set_alarm_ms(ms: u64, interval_ms: u64) -> isize {
    let tv = |ms: u64| [ms / 1000, (ms % 1000) * 1000];
    let (i, v) = (tv(interval_ms), tv(ms));
    let val = [i[0], i[1], v[0], v[1]];
    unsafe { syscall3(sys::SETITIMER, 0, val.as_ptr() as u64, 0) as isize }
}

/// raise(3): send a signal to ourselves.
pub fn raise(sig: i32) -> isize {
    kill(crate::getpid(), sig)
}

pub fn setpgid(pid: i32, pgid: i32) -> isize {
    unsafe { syscall3(sys::SETPGID, pid as u64, pgid as u64, 0) as isize }
}

pub fn getpgrp() -> i32 {
    unsafe { syscall3(sys::GETPGRP, 0, 0, 0) as i32 }
}

/// nanosleep(2) for `ms` milliseconds. Returns 0, or -EINTR if a signal
/// cut it short.
pub fn sleep_ms(ms: u64) -> isize {
    let ts = [(ms / 1000) as i64, ((ms % 1000) * 1_000_000) as i64];
    let mut rem = [0i64; 2];
    unsafe { syscall3(sys::NANOSLEEP, ts.as_ptr() as u64, rem.as_mut_ptr() as u64, 0) as isize }
}

/// Time since boot, as (seconds, nanoseconds) (CLOCK_MONOTONIC).
pub fn monotonic_time() -> (u64, u64) {
    let mut ts = [0i64; 2];
    unsafe { syscall3(sys::CLOCK_GETTIME, 1, ts.as_mut_ptr() as u64, 0) };
    (ts[0] as u64, ts[1] as u64)
}

/// Seconds since 1970 (CLOCK_REALTIME).
pub fn unix_time() -> u64 {
    let mut ts = [0i64; 2];
    unsafe { syscall3(sys::CLOCK_GETTIME, 0, ts.as_mut_ptr() as u64, 0) };
    ts[0] as u64
}

/// A Unix time as (year, month, day, hour, minute, second), UTC.
pub fn civil_time(t: u64) -> (i64, u32, u32, u32, u32, u32) {
    let days = (t / 86400) as i64;
    let secs = t % 86400;
    // days -> civil date (Howard Hinnant's algorithm)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (y, m, d, (secs / 3600) as u32, (secs / 60 % 60) as u32, (secs % 60) as u32)
}

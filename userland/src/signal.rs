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

/// (number, name) of the common signals, for kill -l and messages.
pub const SIGNAL_NAMES: &[(i32, &str)] = &[
    (1, "HUP"), (2, "INT"), (3, "QUIT"), (4, "ILL"), (5, "TRAP"), (6, "ABRT"), (7, "BUS"),
    (8, "FPE"), (9, "KILL"), (10, "USR1"), (11, "SEGV"), (12, "USR2"), (13, "PIPE"),
    (14, "ALRM"), (15, "TERM"), (17, "CHLD"), (18, "CONT"), (19, "STOP"), (20, "TSTP"),
];

pub fn signal_name(sig: i32) -> Option<&'static str> {
    SIGNAL_NAMES.iter().find(|(n, _)| *n == sig).map(|(_, name)| *name)
}

/// kill(2): send `sig` to process `pid` (or a group if pid < 0).
pub fn kill(pid: i32, sig: i32) -> isize {
    unsafe { syscall3(sys::KILL, pid as i64 as u64, sig as u64, 0) as isize }
}

pub const SIG_DFL: u64 = 0;
pub const SIG_IGN: u64 = 1;

/// Set a signal to SIG_DFL or SIG_IGN (handlers aren't supported yet).
pub fn set_signal(sig: i32, disposition: u64) -> isize {
    let act = [disposition, 0u64, 0, 0]; // struct sigaction
    unsafe { syscall3(sys::RT_SIGACTION, sig as u64, act.as_ptr() as u64, 0) as isize }
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

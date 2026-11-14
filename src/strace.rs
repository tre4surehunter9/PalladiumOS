// Copyright (c) 2026 tre4surehunter9
//! strace-style tracing, done by the kernel: a traced process's system
//! calls are written out as they finish, one line each (name, arguments
//! with paths and data shown as strings, result with its errno name), to
//! a file it chose. /bin/strace turns it on for itself, then runs the
//! program (`strace ls`, `strace -o FILE cmd`). Children are traced too
//! (their lines start with [pid N]). Linux's strace uses ptrace instead,
//! which PalladiumOS doesn't have.
//!
//! prctl(PR_PALLADIUM_TRACE, fd): trace this process (and its children
//! from now on) to fd (a handle of its own: closing fd later doesn't stop
//! it).

use alloc::collections::BTreeMap;
use alloc::string::String;
use core::fmt::Write;
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;

use crate::syscall::{user_slice, SyscallFrame};
use crate::vfs::Handle;

pub const PR_PALLADIUM_TRACE: u64 = 0x5041_4C54; // "PALT"

struct Target {
    out: Handle,
    /// The first process traced (its lines have no [pid N]).
    root: u64,
}

static TRACED: Mutex<BTreeMap<u64, Target>> = Mutex::new(BTreeMap::new());
/// How many processes are traced (0: the lock-free system calls go on).
pub static ANY: AtomicUsize = AtomicUsize::new(0);

/// prctl(PR_PALLADIUM_TRACE, fd).
pub fn start(fd: u64) -> i64 {
    let out = match crate::vfs::handle_of(fd) {
        Ok(h) => h,
        Err(e) => return -e,
    };
    let me = crate::process::current_tgid();
    let mut t = TRACED.lock();
    t.insert(me, Target { out, root: me });
    ANY.store(t.len(), Ordering::SeqCst);
    0
}

/// A new process: traced if its parent is.
pub fn inherit(parent: u64, child: u64) {
    if ANY.load(Ordering::Relaxed) == 0 {
        return;
    }
    let mut t = TRACED.lock();
    if let Some(p) = t.get(&parent) {
        let target = Target { out: p.out.clone(), root: p.root };
        t.insert(child, target);
        ANY.store(t.len(), Ordering::SeqCst);
    }
}

/// A process is gone.
pub fn forget(tgid: u64) {
    if ANY.load(Ordering::Relaxed) == 0 {
        return;
    }
    let mut t = TRACED.lock();
    t.remove(&tgid);
    ANY.store(t.len(), Ordering::SeqCst);
}

/// The system call's name.
pub fn name(num: u64) -> &'static str {
    match num {
        0 => "read",
        1 => "write",
        2 => "open",
        3 => "close",
        4 => "stat",
        5 => "fstat",
        6 => "lstat",
        7 => "poll",
        8 => "lseek",
        9 => "mmap",
        10 => "mprotect",
        11 => "munmap",
        12 => "brk",
        13 => "rt_sigaction",
        14 => "rt_sigprocmask",
        15 => "rt_sigreturn",
        16 => "ioctl",
        17 => "pread64",
        18 => "pwrite64",
        19 => "readv",
        20 => "writev",
        21 => "access",
        22 => "pipe",
        23 => "select",
        24 => "sched_yield",
        25 => "mremap",
        26 => "msync",
        28 => "madvise",
        29 => "shmget",
        30 => "shmat",
        31 => "shmctl",
        32 => "dup",
        33 => "dup2",
        34 => "pause",
        35 => "nanosleep",
        36 => "getitimer",
        37 => "alarm",
        38 => "setitimer",
        39 => "getpid",
        40 => "sendfile",
        41 => "socket",
        42 => "connect",
        43 => "accept",
        44 => "sendto",
        45 => "recvfrom",
        46 => "sendmsg",
        47 => "recvmsg",
        48 => "shutdown",
        49 => "bind",
        50 => "listen",
        51 => "getsockname",
        52 => "getpeername",
        53 => "socketpair",
        54 => "setsockopt",
        55 => "getsockopt",
        56 => "clone",
        57 => "fork",
        58 => "vfork",
        59 => "execve",
        60 => "exit",
        61 => "wait4",
        62 => "kill",
        63 => "uname",
        67 => "shmdt",
        72 => "fcntl",
        73 => "flock",
        74 => "fsync",
        75 => "fdatasync",
        76 => "truncate",
        77 => "ftruncate",
        79 => "getcwd",
        80 => "chdir",
        81 => "fchdir",
        82 => "rename",
        83 => "mkdir",
        84 => "rmdir",
        86 => "link",
        87 => "unlink",
        88 => "symlink",
        89 => "readlink",
        90 => "chmod",
        91 => "fchmod",
        92 => "chown",
        93 => "fchown",
        94 => "lchown",
        95 => "umask",
        96 => "gettimeofday",
        97 => "getrlimit",
        98 => "getrusage",
        99 => "sysinfo",
        100 => "times",
        102 => "getuid",
        104 => "getgid",
        105 => "setuid",
        106 => "setgid",
        107 => "geteuid",
        108 => "getegid",
        109 => "setpgid",
        110 => "getppid",
        111 => "getpgrp",
        112 => "setsid",
        113 => "setreuid",
        114 => "setregid",
        115 => "getgroups",
        116 => "setgroups",
        117 => "setresuid",
        118 => "getresuid",
        119 => "setresgid",
        120 => "getresgid",
        121 => "getpgid",
        124 => "getsid",
        125 => "capget",
        127 => "rt_sigpending",
        128 => "rt_sigtimedwait",
        129 => "rt_sigqueueinfo",
        130 => "rt_sigsuspend",
        131 => "sigaltstack",
        132 => "utime",
        135 => "personality",
        137 => "statfs",
        138 => "fstatfs",
        140 => "getpriority",
        141 => "setpriority",
        143 => "sched_getparam",
        144 => "sched_setscheduler",
        145 => "sched_getscheduler",
        146 => "sched_get_priority_max",
        147 => "sched_get_priority_min",
        148 => "sched_rr_get_interval",
        149 => "mlock",
        150 => "munlock",
        151 => "mlockall",
        152 => "munlockall",
        157 => "prctl",
        158 => "arch_prctl",
        160 => "setrlimit",
        161 => "chroot",
        162 => "sync",
        165 => "mount",
        166 => "umount2",
        169 => "reboot",
        170 => "sethostname",
        186 => "gettid",
        187 => "readahead",
        200 => "tkill",
        201 => "time",
        202 => "futex",
        203 => "sched_setaffinity",
        204 => "sched_getaffinity",
        206 => "io_setup",
        213 => "epoll_create",
        217 => "getdents64",
        218 => "set_tid_address",
        221 => "fadvise64",
        222 => "timer_create",
        223 => "timer_settime",
        224 => "timer_gettime",
        225 => "timer_getoverrun",
        226 => "timer_delete",
        228 => "clock_gettime",
        229 => "clock_getres",
        230 => "clock_nanosleep",
        231 => "exit_group",
        232 => "epoll_wait",
        233 => "epoll_ctl",
        234 => "tgkill",
        235 => "utimes",
        247 => "waitid",
        251 => "ioprio_set",
        252 => "ioprio_get",
        253 => "inotify_init",
        254 => "inotify_add_watch",
        255 => "inotify_rm_watch",
        257 => "openat",
        258 => "mkdirat",
        260 => "fchownat",
        262 => "newfstatat",
        263 => "unlinkat",
        264 => "renameat",
        265 => "linkat",
        266 => "symlinkat",
        267 => "readlinkat",
        268 => "fchmodat",
        269 => "faccessat",
        270 => "pselect6",
        271 => "ppoll",
        273 => "set_robust_list",
        275 => "splice",
        277 => "sync_file_range",
        280 => "utimensat",
        281 => "epoll_pwait",
        282 => "signalfd",
        283 => "timerfd_create",
        284 => "eventfd",
        285 => "fallocate",
        286 => "timerfd_settime",
        287 => "timerfd_gettime",
        288 => "accept4",
        289 => "signalfd4",
        290 => "eventfd2",
        291 => "epoll_create1",
        292 => "dup3",
        293 => "pipe2",
        294 => "inotify_init1",
        295 => "preadv",
        296 => "pwritev",
        297 => "rt_tgsigqueueinfo",
        302 => "prlimit64",
        306 => "syncfs",
        309 => "getcpu",
        316 => "renameat2",
        318 => "getrandom",
        319 => "memfd_create",
        324 => "membarrier",
        325 => "mlock2",
        326 => "copy_file_range",
        327 => "preadv2",
        328 => "pwritev2",
        332 => "statx",
        334 => "rseq",
        425 => "io_uring_setup",
        434 => "pidfd_open",
        436 => "close_range",
        439 => "faccessat2",
        441 => "epoll_pwait2",
        452 => "fchmodat2",
        _ => "?",
    }
}

/// How many arguments to show.
fn nargs(num: u64) -> usize {
    match name(num) {
        "getpid" | "getppid" | "getuid" | "geteuid" | "getgid" | "getegid" | "gettid" | "sched_yield" | "fork" | "vfork"
        | "pause" | "setsid" | "getpgrp" | "sync" | "rt_sigreturn" => 0,
        "close" | "exit" | "exit_group" | "brk" | "dup" | "chdir" | "fchdir" | "rmdir" | "unlink" | "umask" | "alarm"
        | "uname" | "pipe" | "time" | "set_tid_address" | "fsync" | "getpgid" | "getsid" | "sysinfo" => 1,
        "kill" | "access" | "stat" | "lstat" | "fstat" | "mkdir" | "dup2" | "rename" | "nanosleep" | "gettimeofday"
        | "getcwd" | "clock_gettime" | "chmod" | "fchmod" | "link" | "symlink" | "munmap" | "listen" | "shutdown"
        | "pipe2" | "setpgid" | "truncate" | "ftruncate" | "flock" | "getrlimit" | "setrlimit" | "arch_prctl"
        | "creat" | "sigaltstack" | "tkill" | "eventfd2" | "epoll_create1" | "timerfd_create" => 2,
        "openat" | "wait4" | "rt_sigaction" | "rt_sigprocmask" | "newfstatat" | "pread64" | "pwrite64" | "sendfile"
        | "socketpair" | "accept4" | "getdents64" | "epoll_ctl" | "epoll_wait" | "fchownat" | "renameat" | "readlinkat"
        | "mknodat" | "faccessat" | "unlinkat" | "utimensat" | "prlimit64" | "tgkill" | "fchmodat" | "clock_nanosleep" => 4,
        "mremap" | "prctl" | "select" | "ppoll" | "clone" | "setsockopt" | "getsockopt" | "statx" | "renameat2"
        | "pselect6" | "waitid" | "epoll_pwait" => 5,
        "mmap" | "futex" | "recvfrom" | "sendto" => 6,
        _ => 3,
    }
}

/// Which arguments are paths (shown as strings).
fn is_path(num: u64, i: usize) -> bool {
    match name(num) {
        "open" | "stat" | "lstat" | "access" | "execve" | "mkdir" | "rmdir" | "unlink" | "chdir" | "readlink" | "chmod"
        | "chown" | "lchown" | "truncate" | "creat" | "statfs" | "chroot" | "mknod" | "utimes" => i == 0,
        "rename" | "link" | "symlink" => i < 2,
        "openat" | "newfstatat" | "faccessat" | "faccessat2" | "unlinkat" | "mkdirat" | "readlinkat" | "fchmodat"
        | "fchownat" | "statx" | "mknodat" | "utimensat" => i == 1,
        "renameat" | "renameat2" | "linkat" => i == 1 || i == 3,
        "symlinkat" => i == 0 || i == 2,
        _ => false,
    }
}

/// A user string (up to `max` bytes, or its NUL), escaped and quoted.
fn quoted(out: &mut String, ptr: u64, max: usize, nul: bool) {
    if ptr == 0 {
        out.push_str("NULL");
        return;
    }
    // Read page by page (a string may end just before an unmapped page).
    let n = max.min(256);
    let mut b: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
    let mut at = ptr;
    while b.len() < n {
        let chunk = ((4096 - (at & 4095)) as usize).min(n - b.len());
        let Some(part) = (unsafe { user_slice(at, chunk as u64) }) else { break };
        b.extend_from_slice(part);
        if nul && part.contains(&0) {
            break;
        }
        at += chunk as u64;
    }
    if b.is_empty() {
        let _ = write!(out, "{:#x}", ptr);
        return;
    }
    let end = if nul { b.iter().position(|&c| c == 0).unwrap_or(b.len()) } else { b.len() };
    out.push('"');
    for &c in &b[..end.min(32)] {
        match c {
            b'\n' => out.push_str("\\n"),
            b'\t' => out.push_str("\\t"),
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            0x20..=0x7e => out.push(c as char),
            _ => {
                let _ = write!(out, "\\x{:02x}", c);
            }
        }
    }
    out.push('"');
    if end > 32 || (!nul && max > 32) {
        out.push_str("...");
    }
}

/// The line so far, before the call ("name(args"): None if not traced.
/// (Paths are read now: execve replaces the memory they're in.)
pub fn before(num: u64, f: &SyscallFrame) -> Option<String> {
    if ANY.load(Ordering::Relaxed) == 0 || !TRACED.lock().contains_key(&crate::process::current_tgid()) {
        return None;
    }
    let args = [f.rdi, f.rsi, f.rdx, f.r10, f.r8, f.r9];
    let mut line = String::new();
    let _ = write!(line, "{}(", name(num));
    let n = name(num);
    for i in 0..nargs(num) {
        if i > 0 {
            line.push_str(", ");
        }
        let a = args[i];
        if is_path(num, i) {
            quoted(&mut line, a, 256, true);
        } else if i == 1 && (n == "write" || n == "pwrite64") {
            quoted(&mut line, a, args[2] as usize, false);
        } else if i == 1 && (n == "read" || n == "pread64") {
            line.push('\u{1}'); // (the data: filled in after)
        } else if (a as i64) < 0 && (a as i64) > -4096 {
            let _ = write!(line, "{}", a as i64);
        } else if a < 0x10000 {
            let _ = write!(line, "{}", a);
        } else {
            let _ = write!(line, "{:#x}", a);
        }
    }
    line.push(')');
    if matches!(n, "exit" | "exit_group") {
        line.push_str(" = ?");
        emit(line.clone());
        return None; // (it doesn't come back)
    }
    Some(line)
}

/// Finish the line with the result and write it out.
pub fn after(mut line: String, num: u64, f_args: (u64, u64), ret: i64) {
    let n = name(num);
    if (n == "read" || n == "pread64") && line.contains('\u{1}') {
        let mut data = String::new();
        if ret > 0 {
            quoted(&mut data, f_args.1, ret as usize, false);
        } else {
            let _ = write!(data, "{:#x}", f_args.1);
        }
        line = line.replacen('\u{1}', &data, 1);
    }
    if ret < 0 && ret > -4096 {
        let e = -ret;
        let _ = write!(line, " = -1 {}", errno_name(e));
    } else if (n == "mmap" || n == "brk" || n == "mremap") && ret > 0 {
        let _ = write!(line, " = {:#x}", ret);
    } else {
        let _ = write!(line, " = {}", ret);
    }
    emit(line);
}

/// Write a line to the process's trace (with [pid N] if it isn't the
/// first one traced).
fn emit(mut line: String) {
    let me = crate::process::current_tgid();
    let (out, root) = match TRACED.lock().get(&me) {
        Some(t) => (t.out.clone(), t.root),
        None => return,
    };
    if me != root {
        line.insert_str(0, &alloc::format!("[pid {}] ", me));
    }
    line.push('\n');
    let _ = crate::vfs::write_handle(&out, line.as_bytes());
}

fn errno_name(e: i64) -> &'static str {
    match e {
        1 => "EPERM (Operation not permitted)",
        2 => "ENOENT (No such file or directory)",
        3 => "ESRCH (No such process)",
        4 => "EINTR (Interrupted system call)",
        5 => "EIO (Input/output error)",
        6 => "ENXIO (No such device or address)",
        7 => "E2BIG (Argument list too long)",
        8 => "ENOEXEC (Exec format error)",
        9 => "EBADF (Bad file descriptor)",
        10 => "ECHILD (No child processes)",
        11 => "EAGAIN (Resource temporarily unavailable)",
        12 => "ENOMEM (Cannot allocate memory)",
        13 => "EACCES (Permission denied)",
        14 => "EFAULT (Bad address)",
        16 => "EBUSY (Device or resource busy)",
        17 => "EEXIST (File exists)",
        18 => "EXDEV (Invalid cross-device link)",
        19 => "ENODEV (No such device)",
        20 => "ENOTDIR (Not a directory)",
        21 => "EISDIR (Is a directory)",
        22 => "EINVAL (Invalid argument)",
        24 => "EMFILE (Too many open files)",
        25 => "ENOTTY (Inappropriate ioctl for device)",
        26 => "ETXTBSY (Text file busy)",
        27 => "EFBIG (File too large)",
        28 => "ENOSPC (No space left on device)",
        29 => "ESPIPE (Illegal seek)",
        30 => "EROFS (Read-only file system)",
        32 => "EPIPE (Broken pipe)",
        34 => "ERANGE (Numerical result out of range)",
        36 => "ENAMETOOLONG (File name too long)",
        38 => "ENOSYS (Function not implemented)",
        39 => "ENOTEMPTY (Directory not empty)",
        40 => "ELOOP (Too many levels of symbolic links)",
        61 => "ENODATA (No data available)",
        75 => "EOVERFLOW (Value too large for defined data type)",
        88 => "ENOTSOCK (Socket operation on non-socket)",
        95 => "EOPNOTSUPP (Operation not supported)",
        97 => "EAFNOSUPPORT (Address family not supported by protocol)",
        98 => "EADDRINUSE (Address already in use)",
        101 => "ENETUNREACH (Network is unreachable)",
        104 => "ECONNRESET (Connection reset by peer)",
        110 => "ETIMEDOUT (Connection timed out)",
        111 => "ECONNREFUSED (Connection refused)",
        115 => "EINPROGRESS (Operation now in progress)",
        _ => "E?",
    }
}

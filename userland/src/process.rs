// Copyright (c) 2026 tre4surehunter9
//! Process API: fork, execve, waitpid, and a spawn helper.

use crate::fs::{E2BIG, ENAMETOOLONG};
use crate::sys::{self, syscall3};

pub const WNOHANG: u64 = 1;
/// waitpid option: also report children that stopped (Ctrl-Z, SIGSTOP).
pub const WUNTRACED: u64 = 2;
/// waitpid option: also report stopped children that continued.
pub const WCONTINUED: u64 = 8;

pub fn getppid() -> i32 {
    unsafe { syscall3(sys::GETPPID, 0, 0, 0) as i32 }
}

/// fork(2): returns the child's pid in the parent, 0 in the child, or -errno.
pub fn fork() -> isize {
    unsafe { syscall3(sys::FORK, 0, 0, 0) as isize }
}

/// waitpid(2) via wait4: wait for child `pid` (-1 = any). Returns the pid
/// that changed state (0 with WNOHANG if none yet) and stores its status.
pub fn waitpid(pid: i32, status: &mut i32, options: u64) -> isize {
    // wait4's 4th argument (rusage) must be NULL: the kernel fills it in
    // if it's not, so leaving the register as it happens to be corrupts memory.
    unsafe { crate::sys::syscall4(sys::WAIT4, pid as i64 as u64, status as *mut i32 as u64, options, 0) as isize }
}

// Decoding wait statuses, like the C macros.
pub fn wifexited(status: i32) -> bool {
    status & 0x7f == 0
}
pub fn wexitstatus(status: i32) -> i32 {
    (status >> 8) & 0xff
}
pub fn wifsignaled(status: i32) -> bool {
    let sig = status & 0x7f;
    sig != 0 && sig != 0x7f
}
pub fn wtermsig(status: i32) -> i32 {
    status & 0x7f
}
pub fn wifstopped(status: i32) -> bool {
    status & 0xff == 0x7f
}
pub fn wstopsig(status: i32) -> i32 {
    (status >> 8) & 0xff
}
pub fn wifcontinued(status: i32) -> bool {
    status == 0xffff
}

/// The kernel's limits for execve (argv and envp each, and all the strings).
const MAX_ARGV: usize = 8192;
const MAX_STRING_BYTES: usize = 256 * 1024;

/// execve(2) with Rust strings: copies them into C strings (on the heap:
/// `rm *` in a big directory passes thousands). Only returns on failure,
/// with -errno.
pub fn execve(path: &str, argv: &[&str], envp: &[&str]) -> isize {
    if argv.len() > MAX_ARGV || envp.len() > MAX_ARGV {
        return -E2BIG;
    }
    if path.as_bytes().contains(&0) {
        return -ENAMETOOLONG;
    }
    let total: usize = argv.iter().chain(envp).map(|a| a.len() + 1).sum();
    if total > MAX_STRING_BYTES || argv.iter().chain(envp).any(|a| a.as_bytes().contains(&0)) {
        return -E2BIG;
    }
    // All the strings, NUL-terminated, in one buffer; then pointers into it
    // (taken after it's complete, so they stay valid).
    let mut strings: alloc::vec::Vec<u8> = alloc::vec::Vec::with_capacity(total + path.len() + 1);
    let mut offsets = alloc::vec::Vec::with_capacity(argv.len() + envp.len() + 1);
    for s in core::iter::once(&path).chain(argv).chain(envp) {
        offsets.push(strings.len());
        strings.extend_from_slice(s.as_bytes());
        strings.push(0);
    }
    let base = strings.as_ptr() as u64;
    let ptrs = |range: core::ops::Range<usize>| -> alloc::vec::Vec<u64> {
        let mut v: alloc::vec::Vec<u64> = offsets[range].iter().map(|&o| base + o as u64).collect();
        v.push(0);
        v
    };
    let argv_ptrs = ptrs(1..1 + argv.len());
    let envp_ptrs = ptrs(1 + argv.len()..offsets.len());
    unsafe { syscall3(sys::EXECVE, base, argv_ptrs.as_ptr() as u64, envp_ptrs.as_ptr() as u64) as isize }
}

/// Start `path` with `argv` in a new process (fork + execve). Returns the
/// child's pid, or -errno if fork failed. If execve fails, the child prints
/// an error and exits with 127, like a shell's "command not found".
pub fn spawn(path: &str, argv: &[&str]) -> isize {
    let pid = fork();
    if pid == 0 {
        let r = execve(path, argv, &[]);
        crate::eprintln!("{}: {}", path, crate::fs::strerror(-r));
        crate::exit(127);
    }
    pid
}

/// spawn() and wait for the child. Returns its wait status, or -errno.
pub fn spawn_and_wait(path: &str, argv: &[&str]) -> Result<i32, isize> {
    let pid = spawn(path, argv);
    if pid < 0 {
        return Err(pid);
    }
    let mut status = 0;
    let r = waitpid(pid as i32, &mut status, 0);
    if r < 0 { Err(r) } else { Ok(status) }
}

// Copyright (c) 2026 tre4surehunter9
//! Process API: fork, execve, waitpid, and a spawn helper.

use crate::fs::{E2BIG, ENAMETOOLONG};
use crate::sys::{self, syscall3};

pub const WNOHANG: u64 = 1;

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
    unsafe { syscall3(sys::WAIT4, pid as i64 as u64, status as *mut i32 as u64, options) as isize }
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

const MAX_ARGV: usize = 32;
const STRING_SPACE: usize = 4096;

/// execve(2) with Rust strings: copies them into C strings on the stack.
/// Only returns on failure, with -errno.
pub fn execve(path: &str, argv: &[&str], envp: &[&str]) -> isize {
    if argv.len() > MAX_ARGV || envp.len() > MAX_ARGV {
        return -E2BIG;
    }
    let mut strings = [0u8; STRING_SPACE];
    let mut used = 0usize;
    // Copy `s` plus a NUL into `strings`, returning its address.
    let mut store = |s: &str| -> Option<u64> {
        if used + s.len() + 1 > STRING_SPACE || s.as_bytes().contains(&0) {
            return None;
        }
        strings[used..used + s.len()].copy_from_slice(s.as_bytes());
        strings[used + s.len()] = 0;
        let ptr = strings.as_ptr() as u64 + used as u64;
        used += s.len() + 1;
        Some(ptr)
    };

    let Some(path_ptr) = store(path) else { return -ENAMETOOLONG };
    let mut argv_ptrs = [0u64; MAX_ARGV + 1];
    let mut envp_ptrs = [0u64; MAX_ARGV + 1];
    for (i, a) in argv.iter().enumerate() {
        let Some(p) = store(a) else { return -E2BIG };
        argv_ptrs[i] = p;
    }
    for (i, e) in envp.iter().enumerate() {
        let Some(p) = store(e) else { return -E2BIG };
        envp_ptrs[i] = p;
    }
    unsafe {
        syscall3(sys::EXECVE, path_ptr, argv_ptrs.as_ptr() as u64, envp_ptrs.as_ptr() as u64) as isize
    }
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

// Copyright (c) 2026 tre4surehunter9
//! userlib: the runtime for PalladiumOS user programs.
//!
//! Provides the `_start` entry point, syscall wrappers using the Linux x86_64
//! ABI, `print!`/`println!`, and argument access. A program looks like:
//!
//! ```ignore
//! #![no_std]
//! #![no_main]
//! userlib::entry!(main);
//! fn main(args: userlib::Args) -> i32 { userlib::println!("hi"); 0 }
//! ```
#![no_std]

extern crate alloc;

use core::arch::{asm, global_asm};
use core::ffi::{c_char, CStr};
use core::fmt;

/// Raw syscalls. Numbers match Linux x86_64.
pub mod sys {
    use super::asm;

    pub const READ: u64 = 0;
    pub const WRITE: u64 = 1;
    pub const OPEN: u64 = 2;
    pub const CLOSE: u64 = 3;
    pub const STAT: u64 = 4;
    pub const FSTAT: u64 = 5;
    pub const LSTAT: u64 = 6;
    pub const LSEEK: u64 = 8;
    pub const ACCESS: u64 = 21;
    pub const RENAME: u64 = 82;
    pub const LINK: u64 = 86;
    pub const SYMLINK: u64 = 88;
    pub const READLINK: u64 = 89;
    pub const CHMOD: u64 = 90;
    pub const UMASK: u64 = 95;
    pub const STATFS: u64 = 137;
    pub const UTIMENSAT: u64 = 280;
    pub const PIPE: u64 = 22;
    pub const BRK: u64 = 12;
    pub const RT_SIGACTION: u64 = 13;
    pub const RT_SIGPROCMASK: u64 = 14;
    pub const RT_SIGRETURN: u64 = 15;
    pub const PAUSE: u64 = 34;
    pub const ALARM: u64 = 37;
    pub const SETITIMER: u64 = 38;
    pub const RT_SIGPENDING: u64 = 127;
    pub const RT_SIGSUSPEND: u64 = 130;
    pub const IOCTL: u64 = 16;
    pub const SCHED_YIELD: u64 = 24;
    pub const NANOSLEEP: u64 = 35;
    pub const DUP: u64 = 32;
    pub const DUP2: u64 = 33;
    pub const GETPID: u64 = 39;
    pub const FORK: u64 = 57;
    pub const EXECVE: u64 = 59;
    pub const EXIT: u64 = 60;
    pub const WAIT4: u64 = 61;
    pub const KILL: u64 = 62;
    pub const FCNTL: u64 = 72;
    pub const GETUID: u64 = 102;
    pub const GETGID: u64 = 104;
    pub const SETUID: u64 = 105;
    pub const SETGID: u64 = 106;
    pub const GETEUID: u64 = 107;
    pub const GETEGID: u64 = 108;
    pub const GETGROUPS: u64 = 115;
    pub const SETGROUPS: u64 = 116;
    pub const SETRESUID: u64 = 117;
    pub const SETRESGID: u64 = 119;
    pub const CHOWN: u64 = 92;
    pub const LCHOWN: u64 = 94;
    pub const SETPGID: u64 = 109;
    pub const GETPGRP: u64 = 111;
    pub const GETPPID: u64 = 110;
    pub const SYNC: u64 = 162;
    pub const REBOOT: u64 = 169;
    pub const CLOCK_GETTIME: u64 = 228;
    pub const UNAME: u64 = 63;
    pub const GETCWD: u64 = 79;
    pub const CHDIR: u64 = 80;
    pub const MKDIR: u64 = 83;
    pub const RMDIR: u64 = 84;
    pub const UNLINK: u64 = 87;
    pub const GETDENTS64: u64 = 217;
    pub const OPENAT: u64 = 257;
    pub const PIPE2: u64 = 293;

    /// Invoke syscall `n` with four arguments (the 4th goes in r10).
    #[inline(always)]
    pub unsafe fn syscall4(n: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> i64 {
        let ret: i64;
        unsafe {
            asm!(
                "int 0x80",
                inlateout("rax") n as i64 => ret,
                in("rdi") a1,
                in("rsi") a2,
                in("rdx") a3,
                in("r10") a4,
                options(nostack),
            );
        }
        ret
    }

    /// Invoke syscall `n` with up to three arguments. The kernel preserves
    /// every register except rax, which holds the result (-errno on failure).
    #[inline(always)]
    pub unsafe fn syscall3(n: u64, a1: u64, a2: u64, a3: u64) -> i64 {
        let ret: i64;
        unsafe {
            asm!(
                "int 0x80",
                inlateout("rax") n as i64 => ret,
                in("rdi") a1,
                in("rsi") a2,
                in("rdx") a3,
                options(nostack),
            );
        }
        ret
    }
}

pub mod fs;
pub use fs::*;
pub mod process;
pub use process::*;
pub mod heap;
pub mod tty;
pub use tty::*;
pub mod signal;
pub use signal::*;
pub mod io;
pub mod procinfo;
pub mod users;

pub const STDIN: i32 = 0;
pub const STDOUT: i32 = 1;
pub const STDERR: i32 = 2;

/// write(2). Returns bytes written or -errno.
pub fn write(fd: i32, buf: &[u8]) -> isize {
    unsafe { sys::syscall3(sys::WRITE, fd as u64, buf.as_ptr() as u64, buf.len() as u64) as isize }
}

/// read(2). Returns bytes read (0 at end-of-file) or -errno.
pub fn read(fd: i32, buf: &mut [u8]) -> isize {
    unsafe { sys::syscall3(sys::READ, fd as u64, buf.as_mut_ptr() as u64, buf.len() as u64) as isize }
}

/// _exit(2).
pub fn exit(status: i32) -> ! {
    unsafe { sys::syscall3(sys::EXIT, status as u64, 0, 0) };
    loop {} // unreachable: exit never returns
}

pub fn getpid() -> i32 {
    unsafe { sys::syscall3(sys::GETPID, 0, 0, 0) as i32 }
}

pub fn getuid() -> u32 {
    unsafe { sys::syscall3(sys::GETUID, 0, 0, 0) as u32 }
}
pub fn geteuid() -> u32 {
    unsafe { sys::syscall3(sys::GETEUID, 0, 0, 0) as u32 }
}
pub fn getgid() -> u32 {
    unsafe { sys::syscall3(sys::GETGID, 0, 0, 0) as u32 }
}
pub fn getegid() -> u32 {
    unsafe { sys::syscall3(sys::GETEGID, 0, 0, 0) as u32 }
}
/// setuid(2)
pub fn setuid(uid: u32) -> isize {
    unsafe { sys::syscall3(sys::SETUID, uid as u64, 0, 0) as isize }
}
/// setgid(2)
pub fn setgid(gid: u32) -> isize {
    unsafe { sys::syscall3(sys::SETGID, gid as u64, 0, 0) as isize }
}
/// setresuid(2): u32::MAX leaves an ID unchanged.
pub fn setresuid(r: u32, e: u32, s: u32) -> isize {
    unsafe { sys::syscall3(sys::SETRESUID, r as u64, e as u64, s as u64) as isize }
}
pub fn setresgid(r: u32, e: u32, s: u32) -> isize {
    unsafe { sys::syscall3(sys::SETRESGID, r as u64, e as u64, s as u64) as isize }
}
/// The supplementary groups.
pub fn getgroups() -> alloc::vec::Vec<u32> {
    let mut groups = alloc::vec![0u32; 64];
    let n = unsafe { sys::syscall3(sys::GETGROUPS, 64, groups.as_mut_ptr() as u64, 0) };
    groups.truncate(n.max(0) as usize);
    groups
}
/// setgroups(2): root only.
pub fn setgroups(groups: &[u32]) -> isize {
    unsafe { sys::syscall3(sys::SETGROUPS, groups.len() as u64, groups.as_ptr() as u64, 0) as isize }
}

/// dup(2)
pub fn dup(fd: i32) -> isize {
    unsafe { sys::syscall3(sys::DUP, fd as u64, 0, 0) as isize }
}

/// dup2(2): make `new` refer to the same file as `old`.
pub fn dup2(old: i32, new: i32) -> isize {
    unsafe { sys::syscall3(sys::DUP2, old as u64, new as u64, 0) as isize }
}

/// brk(2): returns the new program break (or the current one on failure).
pub fn brk(addr: u64) -> u64 {
    unsafe { sys::syscall3(sys::BRK, addr, 0, 0) as u64 }
}

pub const REBOOT_RESTART: u64 = 0x01234567;
pub const REBOOT_POWER_OFF: u64 = 0x4321fedc;

/// reboot(2) with the Linux magic numbers. Only returns on error.
pub fn reboot(cmd: u64) -> isize {
    unsafe { sys::syscall3(sys::REBOOT, 0xfee1dead, 672274793, cmd) as isize }
}

/// sync(2): write cached data to the disks.
pub fn sync() {
    unsafe { sys::syscall3(sys::SYNC, 0, 0, 0) };
}

pub fn sched_yield() {
    unsafe { sys::syscall3(sys::SCHED_YIELD, 0, 0, 0) };
}

/// struct utsname, as filled in by uname(2).
#[repr(C)]
pub struct Utsname {
    pub sysname: [u8; 65],
    pub nodename: [u8; 65],
    pub release: [u8; 65],
    pub version: [u8; 65],
    pub machine: [u8; 65],
    pub domainname: [u8; 65],
}

/// Turn a NUL-padded utsname field into a &str.
pub fn field_str(field: &[u8]) -> &str {
    let len = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    core::str::from_utf8(&field[..len]).unwrap_or("?")
}

pub fn uname() -> Result<Utsname, isize> {
    let mut u = Utsname {
        sysname: [0; 65], nodename: [0; 65], release: [0; 65],
        version: [0; 65], machine: [0; 65], domainname: [0; 65],
    };
    let r = unsafe { sys::syscall3(sys::UNAME, &mut u as *mut Utsname as u64, 0, 0) };
    if r < 0 { Err(r as isize) } else { Ok(u) }
}

// ---- formatted output ----

pub struct Fd(pub i32);

impl fmt::Write for Fd {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let mut bytes = s.as_bytes();
        while !bytes.is_empty() {
            let n = write(self.0, bytes);
            if n <= 0 {
                return Err(fmt::Error);
            }
            bytes = &bytes[n as usize..];
        }
        Ok(())
    }
}

#[doc(hidden)]
pub fn _print(fd: i32, args: fmt::Arguments) {
    let _ = fmt::Write::write_fmt(&mut Fd(fd), args);
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::_print($crate::STDOUT, format_args!($($arg)*)));
}

#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}

#[macro_export]
macro_rules! eprintln {
    ($($arg:tt)*) => ($crate::_print($crate::STDERR, format_args!("{}\n", format_args!($($arg)*))));
}

// ---- arguments ----

/// The program's argv, as an iterator of &str (argv[0] is the program name).
#[derive(Clone, Copy)]
pub struct Args {
    argc: usize,
    argv: *const *const c_char,
    index: usize,
}

impl Args {
    /// Number of arguments, including argv[0].
    pub fn count(&self) -> usize {
        self.argc
    }

    pub fn get(&self, i: usize) -> Option<&'static str> {
        if i >= self.argc {
            return None;
        }
        let ptr = unsafe { *self.argv.add(i) };
        if ptr.is_null() {
            return None;
        }
        Some(unsafe { CStr::from_ptr(ptr) }.to_str().unwrap_or("?"))
    }
}

impl Iterator for Args {
    type Item = &'static str;
    fn next(&mut self) -> Option<&'static str> {
        let arg = self.get(self.index)?;
        self.index += 1;
        Some(arg)
    }
}

// ---- environment ----

static mut ENVP: *const *const c_char = core::ptr::null();

/// The environment as "NAME=value" strings.
pub fn environ() -> impl Iterator<Item = &'static str> {
    let mut p = unsafe { ENVP };
    core::iter::from_fn(move || unsafe {
        if p.is_null() || (*p).is_null() {
            return None;
        }
        let s = CStr::from_ptr(*p).to_str().unwrap_or("");
        p = p.add(1);
        Some(s)
    })
}

/// Look up an environment variable, like getenv(3).
pub fn getenv(name: &str) -> Option<&'static str> {
    environ().find_map(|kv| kv.strip_prefix(name)?.strip_prefix('='))
}

// ---- entry point ----

unsafe extern "Rust" {
    fn __userlib_main(args: Args) -> i32;
}

// The kernel enters at _start with rsp pointing at argc (System V ABI).
global_asm!(
    ".global _start",
    "_start:",
    "mov rdi, rsp",
    "and rsp, -16",
    "call {start}",
    "ud2",
    start = sym userlib_start,
);

extern "C" fn userlib_start(sp: *const u64) -> ! {
    let argc = unsafe { *sp } as usize;
    let argv = unsafe { sp.add(1) } as *const *const c_char;
    // envp follows argv's NULL terminator
    unsafe { ENVP = argv.add(argc + 1) };
    let status = unsafe { __userlib_main(Args { argc, argv, index: 0 }) };
    exit(status)
}

/// Declare the program's main function: `fn main(args: Args) -> i32`.
#[macro_export]
macro_rules! entry {
    ($main:path) => {
        #[unsafe(no_mangle)]
        pub fn __userlib_main(args: $crate::Args) -> i32 {
            let f: fn($crate::Args) -> i32 = $main;
            f(args)
        }
    };
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    eprintln!("panic: {}", info);
    exit(101)
}

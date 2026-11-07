// Copyright (c) 2026 tre4surehunter9
//! System call layer.
//!
//! Uses the Linux x86_64 syscall ABI so that a real libc (musl / relibc) can
//! be ported later:
//!   rax = syscall number
//!   rdi, rsi, rdx, r10, r8, r9 = arguments 1-6
//!   rax = return value (negative errno on failure)
//! Every other register is preserved across the call.
//!
//! Syscalls are entered with `int 0x80` for now.

use core::arch::global_asm;
use x86_64::structures::idt::InterruptDescriptorTable;
use x86_64::{PrivilegeLevel, VirtAddr};
use crate::{process, vfs};
use alloc::vec::Vec;

// ---- Syscall numbers (Linux x86_64) ----
pub const SYS_READ: u64 = 0;
pub const SYS_WRITE: u64 = 1;
pub const SYS_OPEN: u64 = 2;
pub const SYS_CLOSE: u64 = 3;
pub const SYS_STAT: u64 = 4;
pub const SYS_FSTAT: u64 = 5;
pub const SYS_LSTAT: u64 = 6;
pub const SYS_LSEEK: u64 = 8;
pub const SYS_PIPE: u64 = 22;
pub const SYS_BRK: u64 = 12;
pub const SYS_IOCTL: u64 = 16;
pub const SYS_RT_SIGACTION: u64 = 13;
pub const SYS_RT_SIGPROCMASK: u64 = 14;
pub const SYS_SCHED_YIELD: u64 = 24;
pub const SYS_NANOSLEEP: u64 = 35;
pub const SYS_DUP: u64 = 32;
pub const SYS_DUP2: u64 = 33;
pub const SYS_GETPID: u64 = 39;
pub const SYS_FORK: u64 = 57;
pub const SYS_VFORK: u64 = 58;
pub const SYS_EXECVE: u64 = 59;
pub const SYS_EXIT: u64 = 60;
pub const SYS_WAIT4: u64 = 61;
pub const SYS_KILL: u64 = 62;
pub const SYS_FCNTL: u64 = 72;
pub const SYS_RT_SIGRETURN: u64 = 15;
pub const SYS_POLL: u64 = 7;
pub const SYS_MMAP: u64 = 9;
pub const SYS_MPROTECT: u64 = 10;
pub const SYS_MUNMAP: u64 = 11;
pub const SYS_PREAD64: u64 = 17;
pub const SYS_PWRITE64: u64 = 18;
pub const SYS_READV: u64 = 19;
pub const SYS_WRITEV: u64 = 20;
pub const SYS_MREMAP: u64 = 25;
pub const SYS_MSYNC: u64 = 26;
pub const SYS_MADVISE: u64 = 28;
pub const SYS_FSYNC: u64 = 74;
pub const SYS_FDATASYNC: u64 = 75;
pub const SYS_TRUNCATE: u64 = 76;
pub const SYS_FTRUNCATE: u64 = 77;
pub const SYS_GETRLIMIT: u64 = 97;
pub const SYS_GETRUSAGE: u64 = 98;
pub const SYS_ARCH_PRCTL: u64 = 158;
pub const SYS_SETRLIMIT: u64 = 160;
pub const SYS_SCHED_GETAFFINITY: u64 = 204;
pub const SYS_SET_TID_ADDRESS: u64 = 218;
pub const SYS_FADVISE64: u64 = 221;
pub const SYS_CLOCK_NANOSLEEP: u64 = 230;
pub const SYS_PPOLL: u64 = 271;
pub const SYS_SET_ROBUST_LIST: u64 = 273;
pub const SYS_PRLIMIT64: u64 = 302;
pub const SYS_GETRANDOM: u64 = 318;
pub const SYS_CLONE: u64 = 56;
pub const SYS_RSEQ: u64 = 334;
pub const SYS_FUTEX: u64 = 202;
pub const SYS_SOCKET: u64 = 41;
pub const SYS_CONNECT: u64 = 42;
pub const SYS_ACCEPT: u64 = 43;
pub const SYS_SENDTO: u64 = 44;
pub const SYS_RECVFROM: u64 = 45;
pub const SYS_SHUTDOWN: u64 = 48;
pub const SYS_SOCKETPAIR: u64 = 53;
pub const SYS_SETSID: u64 = 112;
pub const SYS_GETSID: u64 = 124;
pub const SYS_SELECT: u64 = 23;
pub const SYS_MEMBARRIER: u64 = 324;
pub const SYS_PSELECT6: u64 = 270;
pub const SYS_SENDMSG: u64 = 46;
pub const SYS_RECVMSG: u64 = 47;
pub const SYS_BIND: u64 = 49;
pub const SYS_LISTEN: u64 = 50;
pub const SYS_GETSOCKNAME: u64 = 51;
pub const SYS_GETPEERNAME: u64 = 52;
pub const SYS_SETSOCKOPT: u64 = 54;
pub const SYS_GETSOCKOPT: u64 = 55;
pub const SYS_ACCEPT4: u64 = 288;
pub const SYS_SENDFILE: u64 = 40;
pub const SYS_STATX: u64 = 332;
pub const EAFNOSUPPORT: i64 = 97;
pub const EPROTOTYPE: i64 = 91;
pub const EOPNOTSUPP: i64 = 95;
pub const EMSGSIZE: i64 = 90;
pub const ENOTSOCK: i64 = 88;
pub const EPROTONOSUPPORT: i64 = 93;
pub const SYS_CLOCK_GETRES: u64 = 229;
pub const SYS_PAUSE: u64 = 34;
pub const SYS_GETITIMER: u64 = 36;
pub const SYS_ALARM: u64 = 37;
pub const SYS_SETITIMER: u64 = 38;
pub const SYS_RT_SIGPENDING: u64 = 127;
pub const SYS_RT_SIGSUSPEND: u64 = 130;
pub const SYS_GETTID: u64 = 186;
pub const SYS_TKILL: u64 = 200;
pub const SYS_TGKILL: u64 = 234;
pub const SYS_GETUID: u64 = 102;
pub const SYS_GETGID: u64 = 104;
pub const SYS_SETUID: u64 = 105;
pub const SYS_SETGID: u64 = 106;
pub const SYS_GETEUID: u64 = 107;
pub const SYS_GETEGID: u64 = 108;
pub const SYS_SETREUID: u64 = 113;
pub const SYS_SETREGID: u64 = 114;
pub const SYS_GETGROUPS: u64 = 115;
pub const SYS_SETGROUPS: u64 = 116;
pub const SYS_SETRESUID: u64 = 117;
pub const SYS_GETRESUID: u64 = 118;
pub const SYS_SETRESGID: u64 = 119;
pub const SYS_GETRESGID: u64 = 120;
pub const SYS_CHOWN: u64 = 92;
pub const SYS_FCHOWN: u64 = 93;
pub const SYS_LCHOWN: u64 = 94;
pub const SYS_FCHOWNAT: u64 = 260;
pub const SYS_SETPGID: u64 = 109;
pub const SYS_GETPGRP: u64 = 111;
pub const SYS_GETPGID: u64 = 121;
pub const SYS_GETPPID: u64 = 110;
pub const SYS_UNAME: u64 = 63;
pub const SYS_GETCWD: u64 = 79;
pub const SYS_CHDIR: u64 = 80;
pub const SYS_MKDIR: u64 = 83;
pub const SYS_RMDIR: u64 = 84;
pub const SYS_RENAME: u64 = 82;
pub const SYS_LINK: u64 = 86;
pub const SYS_SYMLINK: u64 = 88;
pub const SYS_READLINK: u64 = 89;
pub const SYS_CHMOD: u64 = 90;
pub const SYS_FCHMOD: u64 = 91;
pub const SYS_UMASK: u64 = 95;
pub const SYS_ACCESS: u64 = 21;
pub const SYS_STATFS: u64 = 137;
pub const SYS_FSTATFS: u64 = 138;
pub const SYS_MKDIRAT: u64 = 258;
pub const SYS_NEWFSTATAT: u64 = 262;
pub const SYS_UNLINKAT: u64 = 263;
pub const SYS_RENAMEAT: u64 = 264;
pub const SYS_LINKAT: u64 = 265;
pub const SYS_SYMLINKAT: u64 = 266;
pub const SYS_READLINKAT: u64 = 267;
pub const SYS_FCHMODAT: u64 = 268;
pub const SYS_FACCESSAT: u64 = 269;
pub const SYS_UTIMENSAT: u64 = 280;
pub const SYS_RENAMEAT2: u64 = 316;
pub const SYS_FACCESSAT2: u64 = 439;
pub const SYS_UNLINK: u64 = 87;
pub const SYS_SYNC: u64 = 162;
pub const SYS_REBOOT: u64 = 169;
pub const SYS_GETDENTS64: u64 = 217;
pub const SYS_CLOCK_GETTIME: u64 = 228;
pub const SYS_EXIT_GROUP: u64 = 231;
pub const SYS_OPENAT: u64 = 257;
pub const SYS_DUP3: u64 = 292;
pub const SYS_PIPE2: u64 = 293;

// ---- errno values (Linux) ----
pub const EPERM: i64 = 1;
pub const ENOENT: i64 = 2;
pub const ESRCH: i64 = 3;
pub const EINTR: i64 = 4;
pub const E2BIG: i64 = 7;
pub const ENOEXEC: i64 = 8;
pub const EIO: i64 = 5;
pub const EBADF: i64 = 9;
pub const ECHILD: i64 = 10;
pub const EAGAIN: i64 = 11;
pub const ENOMEM: i64 = 12;
pub const ENODEV: i64 = 19;
pub const EACCES: i64 = 13;
pub const EFAULT: i64 = 14;
pub const EBUSY: i64 = 16;
pub const EEXIST: i64 = 17;
pub const ENOTDIR: i64 = 20;
pub const EISDIR: i64 = 21;
pub const EINVAL: i64 = 22;
pub const EMFILE: i64 = 24;
pub const ENOTTY: i64 = 25;
pub const ENXIO: i64 = 6;
pub const ENOSPC: i64 = 28;
pub const EFBIG: i64 = 27;
pub const ESPIPE: i64 = 29;
pub const EPIPE: i64 = 32;
pub const ERANGE: i64 = 34;
pub const ENAMETOOLONG: i64 = 36;
pub const ENOSYS: i64 = 38;
pub const ENOTEMPTY: i64 = 39;

/// The full user register state saved on the kernel stack by
/// `syscall_entry`, lowest address first: the 15 general-purpose registers
/// we push (in reverse order), then the 5 words the CPU pushed on entry.
/// fork() copies this to the child; execve() rewrites it.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SyscallFrame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub rbp: u64,
    pub rbx: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rax: u64,
    // pushed by the CPU
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

impl SyscallFrame {
    /// A frame that "returns" to ring 3 at `rip` with stack `rsp` and all
    /// other registers zero (used for new programs).
    pub fn new_user(rip: u64, rsp: u64) -> Self {
        let (code_sel, data_sel) = crate::gdt::user_selectors();
        SyscallFrame {
            rip,
            rsp,
            // RPL 3 = "requested privilege level 3", the low 2 bits of the selector
            cs: (code_sel.0 as u64) | 3,
            ss: (data_sel.0 as u64) | 3,
            rflags: 0x202, // IF set (+ reserved bit 1)
            ..Default::default()
        }
    }
}

// The CPU has already pushed SS, RSP, RFLAGS, CS, RIP (40 bytes) onto the
// 16-byte-aligned RSP0 stack. We push all 15 general-purpose registers
// (120 bytes), 160 in total, so RSP is still 16-byte aligned at the `call`.
//
// `syscall_return` is also where brand-new processes start: the scheduler
// builds a SyscallFrame on their kernel stack and "returns" through here.
global_asm!(
    ".global syscall_entry",
    ".global syscall_return",
    "syscall_entry:",
    "push rax",
    "push rcx",
    "push rdx",
    "push rsi",
    "push rdi",
    "push r8",
    "push r9",
    "push r10",
    "push r11",
    "push rbx",
    "push rbp",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    "cld",                   // the program may have left the direction flag set
    "mov rdi, rsp",          // arg0 = &mut SyscallFrame
    "call syscall_dispatch",
    "syscall_return:",
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop rbp",
    "pop rbx",
    "pop r11",
    "pop r10",
    "pop r9",
    "pop r8",
    "pop rdi",
    "pop rsi",
    "pop rdx",
    "pop rcx",
    "pop rax",               // return value, written into frame.rax
    "iretq",
);

// The `syscall` instruction (what Linux programs and musl use; ours use
// `int 0x80`). Unlike an interrupt, it doesn't switch stacks or save
// anything but rip (in rcx) and rflags (in r11), so this builds the same
// SyscallFrame by hand on the kernel stack, and returns with iretq like
// the int 0x80 path. Interrupts stay off on entry (SFMASK clears IF), and
// there's one CPU, so two statics hold the stack pointers.
global_asm!(
    ".global syscall_fast_entry",
    "syscall_fast_entry:",
    "mov [rip + SYSCALL_USER_RSP], rsp",
    "mov rsp, [rip + SYSCALL_KERNEL_RSP]",
    "push qword ptr [rip + SYSCALL_USER_SS]",
    "push qword ptr [rip + SYSCALL_USER_RSP]",
    "push r11",              // rflags
    "push qword ptr [rip + SYSCALL_USER_CS]",
    "push rcx",              // rip
    "push rax",
    "push rcx",
    "push rdx",
    "push rsi",
    "push rdi",
    "push r8",
    "push r9",
    "push r10",
    "push r11",
    "push rbx",
    "push rbp",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    "cld",
    "mov rdi, rsp",
    "call syscall_dispatch",
    "jmp syscall_return",
);

/// Top of the running process's kernel stack, for `syscall` (kept equal to
/// the TSS's RSP0 by gdt::set_kernel_stack).
#[unsafe(no_mangle)]
pub static mut SYSCALL_KERNEL_RSP: u64 = 0;
#[unsafe(no_mangle)]
static mut SYSCALL_USER_RSP: u64 = 0;
#[unsafe(no_mangle)]
static mut SYSCALL_USER_CS: u64 = 0;
#[unsafe(no_mangle)]
static mut SYSCALL_USER_SS: u64 = 0;

unsafe extern "C" {
    fn syscall_entry();
    fn syscall_return();
    fn syscall_fast_entry();
}

/// Turn on the `syscall` instruction: EFER.SCE, its entry point (LSTAR),
/// the kernel segments (STAR) and the flags to clear on entry (SFMASK).
pub fn init_fast_syscalls() {
    use x86_64::registers::model_specific::Msr;
    let (code, data) = crate::gdt::user_selectors();
    unsafe {
        SYSCALL_USER_CS = (code.0 | 3) as u64;
        SYSCALL_USER_SS = (data.0 | 3) as u64;
        if SYSCALL_KERNEL_RSP == 0 {
            SYSCALL_KERNEL_RSP = crate::gdt::kernel_stack_top();
        }
        let mut efer = Msr::new(0xC000_0080);
        efer.write(efer.read() | 1); // SCE: syscall enable
        // STAR[47:32]: kernel CS (SS = CS + 8). The sysret half is unused
        // (we return with iretq).
        Msr::new(0xC000_0081).write(0x08u64 << 32);
        Msr::new(0xC000_0082).write(syscall_fast_entry as *const () as u64); // LSTAR
        // SFMASK: clear TF, IF, DF, IOPL, NT and AC on entry.
        Msr::new(0xC000_0084).write(0x100 | 0x200 | 0x400 | 0x3000 | 0x4000 | 0x40000);
    }
}

/// Address new processes start at (see the asm above).
pub fn syscall_return_addr() -> u64 {
    syscall_return as *const () as u64
}

pub fn init_syscalls(idt: &mut InterruptDescriptorTable) {
    unsafe {
        idt[0x80]
            .set_handler_addr(VirtAddr::new(syscall_entry as *const () as u64))
            .set_privilege_level(PrivilegeLevel::Ring3);
    }
}

#[unsafe(no_mangle)]
extern "C" fn syscall_dispatch(frame: &mut SyscallFrame) {
    let (a1, a2, a3, a4, a5) = (frame.rdi, frame.rsi, frame.rdx, frame.r10, frame.r8);
    const FDCWD: u64 = -100i64 as u64;
    let num = frame.rax;

    let ret: i64 = match num {
        SYS_READ => match unsafe { user_slice_mut(a2, a3) } {
            Some(buf) => vfs::sys_read(a1, buf),
            None => -EFAULT,
        },
        SYS_WRITE => match unsafe { user_slice(a2, a3) } {
            Some(buf) => vfs::sys_write(a1, buf),
            None => -EFAULT,
        },
        SYS_OPEN => vfs::sys_open(a1, a2, a3),
        SYS_OPENAT => vfs::sys_openat(a1, a2, a3, a4),
        SYS_CLOSE => vfs::sys_close(a1),
        SYS_LSEEK => vfs::sys_lseek(a1, a2, a3),
        SYS_STAT | SYS_LSTAT => match unsafe { user_slice_mut(a2, vfs::STAT_SIZE) } {
            Some(buf) => vfs::sys_stat(a1, buf, frame.rax == SYS_STAT),
            None => -EFAULT,
        },
        SYS_NEWFSTATAT => match unsafe { user_slice_mut(a3, vfs::STAT_SIZE) } {
            Some(buf) => vfs::sys_fstatat(a1, a2, buf, a4),
            None => -EFAULT,
        },
        SYS_FSTAT => match unsafe { user_slice_mut(a2, vfs::STAT_SIZE) } {
            Some(buf) => vfs::sys_fstat(a1, buf),
            None => -EFAULT,
        },
        SYS_GETDENTS64 => match unsafe { user_slice_mut(a2, a3) } {
            Some(buf) => vfs::sys_getdents64(a1, buf),
            None => -EFAULT,
        },
        SYS_GETCWD => match unsafe { user_slice_mut(a1, a2) } {
            Some(buf) => vfs::sys_getcwd(buf),
            None => -EFAULT,
        },
        SYS_CHDIR => vfs::sys_chdir(a1),
        SYS_MKDIR => vfs::sys_mkdirat(FDCWD, a1, a2),
        SYS_MKDIRAT => vfs::sys_mkdirat(a1, a2, a3),
        SYS_UNLINKAT => vfs::sys_unlinkat(a1, a2, a3),
        SYS_RENAME => vfs::sys_renameat(FDCWD, a1, FDCWD, a2, 0),
        SYS_RENAMEAT => vfs::sys_renameat(a1, a2, a3, a4, 0),
        SYS_RENAMEAT2 => vfs::sys_renameat(a1, a2, a3, a4, a5),
        SYS_LINK => vfs::sys_linkat(FDCWD, a1, FDCWD, a2, 0),
        SYS_LINKAT => vfs::sys_linkat(a1, a2, a3, a4, a5),
        SYS_SYMLINK => vfs::sys_symlinkat(a1, FDCWD, a2),
        SYS_SYMLINKAT => vfs::sys_symlinkat(a1, a2, a3),
        SYS_READLINK => match unsafe { user_slice_mut(a2, a3) } {
            Some(buf) => vfs::sys_readlinkat(FDCWD, a1, buf),
            None => -EFAULT,
        },
        SYS_READLINKAT => match unsafe { user_slice_mut(a3, a4) } {
            Some(buf) => vfs::sys_readlinkat(a1, a2, buf),
            None => -EFAULT,
        },
        SYS_CHMOD => vfs::sys_fchmodat(FDCWD, a1, a2),
        SYS_FCHMODAT => vfs::sys_fchmodat(a1, a2, a3),
        SYS_FCHMOD => vfs::sys_fchmod(a1, a2),
        SYS_ACCESS => vfs::sys_faccessat(FDCWD, a1, a2),
        SYS_FACCESSAT | SYS_FACCESSAT2 => vfs::sys_faccessat(a1, a2, a3),
        SYS_UMASK => vfs::sys_umask(a1),
        SYS_STATFS => match unsafe { user_slice_mut(a2, vfs::STATFS_SIZE) } {
            Some(buf) => vfs::sys_statfs(Some(a1), 0, buf),
            None => -EFAULT,
        },
        SYS_FSTATFS => match unsafe { user_slice_mut(a2, vfs::STATFS_SIZE) } {
            Some(buf) => vfs::sys_statfs(None, a1, buf),
            None => -EFAULT,
        },
        SYS_UTIMENSAT => vfs::sys_utimensat(a1, a2, a3, a4),
        SYS_RMDIR => vfs::sys_remove(a1, true),
        SYS_UNLINK => vfs::sys_remove(a1, false),
        SYS_IOCTL => vfs::sys_ioctl(a1, a2, a3),
        SYS_DUP => vfs::sys_dup(a1),
        SYS_DUP2 => vfs::sys_dup3(a1, a2, 0, false),
        SYS_DUP3 => vfs::sys_dup3(a1, a2, a3, true),
        SYS_PIPE => vfs::sys_pipe2(a1, 0),
        SYS_PIPE2 => vfs::sys_pipe2(a1, a2),
        SYS_FCNTL => vfs::sys_fcntl(a1, a2, a3),
        SYS_BRK => process::sys_brk(a1),
        SYS_SYNC => vfs::sys_sync(),
        SYS_REBOOT => sys_reboot(a1, a2, a3),
        SYS_SCHED_YIELD => {
            process::yield_now();
            0
        }
        SYS_GETPID => process::current_tgid() as i64,
        SYS_GETPPID => process::current_ppid() as i64,
        SYS_UNAME => sys_uname(a1),
        // vfork may be implemented as fork (POSIX allows it).
        SYS_FORK | SYS_VFORK => process::fork(frame),
        SYS_EXECVE => process::execve(frame, a1, a2, a3),
        SYS_WAIT4 => process::sys_wait4(a1, a2, a3, a4),
        SYS_KILL => process::sys_kill(a1, a2),
        SYS_GETUID | SYS_GETEUID | SYS_GETGID | SYS_GETEGID | SYS_SETUID | SYS_SETGID | SYS_SETREUID
        | SYS_SETREGID | SYS_SETRESUID | SYS_SETRESGID | SYS_GETRESUID | SYS_GETRESGID => process::sys_id(frame.rax, a1, a2, a3),
        SYS_GETGROUPS => process::sys_getgroups(a1, a2),
        SYS_SETGROUPS => process::sys_setgroups(a1, a2),
        SYS_CHOWN => vfs::sys_fchownat(FDCWD, a1, a2, a3, 0),
        SYS_LCHOWN => vfs::sys_fchownat(FDCWD, a1, a2, a3, 0x100),
        SYS_FCHOWNAT => vfs::sys_fchownat(a1, a2, a3, a4, a5),
        SYS_FCHOWN => vfs::sys_fchown(a1, a2, a3),
        SYS_SETPGID => process::sys_setpgid(a1, a2),
        SYS_GETPGID => process::sys_getpgid(a1),
        SYS_GETPGRP => process::sys_getpgid(0),
        SYS_RT_SIGACTION => process::sys_rt_sigaction(a1, a2, a3),
        SYS_RT_SIGPROCMASK => process::sys_rt_sigprocmask(a1, a2, a3),
        SYS_RT_SIGRETURN => process::sys_rt_sigreturn(frame),
        SYS_RT_SIGPENDING => process::sys_rt_sigpending(a1),
        SYS_RT_SIGSUSPEND => process::sys_rt_sigsuspend(Some(a1)),
        SYS_PAUSE => process::sys_rt_sigsuspend(None),
        SYS_ALARM => process::sys_alarm(a1),
        SYS_GETITIMER => process::sys_itimer(a1, 0, a2),
        SYS_SETITIMER => process::sys_itimer(a1, a2, a3),
        SYS_GETTID => process::current_pid() as i64,
        SYS_TKILL => process::sys_tkill(0, a1, a2),
        SYS_TGKILL => process::sys_tkill(a1, a2, a3),
        SYS_NANOSLEEP => process::sys_nanosleep(a1, a2),
        SYS_CLOCK_GETTIME => process::sys_clock_gettime(a1, a2),
        SYS_EXIT => process::exit_thread(a1 as i32),
        SYS_EXIT_GROUP => process::exit_group(process::ExitReason::Exited(a1 as i32)),
        // memory
        SYS_MMAP => crate::mmap::sys_mmap(a1, a2, a3, a4, a5, frame.r9),
        SYS_MUNMAP => crate::mmap::sys_munmap(a1, a2),
        SYS_MPROTECT => crate::mmap::sys_mprotect(a1, a2, a3),
        SYS_MREMAP => -ENOMEM, // libc falls back to allocate + copy
        SYS_MADVISE | SYS_MSYNC => 0,
        // files
        SYS_PREAD64 => match unsafe { user_slice_mut(a2, a3) } {
            Some(buf) => vfs::sys_pread(a1, buf, a4),
            None => -EFAULT,
        },
        SYS_PWRITE64 => match unsafe { user_slice(a2, a3) } {
            Some(buf) => vfs::sys_pwrite(a1, buf, a4),
            None => -EFAULT,
        },
        SYS_READV => sys_iov(a1, a2, a3, false),
        SYS_WRITEV => sys_iov(a1, a2, a3, true),
        SYS_POLL => sys_poll(a1, a2, a3 as i32 as i64),
        SYS_PPOLL => sys_ppoll(a1, a2, a3),
        SYS_FTRUNCATE => vfs::sys_ftruncate(a1, a2),
        SYS_TRUNCATE => vfs::sys_truncate(a1, a2),
        SYS_FSYNC | SYS_FDATASYNC => vfs::sys_sync(),
        SYS_FADVISE64 => 0,
        // threads (one per process) and TLS
        SYS_ARCH_PRCTL => process::sys_arch_prctl(a1, a2),
        SYS_SET_TID_ADDRESS => process::sys_set_tid_address(a1),
        SYS_FUTEX => process::sys_futex(a1, a2, a3, a4, a5, frame.r9),
        SYS_SET_ROBUST_LIST => 0,
        // Restartable sequences: optional (glibc tries it and carries on).
        SYS_RSEQ => -ENOSYS,
        SYS_SCHED_GETAFFINITY => sys_sched_getaffinity(a2, a3),
        // limits, time, randomness
        SYS_GETRLIMIT => sys_prlimit(a1, 0, a2),
        SYS_PRLIMIT64 => sys_prlimit(a2, a3, a4),
        SYS_SETRLIMIT => 0,
        SYS_GETRUSAGE => match unsafe { user_slice_mut(a2, 144) } {
            Some(out) => {
                out.fill(0);
                0
            }
            None => -EFAULT,
        },
        SYS_CLOCK_NANOSLEEP => process::sys_clock_nanosleep(a1, a2, a3, a4),
        SYS_CLOCK_GETRES => process::sys_clock_getres(a1, a2),
        SYS_CLONE => process::sys_clone(frame, a1, a2, a3, a4, a5),
        SYS_STATX => vfs::sys_statx(a1, a2, a3, a4, a5),
        SYS_SENDFILE => sys_sendfile(a1, a2, a3, a4),
        SYS_SOCKET => vfs::sys_socket(a1, a2, a3),
        SYS_BIND => vfs::sys_bind(a1, a2, a3),
        SYS_LISTEN => vfs::sys_listen(a1, a2),
        SYS_CONNECT => vfs::sys_connect(a1, a2, a3),
        SYS_ACCEPT => vfs::sys_accept(a1, a2, a3, 0),
        SYS_ACCEPT4 => vfs::sys_accept(a1, a2, a3, a4),
        SYS_SENDTO => vfs::sys_sendto(a1, a2, a3, a4, a5, frame.r9),
        SYS_RECVFROM => vfs::sys_recvfrom(a1, a2, a3, a4, a5, frame.r9),
        SYS_SHUTDOWN => vfs::sys_shutdown(a1, a2),
        SYS_SOCKETPAIR => vfs::sys_socketpair(a1, a2, a3, a4),
        SYS_SETSID => process::sys_setsid(),
        SYS_GETSID => process::sys_getsid(a1),
        SYS_SELECT => sys_select(a1, a2, a3, a4, a5, false),
        SYS_MEMBARRIER => sys_membarrier(a1),
        SYS_PSELECT6 => sys_select(a1, a2, a3, a4, a5, true),
        SYS_SENDMSG => vfs::sys_sendmsg(a1, a2, a3),
        SYS_RECVMSG => vfs::sys_recvmsg(a1, a2, a3),
        SYS_GETSOCKNAME => vfs::sys_getsockname(a1, a2, a3),
        SYS_GETPEERNAME => vfs::sys_getpeername(a1, a2, a3),
        SYS_SETSOCKOPT => vfs::sys_setsockopt(a1, a2, a3, a4, a5),
        SYS_GETSOCKOPT => vfs::sys_getsockopt(a1, a2, a3, a4, a5),
        SYS_GETRANDOM => match unsafe { user_slice_mut(a1, a2.min(1 << 20)) } {
            Some(buf) => {
                crate::random::fill(buf);
                buf.len() as i64
            }
            None => -EFAULT,
        },
        _ => {
            report_unimplemented(num);
            -ENOSYS
        }
    };

    frame.rax = ret as u64;
    // Signals take effect before returning to the program (a handler may
    // run first). After sigreturn, there's no syscall to restart.
    let syscall = (num != SYS_RT_SIGRETURN).then_some((num, ret));
    process::handle_signals(frame, syscall);
}

/// Say once per syscall number that a program used one we don't have
/// (handy when porting software).
fn report_unimplemented(num: u64) {
    use core::sync::atomic::{AtomicU64, Ordering};
    static SEEN: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];
    if num < 512 {
        let (word, bit) = ((num / 64) as usize, 1u64 << (num % 64));
        if SEEN[word].fetch_or(bit, Ordering::Relaxed) & bit == 0 {
            crate::serial_println!("[syscall {} not implemented, used by pid {}]", num, process::current_pid());
            crate::println!("[syscall {} not implemented, used by pid {}]", num, process::current_pid());
        }
    }
}

/// sendfile(out_fd, in_fd, offset, count): copy between files in the
/// kernel. With `offset`, read from there (and update it) without moving
/// in_fd's position.
fn sys_sendfile(out_fd: u64, in_fd: u64, offset_ptr: u64, count: u64) -> i64 {
    let mut offset = if offset_ptr != 0 {
        match unsafe { user_slice(offset_ptr, 8) } {
            Some(b) => Some(u64::from_le_bytes(b.try_into().unwrap())),
            None => return -EFAULT,
        }
    } else {
        None
    };
    let mut buf = alloc::vec![0u8; count.min(64 * 1024) as usize];
    let mut total = 0u64;
    while total < count {
        let want = (count - total).min(buf.len() as u64) as usize;
        let n = match offset {
            Some(off) => vfs::sys_pread(in_fd, &mut buf[..want], off),
            None => vfs::sys_read(in_fd, &mut buf[..want]),
        };
        if n < 0 {
            return if total > 0 { total as i64 } else { n };
        }
        if n == 0 {
            break; // end of the input
        }
        let mut done = 0usize;
        while done < n as usize {
            let w = vfs::sys_write(out_fd, &buf[done..n as usize]);
            if w <= 0 {
                return if total + (done as u64) > 0 { (total + done as u64) as i64 } else { w };
            }
            done += w as usize;
        }
        total += n as u64;
        if let Some(off) = offset.as_mut() {
            *off += n as u64;
        }
        if (n as usize) < want {
            break;
        }
    }
    if let (Some(off), Some(out)) = (offset, unsafe { user_slice_mut(offset_ptr, 8) }.filter(|_| offset_ptr != 0)) {
        out.copy_from_slice(&off.to_le_bytes());
    }
    total as i64
}

/// readv/writev(fd, iov, iovcnt): one read/write per buffer, stopping at
/// a short one.
fn sys_iov(fd: u64, iov: u64, count: u64, write: bool) -> i64 {
    if count > 1024 {
        return -EINVAL;
    }
    let Some(list) = (unsafe { user_slice(iov, count * 16) }) else { return -EFAULT };
    let list: Vec<(u64, u64)> = list
        .chunks(16)
        .map(|c| (u64::from_le_bytes(c[0..8].try_into().unwrap()), u64::from_le_bytes(c[8..16].try_into().unwrap())))
        .collect();
    let mut total = 0i64;
    for (base, len) in list {
        if len == 0 {
            continue;
        }
        let r = if write {
            match unsafe { user_slice(base, len) } {
                Some(buf) => vfs::sys_write(fd, buf),
                None => -EFAULT,
            }
        } else {
            match unsafe { user_slice_mut(base, len) } {
                Some(buf) => vfs::sys_read(fd, buf),
                None => -EFAULT,
            }
        };
        if r < 0 {
            return if total > 0 { total } else { r };
        }
        total += r;
        if (r as u64) < len {
            break;
        }
    }
    total
}

/// poll(fds, nfds, timeout ms; -1 = forever)
fn sys_poll(fds: u64, nfds: u64, timeout_ms: i64) -> i64 {
    let deadline = (timeout_ms > 0).then(|| process::ticks() + (timeout_ms as u64 * 1_000_000).div_ceil(process::TICK_NS));
    poll_until(fds, nfds, timeout_ms == 0, deadline)
}

/// ppoll(fds, nfds, timeout timespec or NULL, sigmask, size) (the mask is
/// not applied)
fn sys_ppoll(fds: u64, nfds: u64, ts: u64) -> i64 {
    if ts == 0 {
        return poll_until(fds, nfds, false, None);
    }
    let Some(t) = (unsafe { user_slice(ts, 16) }) else { return -EFAULT };
    let sec = u64::from_le_bytes(t[0..8].try_into().unwrap());
    let nsec = u64::from_le_bytes(t[8..16].try_into().unwrap());
    let ns = sec.saturating_mul(1_000_000_000).saturating_add(nsec);
    let deadline = (ns > 0).then(|| process::ticks() + ns.div_ceil(process::TICK_NS));
    poll_until(fds, nfds, ns == 0, deadline)
}

fn poll_until(fds: u64, nfds: u64, no_wait: bool, deadline: Option<u64>) -> i64 {
    if nfds > vfs::MAX_FDS as u64 * 4 {
        return -EINVAL;
    }
    loop {
        // Waiting on sockets: the network stack must run to make them ready.
        crate::net::poll_if_due();
        let Some(list) = (unsafe { user_slice_mut(fds, nfds * 8) }) else { return -EFAULT };
        let mut ready = 0;
        for entry in list.chunks_mut(8) {
            let fd = i32::from_le_bytes(entry[0..4].try_into().unwrap());
            let events = i16::from_le_bytes(entry[4..6].try_into().unwrap());
            let revents = vfs::poll_ready(fd, events);
            entry[6..8].copy_from_slice(&revents.to_le_bytes());
            if revents != 0 {
                ready += 1;
            }
        }
        if ready > 0 || no_wait {
            return ready;
        }
        if deadline.is_some_and(|d| process::ticks() >= d) {
            return 0;
        }
        if process::signal_pending() {
            return -EINTR;
        }
        sleep_until_something_happens(deadline);
    }
}

/// poll/select: sleep until anything is woken (data, a key, a closed pipe,
/// a packet...), a signal arrives, or the deadline passes. The caller then
/// checks its fds again.
fn sleep_until_something_happens(deadline: Option<u64>) {
    match deadline {
        Some(d) => process::block_on_until(process::POLL_KEY, d),
        None => process::block_on(process::POLL_KEY),
    }
}

/// membarrier(cmd, flags): make every thread see this thread's memory
/// writes (musl's dynamic linker uses it when loading libraries). With one
/// CPU every thread already does, so the commands just succeed.
fn sys_membarrier(cmd: u64) -> i64 {
    const QUERY: u64 = 0;
    const GLOBAL: u64 = 1;
    const PRIVATE_EXPEDITED: u64 = 8;
    const REGISTER_PRIVATE_EXPEDITED: u64 = 16;
    match cmd {
        QUERY => (GLOBAL | PRIVATE_EXPEDITED | REGISTER_PRIVATE_EXPEDITED) as i64,
        GLOBAL | PRIVATE_EXPEDITED | REGISTER_PRIVATE_EXPEDITED => 0,
        _ => -EINVAL,
    }
}

/// select(nfds, readfds, writefds, exceptfds, timeout) and pselect6 (the
/// same with a timespec, and a signal mask that isn't applied), on top of
/// poll: an fd set is a bit array of nfds bits.
fn sys_select(nfds: u64, rset: u64, wset: u64, eset: u64, timeout: u64, pselect: bool) -> i64 {
    const POLLPRI: i16 = 0x2;
    if nfds > vfs::MAX_FDS as u64 {
        return -EINVAL;
    }
    let bytes = nfds.div_ceil(64) * 8;
    let read_set = |ptr: u64| -> Result<alloc::vec::Vec<u8>, i64> {
        if ptr == 0 || bytes == 0 {
            return Ok(alloc::vec![0; bytes as usize]);
        }
        unsafe { user_slice(ptr, bytes) }.map(|b| b.to_vec()).ok_or(EFAULT)
    };
    let (r, w, e) = match (read_set(rset), read_set(wset), read_set(eset)) {
        (Ok(r), Ok(w), Ok(e)) => (r, w, e),
        _ => return -EFAULT,
    };
    let is_set = |set: &[u8], fd: usize| set[fd / 8] & (1 << (fd % 8)) != 0;
    // NULL: wait forever; a zero timeout: just check.
    let (deadline, no_wait) = if timeout == 0 {
        (None, false)
    } else {
        let Some(t) = (unsafe { user_slice(timeout, 16) }) else { return -EFAULT };
        let sec = u64::from_le_bytes(t[0..8].try_into().unwrap());
        let frac = u64::from_le_bytes(t[8..16].try_into().unwrap());
        let ns = sec.saturating_mul(1_000_000_000).saturating_add(if pselect { frac } else { frac.saturating_mul(1000) });
        (Some(process::ticks() + ns.div_ceil(process::TICK_NS)), ns == 0)
    };
    loop {
        crate::net::poll_if_due();
        let (mut ro, mut wo, mut eo) = (alloc::vec![0u8; bytes as usize], alloc::vec![0u8; bytes as usize], alloc::vec![0u8; bytes as usize]);
        let mut count = 0i64;
        for fd in 0..nfds as usize {
            let want = (if is_set(&r, fd) { vfs::POLLIN } else { 0 })
                | (if is_set(&w, fd) { vfs::POLLOUT } else { 0 })
                | (if is_set(&e, fd) { POLLPRI } else { 0 });
            if want == 0 {
                continue;
            }
            let got = vfs::poll_ready(fd as i32, want);
            if got & vfs::POLLNVAL != 0 {
                return -EBADF;
            }
            // Hangups and errors count as readable/writable, as on Linux.
            if is_set(&r, fd) && got & (vfs::POLLIN | vfs::POLLHUP | vfs::POLLERR) != 0 {
                ro[fd / 8] |= 1 << (fd % 8);
                count += 1;
            }
            if is_set(&w, fd) && got & (vfs::POLLOUT | vfs::POLLERR) != 0 {
                wo[fd / 8] |= 1 << (fd % 8);
                count += 1;
            }
            if is_set(&e, fd) && got & POLLPRI != 0 {
                eo[fd / 8] |= 1 << (fd % 8);
                count += 1;
            }
        }
        let timed_out = deadline.is_some_and(|d| process::ticks() >= d);
        if count > 0 || no_wait || timed_out {
            for (ptr, set) in [(rset, &ro), (wset, &wo), (eset, &eo)] {
                if ptr != 0 && bytes != 0 {
                    match unsafe { user_slice_mut(ptr, bytes) } {
                        Some(out) => out.copy_from_slice(set),
                        None => return -EFAULT,
                    }
                }
            }
            return count;
        }
        if process::signal_pending() {
            return -EINTR;
        }
        sleep_until_something_happens(deadline);
    }
}

/// getrlimit / prlimit64(pid, resource, new, old): report the limits we
/// have (open files, stack); everything else is unlimited. Setting is
/// accepted and ignored.
fn sys_prlimit(resource: u64, _new: u64, old: u64) -> i64 {
    const RLIMIT_STACK: u64 = 3;
    const RLIMIT_NOFILE: u64 = 7;
    if resource >= 16 {
        return -EINVAL;
    }
    if old != 0 {
        let limit = match resource {
            RLIMIT_STACK => crate::usermode::STACK_MAX,
            RLIMIT_NOFILE => vfs::MAX_FDS as u64,
            _ => u64::MAX, // RLIM_INFINITY
        };
        let Some(out) = (unsafe { user_slice_mut(old, 16) }) else { return -EFAULT };
        out[0..8].copy_from_slice(&limit.to_le_bytes());
        out[8..16].copy_from_slice(&limit.to_le_bytes());
    }
    0
}

/// sched_getaffinity(pid, size, mask): one CPU.
fn sys_sched_getaffinity(size: u64, mask: u64) -> i64 {
    if size < 8 {
        return -EINVAL;
    }
    let Some(out) = (unsafe { user_slice_mut(mask, 8) }) else { return -EFAULT };
    out.copy_from_slice(&1u64.to_le_bytes());
    8
}

/// reboot(magic1, magic2, cmd): restart or power off the machine.
fn sys_reboot(magic1: u64, magic2: u64, cmd: u64) -> i64 {
    const MAGIC1: u64 = 0xfee1dead;
    const MAGIC2: u64 = 672274793;
    const CMD_RESTART: u64 = 0x01234567;
    const CMD_POWER_OFF: u64 = 0x4321fedc;
    const CMD_HALT: u64 = 0xcdef0123;
    use x86_64::instructions::port::Port;

    if magic1 as u32 as u64 != MAGIC1 || magic2 as u32 as u64 != MAGIC2 {
        return -EINVAL;
    }
    // Don't lose data that's still in disk caches.
    crate::mounts::sync_all();
    match cmd as u32 as u64 {
        CMD_RESTART => unsafe {
            crate::println!("Rebooting...");
            Port::<u8>::new(0x64).write(0xFE); // keyboard controller reset
        },
        CMD_POWER_OFF => unsafe {
            crate::println!("Powering off...");
            Port::<u16>::new(0x604).write(0x2000); // QEMU ACPI power off
            Port::<u16>::new(0xB004).write(0x2000); // older QEMU / Bochs
        },
        CMD_HALT => crate::println!("System halted."),
        _ => return -EINVAL,
    }
    crate::hlt_loop();
}

/// uname(struct utsname *buf)
fn sys_uname(buf: u64) -> i64 {
    const FIELD: usize = 65; // Linux's __NEW_UTS_LEN + 1
    const FIELDS: [&str; 6] = [
        "PalladiumOS", // sysname
        "palladium",   // nodename
        crate::VERSION, // release
        "#1",          // version
        "x86_64",      // machine
        "(none)",      // domainname
    ];

    let Some(out) = (unsafe { user_slice_mut(buf, (FIELD * FIELDS.len()) as u64) }) else {
        return -EFAULT;
    };
    out.fill(0);
    for (i, s) in FIELDS.iter().enumerate() {
        let n = s.len().min(FIELD - 1);
        out[i * FIELD..i * FIELD + n].copy_from_slice(&s.as_bytes()[..n]);
    }
    0
}

/// Borrow a user buffer for reading (pub: used by process.rs and vfs.rs), after checking that every page in it
/// is mapped and user-accessible. Returns None for bad pointers.
pub unsafe fn user_slice(ptr: u64, len: u64) -> Option<&'static [u8]> {
    crate::usermode::populate_stack(ptr, len);
    if !crate::memory::is_user_range(ptr, len, false) {
        return None;
    }
    if len == 0 {
        return Some(&[]);
    }
    Some(unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) })
}

/// Same as `user_slice`, but the pages must also be writable.
pub unsafe fn user_slice_mut(ptr: u64, len: u64) -> Option<&'static mut [u8]> {
    crate::usermode::populate_stack(ptr, len);
    if !crate::memory::is_user_range(ptr, len, true) {
        return None;
    }
    if len == 0 {
        return Some(&mut []);
    }
    Some(unsafe { core::slice::from_raw_parts_mut(ptr as *mut u8, len as usize) })
}

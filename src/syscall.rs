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

// ---- Syscall numbers (Linux x86_64) ----
pub const SYS_READ: u64 = 0;
pub const SYS_WRITE: u64 = 1;
pub const SYS_OPEN: u64 = 2;
pub const SYS_CLOSE: u64 = 3;
pub const SYS_STAT: u64 = 4;
pub const SYS_FSTAT: u64 = 5;
pub const SYS_LSTAT: u64 = 6;
pub const SYS_LSEEK: u64 = 8;
pub const SYS_SCHED_YIELD: u64 = 24;
pub const SYS_GETPID: u64 = 39;
pub const SYS_FORK: u64 = 57;
pub const SYS_VFORK: u64 = 58;
pub const SYS_EXECVE: u64 = 59;
pub const SYS_EXIT: u64 = 60;
pub const SYS_WAIT4: u64 = 61;
pub const SYS_GETPPID: u64 = 110;
pub const SYS_UNAME: u64 = 63;
pub const SYS_GETCWD: u64 = 79;
pub const SYS_CHDIR: u64 = 80;
pub const SYS_MKDIR: u64 = 83;
pub const SYS_RMDIR: u64 = 84;
pub const SYS_UNLINK: u64 = 87;
pub const SYS_GETDENTS64: u64 = 217;
pub const SYS_EXIT_GROUP: u64 = 231;
pub const SYS_OPENAT: u64 = 257;

// ---- errno values (Linux) ----
pub const ENOENT: i64 = 2;
pub const E2BIG: i64 = 7;
pub const ENOEXEC: i64 = 8;
pub const EIO: i64 = 5;
pub const EBADF: i64 = 9;
pub const ECHILD: i64 = 10;
pub const EAGAIN: i64 = 11;
pub const ENOMEM: i64 = 12;
pub const EACCES: i64 = 13;
pub const EFAULT: i64 = 14;
pub const EBUSY: i64 = 16;
pub const EEXIST: i64 = 17;
pub const ENOTDIR: i64 = 20;
pub const EISDIR: i64 = 21;
pub const EINVAL: i64 = 22;
pub const EMFILE: i64 = 24;
pub const EFBIG: i64 = 27;
pub const ESPIPE: i64 = 29;
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

unsafe extern "C" {
    fn syscall_entry();
    fn syscall_return();
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
    let (a1, a2, a3) = (frame.rdi, frame.rsi, frame.rdx);

    let ret: i64 = match frame.rax {
        SYS_READ => match unsafe { user_slice_mut(a2, a3) } {
            Some(buf) => vfs::sys_read(a1, buf),
            None => -EFAULT,
        },
        SYS_WRITE => match unsafe { user_slice(a2, a3) } {
            Some(buf) => vfs::sys_write(a1, buf),
            None => -EFAULT,
        },
        SYS_OPEN => vfs::sys_open(a1, a2),
        SYS_OPENAT => vfs::sys_openat(a1, a2, a3),
        SYS_CLOSE => vfs::sys_close(a1),
        SYS_LSEEK => vfs::sys_lseek(a1, a2, a3),
        SYS_STAT | SYS_LSTAT => match unsafe { user_slice_mut(a2, vfs::STAT_SIZE) } {
            Some(buf) => vfs::sys_stat(a1, buf),
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
        SYS_MKDIR => vfs::sys_mkdir(a1),
        SYS_RMDIR => vfs::sys_remove(a1, true),
        SYS_UNLINK => vfs::sys_remove(a1, false),
        SYS_SCHED_YIELD => {
            process::yield_now();
            0
        }
        SYS_GETPID => process::current_pid() as i64,
        SYS_GETPPID => process::current_ppid() as i64,
        SYS_UNAME => sys_uname(a1),
        // vfork may be implemented as fork (POSIX allows it).
        SYS_FORK | SYS_VFORK => process::fork(frame),
        SYS_EXECVE => process::execve(frame, a1, a2, a3),
        SYS_WAIT4 => process::sys_wait4(a1, a2, a3),
        SYS_EXIT | SYS_EXIT_GROUP => process::exit_current(process::ExitReason::Exited(a1 as i32)),
        _ => -ENOSYS,
    };

    frame.rax = ret as u64;
}

/// uname(struct utsname *buf)
fn sys_uname(buf: u64) -> i64 {
    const FIELD: usize = 65; // Linux's __NEW_UTS_LEN + 1
    const FIELDS: [&str; 6] = [
        "PalladiumOS", // sysname
        "palladium",   // nodename
        "0.4.0",       // release
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
    if !crate::memory::is_user_range(ptr, len, true) {
        return None;
    }
    if len == 0 {
        return Some(&mut []);
    }
    Some(unsafe { core::slice::from_raw_parts_mut(ptr as *mut u8, len as usize) })
}

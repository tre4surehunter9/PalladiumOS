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
use crate::vfs;

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
pub const SYS_EXIT: u64 = 60;
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
pub const EIO: i64 = 5;
pub const EBADF: i64 = 9;
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

/// Registers saved by `syscall_entry`, lowest address first.
/// Must match the push order in the assembly below (pushed in reverse).
#[repr(C)]
pub struct SyscallFrame {
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rax: u64,
}

// The CPU has already pushed SS, RSP, RFLAGS, CS, RIP (40 bytes) onto the
// 16-byte-aligned RSP0 stack. We push 9 more registers (72 bytes), for 112 bytes
// in total, so RSP is still 16-byte aligned at the `call` as SysV requires.
// rbx, rbp, r12-r15 are callee-saved, so syscall_dispatch keeps them intact.
global_asm!(
    ".global syscall_entry",
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
    "mov rdi, rsp",          // arg0 = &mut SyscallFrame
    "call syscall_dispatch",
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
        SYS_SCHED_YIELD => 0,
        SYS_GETPID => 1, // only one process exists for now
        SYS_UNAME => sys_uname(a1),
        SYS_EXIT | SYS_EXIT_GROUP => crate::usermode::exit_current(a1 as i32),
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

/// Borrow a user buffer for reading, after checking that every page in it
/// is mapped and user-accessible. Returns None for bad pointers.
unsafe fn user_slice(ptr: u64, len: u64) -> Option<&'static [u8]> {
    if !crate::memory::is_user_range(ptr, len, false) {
        return None;
    }
    if len == 0 {
        return Some(&[]);
    }
    Some(unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) })
}

/// Same as `user_slice`, but the pages must also be writable.
unsafe fn user_slice_mut(ptr: u64, len: u64) -> Option<&'static mut [u8]> {
    if !crate::memory::is_user_range(ptr, len, true) {
        return None;
    }
    if len == 0 {
        return Some(&mut []);
    }
    Some(unsafe { core::slice::from_raw_parts_mut(ptr as *mut u8, len as usize) })
}

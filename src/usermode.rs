// Copyright (c) 2026 tre4surehunter9
//! Entering and leaving ring 3.
//!
//! `launch_program` saves the kernel's callee-saved registers, stack pointer
//! and RFLAGS into KERNEL_CTX (like setjmp), then `iretq`s into the program.
//! When the program calls exit (or crashes), `exit_current` restores that
//! context (like longjmp), so `launch_program` simply returns the exit status
//! and the shell keeps running.

use core::arch::global_asm;
use core::sync::atomic::{AtomicBool, Ordering};
use x86_64::VirtAddr;
use x86_64::structures::paging::Page;
use crate::memory::{self, MAPPER, FRAME_ALLOCATOR};

const USER_CODE_ADDR: u64 = 0x_6666_6666_0000;
const USER_STACK_ADDR: u64 = 0x_5555_5555_0000;

/// Kernel state saved on entry to ring 3. Field offsets are used by the asm.
#[repr(C)]
struct KernelContext {
    rbx: u64,    // 0
    rbp: u64,    // 8
    r12: u64,    // 16
    r13: u64,    // 24
    r14: u64,    // 32
    r15: u64,    // 40
    rflags: u64, // 48
    rsp: u64,    // 56
}

static mut KERNEL_CTX: KernelContext = KernelContext {
    rbx: 0, rbp: 0, r12: 0, r13: 0, r14: 0, r15: 0, rflags: 0, rsp: 0,
};

/// True while a user program is running (only one at a time for now).
static IN_USERMODE: AtomicBool = AtomicBool::new(false);

global_asm!(
    // u64 usermode_enter(ctx: *mut KernelContext, rip, rsp, cs, ss)
    //   rdi = ctx, rsi = user rip, rdx = user rsp, rcx = cs, r8 = ss
    // "Returns" only when usermode_return is called, with rax = exit status.
    ".global usermode_enter",
    "usermode_enter:",
    "mov [rdi + 0], rbx",
    "mov [rdi + 8], rbp",
    "mov [rdi + 16], r12",
    "mov [rdi + 24], r13",
    "mov [rdi + 32], r14",
    "mov [rdi + 40], r15",
    "pushfq",
    "pop rax",
    "mov [rdi + 48], rax",
    "mov [rdi + 56], rsp",   // points at our return address
    // iretq frame: SS, RSP, RFLAGS, CS, RIP
    "push r8",
    "push rdx",
    "mov eax, 0x202",        // RFLAGS: IF set (+ reserved bit 1)
    "push rax",
    "push rcx",
    "push rsi",
    // Don't leak kernel values to the program.
    "xor eax, eax",
    "xor ebx, ebx",
    "xor ecx, ecx",
    "xor edx, edx",
    "xor esi, esi",
    "xor edi, edi",
    "xor ebp, ebp",
    "xor r8d, r8d",
    "xor r9d, r9d",
    "xor r10d, r10d",
    "xor r11d, r11d",
    "xor r12d, r12d",
    "xor r13d, r13d",
    "xor r14d, r14d",
    "xor r15d, r15d",
    "iretq",

    // ! usermode_return(ctx: *const KernelContext, status: u64)
    ".global usermode_return",
    "usermode_return:",
    "mov rbx, [rdi + 0]",
    "mov rbp, [rdi + 8]",
    "mov r12, [rdi + 16]",
    "mov r13, [rdi + 24]",
    "mov r14, [rdi + 32]",
    "mov r15, [rdi + 40]",
    "mov rsp, [rdi + 56]",
    "push qword ptr [rdi + 48]",
    "popfq",                 // restores IF, which the int 0x80 gate cleared
    "mov rax, rsi",
    "ret",                   // back into launch_program
);

unsafe extern "C" {
    fn usermode_enter(ctx: *mut KernelContext, rip: u64, rsp: u64, cs: u64, ss: u64) -> u64;
    fn usermode_return(ctx: *const KernelContext, status: u64) -> !;
}

/// Load `bytes` as a flat binary at USER_CODE_ADDR, run it in ring 3 and
/// return its exit status once it exits.
pub fn launch_program(bytes: &[u8]) -> i32 {
    if bytes.len() > 4096 {
        crate::println!("run: program too large (max 4096 bytes for now)");
        return -1;
    }
    if IN_USERMODE.swap(true, Ordering::SeqCst) {
        crate::println!("run: a program is already running");
        return -1;
    }

    let user_page = Page::containing_address(VirtAddr::new(USER_CODE_ADDR));
    let user_stack_page = Page::containing_address(VirtAddr::new(USER_STACK_ADDR));
    {
        let mut mapper_guard = MAPPER.lock();
        let mut alloc_guard = FRAME_ALLOCATOR.lock();
        let mapper = mapper_guard.as_mut().expect("MAPPER not initialized");
        let frame_allocator = alloc_guard.as_mut().expect("FRAME_ALLOCATOR not initialized");

        memory::map_user_page(mapper, frame_allocator, user_page);
        memory::map_user_page(mapper, frame_allocator, user_stack_page);
    } // locks dropped here: they must not be held while in ring 3

    // Pages may hold data from a previous run, so clear them first.
    unsafe {
        core::ptr::write_bytes(user_page.start_address().as_mut_ptr::<u8>(), 0, 4096);
        core::ptr::write_bytes(user_stack_page.start_address().as_mut_ptr::<u8>(), 0, 4096);
    }
    memory::load_payload_bytes(user_page.start_address(), bytes);

    let (code_sel, data_sel) = crate::gdt::user_selectors();
    // RPL 3 = "requested privilege level 3", the low 2 bits of the selector
    let cs = (code_sel.0 as u64) | 3;
    let ss = (data_sel.0 as u64) | 3;
    let rip = user_page.start_address().as_u64();
    let rsp = (user_stack_page.start_address() + 4096u64).as_u64();

    let status = unsafe { usermode_enter(&raw mut KERNEL_CTX, rip, rsp, cs, ss) };

    IN_USERMODE.store(false, Ordering::SeqCst);
    status as i32
}

/// Terminate the running user program and resume the kernel inside
/// `launch_program`. Called by the exit syscalls and by fault handlers.
pub fn exit_current(status: i32) -> ! {
    if !IN_USERMODE.load(Ordering::SeqCst) {
        panic!("exit_current called with no user program running");
    }
    unsafe { usermode_return(&raw const KERNEL_CTX, status as u32 as u64) }
}

/// True if an interrupt/exception came from ring 3 (low 2 bits of CS).
pub fn from_user(code_segment: u64) -> bool {
    code_segment & 3 == 3
}

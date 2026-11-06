// Copyright (c) 2026 tre4surehunter9
//! Entering and leaving ring 3.
//!
//! `run` saves the kernel's callee-saved registers, stack pointer
//! and RFLAGS into KERNEL_CTX (like setjmp), then `iretq`s into the program.
//! When the program calls exit (or crashes), `exit_current` restores that
//! context (like longjmp), so `run` simply returns the exit status
//! and the shell keeps running.

use core::arch::global_asm;
use core::sync::atomic::{AtomicBool, Ordering};
use alloc::vec::Vec;
use spin::Mutex;
use x86_64::VirtAddr;
use x86_64::structures::paging::{
    FrameAllocator, FrameDeallocator, Mapper, OffsetPageTable, Page, PageTableFlags, Size4KiB, Translate,
};
use crate::elf;
use crate::memory::{self, BootInfoFrameAllocator, MAPPER, FRAME_ALLOCATOR};

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
    "ret",                   // back into run()
);

unsafe extern "C" {
    fn usermode_enter(ctx: *mut KernelContext, rip: u64, rsp: u64, cs: u64, ss: u64) -> u64;
    fn usermode_return(ctx: *const KernelContext, status: u64) -> !;
}

/// Where flat (non-ELF) binaries are loaded and entered.
const FLAT_BASE: u64 = 0x_6666_6666_0000;
const MAX_FLAT_SIZE: usize = 64 * 1024;

/// The user stack: STACK_PAGES pages ending at STACK_TOP. The page below is
/// left unmapped as a guard, so overflowing the stack is a segfault.
const STACK_TOP: u64 = 0x_7fff_ffff_0000;
const STACK_PAGES: u64 = 16;

const MAX_ARGS: usize = 64;
const MAX_ARG_BYTES: usize = 4096;

/// Pages mapped for the current program, so they can be unmapped on exit.
static USER_PAGES: Mutex<Vec<(Page, PageTableFlags)>> = Mutex::new(Vec::new());

/// Load and run a program (ELF or flat binary) in ring 3 with the given
/// argv, and return its exit status once it exits.
pub fn run(bytes: &[u8], argv: &[&str]) -> Result<i32, &'static str> {
    if IN_USERMODE.swap(true, Ordering::SeqCst) {
        return Err("a program is already running");
    }

    let loaded = {
        let mut mapper_guard = MAPPER.lock();
        let mut alloc_guard = FRAME_ALLOCATOR.lock();
        let mapper = mapper_guard.as_mut().expect("MAPPER not initialized");
        let frames = alloc_guard.as_mut().expect("FRAME_ALLOCATOR not initialized");

        let result = load(mapper, frames, bytes, argv);
        if result.is_err() {
            unmap_all(mapper, frames);
        }
        result
    }; // locks dropped here: they must not be held while in ring 3

    let (entry, stack_pointer) = match loaded {
        Ok(v) => v,
        Err(e) => {
            IN_USERMODE.store(false, Ordering::SeqCst);
            return Err(e);
        }
    };

    let (code_sel, data_sel) = crate::gdt::user_selectors();
    // RPL 3 = "requested privilege level 3", the low 2 bits of the selector
    let cs = (code_sel.0 as u64) | 3;
    let ss = (data_sel.0 as u64) | 3;

    // Fresh fds (0/1/2 = console) and a copy of the shell's working directory.
    let cwd = crate::filesystem::CWD.lock().clone();
    crate::vfs::start_process(&cwd);

    let status = unsafe { usermode_enter(&raw mut KERNEL_CTX, entry, stack_pointer, cs, ss) };

    crate::vfs::end_process();

    {
        let mut mapper_guard = MAPPER.lock();
        let mut alloc_guard = FRAME_ALLOCATOR.lock();
        unmap_all(mapper_guard.as_mut().unwrap(), alloc_guard.as_mut().unwrap());
    }
    IN_USERMODE.store(false, Ordering::SeqCst);
    Ok(status as i32)
}

/// Run a program with argv = ["program"]. Prints load errors and returns -1.
pub fn launch_program(bytes: &[u8]) -> i32 {
    match run(bytes, &["program"]) {
        Ok(status) => status,
        Err(e) => {
            crate::println!("run: {}", e);
            -1
        }
    }
}

/// Map the program and its stack. Returns (entry point, initial stack pointer).
fn load(
    mapper: &mut OffsetPageTable<'static>,
    frames: &mut BootInfoFrameAllocator,
    bytes: &[u8],
    argv: &[&str],
) -> Result<(u64, u64), &'static str> {
    use PageTableFlags as F;
    let base = F::PRESENT | F::USER_ACCESSIBLE;

    let entry = if elf::is_elf(bytes) {
        let image = elf::parse(bytes)?;
        for seg in &image.segments {
            let flags = if seg.flags & elf::PF_W != 0 { base | F::WRITABLE } else { base };
            map_range(mapper, frames, seg.vaddr, seg.memsz, flags)?;
            copy_to_user(mapper, seg.vaddr, seg.data);
        }
        image.entry
    } else {
        if bytes.is_empty() || bytes.len() > MAX_FLAT_SIZE {
            return Err("flat binary is empty or larger than 64 KiB");
        }
        map_range(mapper, frames, FLAT_BASE, bytes.len() as u64, base | F::WRITABLE)?;
        copy_to_user(mapper, FLAT_BASE, bytes);
        FLAT_BASE
    };

    let stack_bottom = STACK_TOP - STACK_PAGES * 4096;
    map_range(mapper, frames, stack_bottom, STACK_PAGES * 4096, base | F::WRITABLE)?;
    let stack_pointer = build_initial_stack(mapper, argv)?;

    Ok((entry, stack_pointer))
}

/// Lay out the initial stack the way the System V x86_64 ABI (and libc's
/// _start) expects, from the stack pointer upwards:
///   argc, argv[0..argc], NULL, envp NULL, auxv AT_NULL (0, 0)
/// followed by the argument strings themselves near the top of the stack.
fn build_initial_stack(mapper: &OffsetPageTable<'static>, argv: &[&str]) -> Result<u64, &'static str> {
    let string_bytes: usize = argv.iter().map(|a| a.len() + 1).sum();
    if argv.len() > MAX_ARGS || string_bytes > MAX_ARG_BYTES {
        return Err("argument list too long");
    }

    // Strings, NUL-terminated, packed at the top of the stack.
    let strings_start = STACK_TOP - string_bytes as u64;
    let mut pointers = Vec::with_capacity(argv.len());
    let mut addr = strings_start;
    for arg in argv {
        pointers.push(addr);
        copy_to_user(mapper, addr, arg.as_bytes());
        copy_to_user(mapper, addr + arg.len() as u64, &[0]);
        addr += arg.len() as u64 + 1;
    }

    let mut words: Vec<u64> = Vec::with_capacity(argv.len() + 5);
    words.push(argv.len() as u64); // argc
    words.extend_from_slice(&pointers); // argv
    words.push(0); // argv terminator
    words.push(0); // envp terminator (no environment yet)
    words.push(0); // auxv: AT_NULL
    words.push(0);

    // rsp must be 16-byte aligned at the entry point, pointing at argc.
    let sp = (strings_start - (words.len() * 8) as u64) & !0xf;
    let mut bytes = Vec::with_capacity(words.len() * 8);
    for w in &words {
        bytes.extend_from_slice(&w.to_le_bytes());
    }
    copy_to_user(mapper, sp, &bytes);
    Ok(sp)
}

/// Map every page covering [start, start + len) with `flags`. A page that
/// this program already mapped (two segments sharing a page) gets the union
/// of both flag sets; a page mapped by anything else is refused.
fn map_range(
    mapper: &mut OffsetPageTable<'static>,
    frames: &mut BootInfoFrameAllocator,
    start: u64,
    len: u64,
    flags: PageTableFlags,
) -> Result<(), &'static str> {
    let end = start.checked_add(len).ok_or("address overflow")?;
    if end > memory::USER_SPACE_END {
        return Err("address outside user space");
    }
    let first = Page::<Size4KiB>::containing_address(VirtAddr::new(start));
    let last = Page::<Size4KiB>::containing_address(VirtAddr::new(end - 1));
    let mut pages = USER_PAGES.lock();

    for page in Page::range_inclusive(first, last) {
        if let Some(entry) = pages.iter_mut().find(|(p, _)| *p == page) {
            entry.1 |= flags;
            unsafe {
                mapper.update_flags(page, entry.1).map_err(|_| "update_flags failed")?.flush();
            }
            continue;
        }
        if mapper.translate_page(page).is_ok() {
            return Err("program overlaps memory that is already in use");
        }
        let frame = frames.allocate_frame().ok_or("out of memory")?;
        memory::zero_frame(frame);
        unsafe {
            mapper
                .map_to(page, frame, flags, frames)
                .map_err(|_| "map_to failed")?
                .flush();
        }
        pages.push((page, flags));
    }
    Ok(())
}

/// Copy bytes into already-mapped user memory. Writes go through the
/// physical memory map, so read-only user pages can be filled too.
fn copy_to_user(mapper: &OffsetPageTable<'static>, addr: u64, data: &[u8]) {
    let mut done = 0usize;
    while done < data.len() {
        let va = addr + done as u64;
        let in_page = (4096 - (va & 0xfff)) as usize;
        let n = in_page.min(data.len() - done);
        let phys = mapper.translate_addr(VirtAddr::new(va)).expect("copy_to_user: page not mapped");
        unsafe {
            core::ptr::copy_nonoverlapping(data[done..].as_ptr(), memory::phys_ptr(phys), n);
        }
        done += n;
    }
}

/// Unmap all pages of the current program and give their frames back.
fn unmap_all(mapper: &mut OffsetPageTable<'static>, frames: &mut BootInfoFrameAllocator) {
    for (page, _) in USER_PAGES.lock().drain(..) {
        if let Ok((frame, flush)) = mapper.unmap(page) {
            flush.flush();
            unsafe { frames.deallocate_frame(frame) };
        }
    }
}

/// Terminate the running user program and resume the kernel inside
/// `run`. Called by the exit syscalls and by fault handlers.
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

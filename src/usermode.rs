// Copyright (c) 2026 tre4surehunter9
//! Program loading: builds a new address space containing a program
//! (an ELF executable or a flat binary) and its initial stack.
//!
//! Running programs is the scheduler's job (src/process.rs); `run` and
//! `launch_program` here are the kernel shell's entry points into it.

use alloc::vec::Vec;
use x86_64::VirtAddr;
use x86_64::structures::paging::{
    mapper::TranslateResult, FrameAllocator, Mapper, OffsetPageTable, Page, PageTableFlags, PhysFrame,
    Size4KiB, Translate,
};
use crate::elf;
use crate::memory::{self, BootInfoFrameAllocator, FRAME_ALLOCATOR};

/// Where flat (non-ELF) binaries are loaded and entered.
const FLAT_BASE: u64 = 0x_6666_6666_0000;
const MAX_FLAT_SIZE: usize = 64 * 1024;

/// The user stack ends at STACK_TOP and may grow down to STACK_MAX bytes.
/// Only STACK_PAGES are mapped up front; the rest is mapped on demand when
/// the program first touches it (see handle_stack_fault). Below the limit
/// nothing is ever mapped, so overflowing the stack is a segfault.
pub const STACK_TOP: u64 = 0x_7fff_ffff_0000;
pub const STACK_MAX: u64 = 1024 * 1024;
const STACK_PAGES: u64 = 16;

pub const MAX_ARGS: usize = 64;
pub const MAX_ARG_BYTES: usize = 16 * 1024;

pub const ERR_TOO_MANY_ARGS: &str = "argument list too long";
pub const ERR_NO_MEMORY: &str = "out of memory";
/// The program's interpreter (dynamic linker) doesn't exist.
pub const ERR_NO_INTERP: &str = "program interpreter not found";

/// A loaded program, ready to be entered.
pub struct Image {
    pub p4: PhysFrame,
    pub entry: u64,
    pub stack_pointer: u64,
}

/// Who a new program runs as (for the aux vector: AT_UID, AT_SECURE...).
#[derive(Clone, Copy)]
pub struct Ids {
    pub uid: u32,
    pub euid: u32,
    pub gid: u32,
    pub egid: u32,
}

/// What the aux vector tells the program about itself.
struct AuxInfo {
    entry: u64,
    phdr: u64,
    phnum: u64,
    phent: u64,
    /// Where the interpreter was loaded (AT_BASE; 0: none).
    base: u64,
    ids: Ids,
}

/// Build a new address space holding the program and its stack.
pub fn build_image(bytes: &[u8], argv: &[&str], envp: &[&str], ids: Ids) -> Result<Image, &'static str> {
    // A dynamically linked program: read its interpreter first (not while
    // holding the frame allocator).
    let interp = match elf::is_elf(bytes).then(|| elf::parse(bytes)).transpose()?.and_then(|e| e.interp) {
        Some(path) => Some(crate::filesystem::read_bytes(&crate::vfs::absolute(&path)).map_err(|_| ERR_NO_INTERP)?),
        None => None,
    };
    let mut alloc_guard = FRAME_ALLOCATOR.lock();
    let frames = alloc_guard.as_mut().expect("FRAME_ALLOCATOR not initialized");
    let p4 = memory::new_address_space(frames).ok_or(ERR_NO_MEMORY)?;
    let mut mapper = memory::mapper_for(p4);

    match load(&mut mapper, frames, bytes, interp.as_deref(), argv, envp, ids) {
        Ok((entry, stack_pointer)) => Ok(Image { p4, entry, stack_pointer }),
        Err(e) => {
            memory::free_address_space(p4, frames);
            Err(e)
        }
    }
}

/// Run a program as a child of the kernel and wait for it (and anything it
/// started) to finish. Returns its exit status, or 128 + signal if it was
/// killed (e.g. 139 for a segfault), like a shell's `$?`.
pub fn run(bytes: &[u8], argv: &[&str]) -> Result<i32, &'static str> {
    crate::process::run_program(bytes, argv)
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

/// True if an interrupt/exception came from ring 3 (low 2 bits of CS).
pub fn from_user(code_segment: u64) -> bool {
    code_segment & 3 == 3
}

/// Map the program and its stack. Returns (entry point, initial stack pointer).
fn load(
    mapper: &mut OffsetPageTable<'static>,
    frames: &mut BootInfoFrameAllocator,
    bytes: &[u8],
    interp: Option<&[u8]>,
    argv: &[&str],
    envp: &[&str],
    ids: Ids,
) -> Result<(u64, u64), &'static str> {
    use PageTableFlags as F;
    let base = F::PRESENT | F::USER_ACCESSIBLE;
    let mut aux = AuxInfo { entry: 0, phdr: 0, phnum: 0, phent: 0, base: 0, ids };
    let mut map_elf = |mapper: &mut OffsetPageTable<'static>, image: &elf::Elf| -> Result<(), &'static str> {
        for seg in &image.segments {
            let flags = if seg.flags & elf::PF_W != 0 { base | F::WRITABLE } else { base };
            map_range(mapper, frames, seg.vaddr, seg.memsz, flags)?;
            copy_to_user(mapper, seg.vaddr, seg.data);
        }
        Ok(())
    };

    let entry = if elf::is_elf(bytes) {
        let image = elf::parse(bytes)?;
        map_elf(mapper, &image)?;
        aux = AuxInfo { entry: image.entry, phdr: image.phdr, phnum: image.phnum, phent: image.phent, base: 0, ids };
        match interp {
            // Dynamically linked: the dynamic linker starts first, finds the
            // program through the aux vector, loads its libraries, and
            // jumps to its entry point.
            Some(ld) => {
                let ld = elf::parse_interp(ld)?;
                map_elf(mapper, &ld)?;
                aux.base = elf::INTERP_BASE;
                ld.entry
            }
            None => image.entry,
        }
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
    let stack_pointer = build_initial_stack(mapper, argv, envp, &aux)?;

    Ok((entry, stack_pointer))
}

/// Lay out the initial stack the way the System V x86_64 ABI (and libc's
/// _start) expects, from the stack pointer upwards:
///   argc, argv[0..argc], NULL, envp[..], NULL, auxv pairs, AT_NULL (0, 0)
/// followed by 16 random bytes (AT_RANDOM) and the strings near the top.
fn build_initial_stack(
    mapper: &OffsetPageTable<'static>,
    argv: &[&str],
    envp: &[&str],
    aux: &AuxInfo,
) -> Result<u64, &'static str> {
    let string_bytes: usize = argv.iter().chain(envp).map(|a| a.len() + 1).sum();
    if argv.len() > MAX_ARGS || envp.len() > MAX_ARGS || string_bytes > MAX_ARG_BYTES {
        return Err(ERR_TOO_MANY_ARGS);
    }

    // Strings, NUL-terminated, packed at the top of the stack.
    let strings_start = STACK_TOP - string_bytes as u64;
    let mut addr = strings_start;
    let mut place = |s: &str| {
        let at = addr;
        copy_to_user(mapper, at, s.as_bytes());
        copy_to_user(mapper, at + s.len() as u64, &[0]);
        addr += s.len() as u64 + 1;
        at
    };
    let argv_ptrs: Vec<u64> = argv.iter().map(|s| place(s)).collect();
    let envp_ptrs: Vec<u64> = envp.iter().map(|s| place(s)).collect();
    // 16 random bytes, below the strings (8-byte aligned).
    let random_at = (strings_start - 16) & !7;
    let mut random = [0u8; 16];
    crate::random::fill(&mut random);
    copy_to_user(mapper, random_at, &random);

    let mut words: Vec<u64> = Vec::with_capacity(argv.len() + envp.len() + 5);
    words.push(argv.len() as u64); // argc
    words.extend_from_slice(&argv_ptrs);
    words.push(0); // argv terminator
    words.extend_from_slice(&envp_ptrs);
    words.push(0); // envp terminator
    // The aux vector (as Linux gives it): what libc's startup code needs.
    let ids = aux.ids;
    let secure = (ids.uid != ids.euid || ids.gid != ids.egid) as u64;
    let auxv: [(u64, u64); 15] = [
        (3, aux.phdr),       // AT_PHDR
        (4, aux.phent),      // AT_PHENT
        (5, aux.phnum),      // AT_PHNUM
        (6, 4096),           // AT_PAGESZ
        (7, aux.base),       // AT_BASE: where the dynamic linker is (0: none)
        (8, 0),              // AT_FLAGS
        (9, aux.entry),      // AT_ENTRY
        (11, ids.uid as u64), // AT_UID
        (12, ids.euid as u64), // AT_EUID
        (13, ids.gid as u64), // AT_GID
        (14, ids.egid as u64), // AT_EGID
        (16, 0),             // AT_HWCAP
        (17, 100),           // AT_CLKTCK
        (23, secure),        // AT_SECURE: setuid/setgid program
        (25, random_at),     // AT_RANDOM
    ];
    for (k, v) in auxv {
        if k == 3 && v == 0 {
            continue; // program headers not in memory (flat binaries)
        }
        words.push(k);
        words.push(v);
    }
    words.push(0); // AT_NULL
    words.push(0);

    // rsp must be 16-byte aligned at the entry point, pointing at argc.
    let sp = (random_at - (words.len() * 8) as u64) & !0xf;
    let mut bytes = Vec::with_capacity(words.len() * 8);
    for w in &words {
        bytes.extend_from_slice(&w.to_le_bytes());
    }
    copy_to_user(mapper, sp, &bytes);
    Ok(sp)
}

/// Map every page covering [start, start + len) with `flags`. A page this
/// program already mapped (two segments sharing a page) gets the union of
/// both flag sets. Memory in the kernel's page-table slots is refused.
fn map_range(
    mapper: &mut OffsetPageTable<'static>,
    frames: &mut BootInfoFrameAllocator,
    start: u64,
    len: u64,
    flags: PageTableFlags,
) -> Result<(), &'static str> {
    let end = start.checked_add(len).ok_or("address overflow")?;
    if len == 0 || end > memory::USER_SPACE_END {
        return Err("address outside user space");
    }
    let first = Page::<Size4KiB>::containing_address(VirtAddr::new(start));
    let last = Page::<Size4KiB>::containing_address(VirtAddr::new(end - 1));

    for page in Page::range_inclusive(first, last) {
        let addr = page.start_address();
        if !memory::is_user_slot(memory::p4_index(addr.as_u64())) {
            return Err("program overlaps memory that is already in use");
        }
        if let TranslateResult::Mapped { flags: existing, .. } = mapper.translate(addr) {
            unsafe {
                mapper
                    .update_flags(page, existing | flags)
                    .map_err(|_| "update_flags failed")?
                    .ignore(); // not the active address space
            }
            continue;
        }
        let frame = frames.allocate_frame().ok_or(ERR_NO_MEMORY)?;
        memory::zero_frame(frame);
        unsafe {
            match mapper.map_to(page, frame, flags, frames) {
                Ok(flush) => flush.ignore(),
                Err(_) => {
                    x86_64::structures::paging::FrameDeallocator::deallocate_frame(frames, frame);
                    return Err(ERR_NO_MEMORY);
                }
            }
        }
    }
    Ok(())
}

/// Copy bytes into mapped user memory of `mapper`'s address space. Writes go
/// through the physical memory map, so read-only pages can be filled too.
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

// ---- growing the stack on demand ----

fn in_stack_window(addr: u64) -> bool {
    (STACK_TOP - STACK_MAX..STACK_TOP).contains(&addr)
}

/// Map a zeroed page at `addr` in the current address space if it's in
/// the stack window and not mapped yet. Returns false if it's not a stack
/// address (or memory ran out).
fn map_stack_page(addr: u64) -> bool {
    use x86_64::registers::control::Cr3;
    if !in_stack_window(addr) {
        return false;
    }
    let (p4, _) = Cr3::read();
    let mut mapper = memory::mapper_for(p4);
    let page = Page::<Size4KiB>::containing_address(VirtAddr::new(addr));
    if mapper.translate_page(page).is_ok() {
        return true;
    }
    let mut alloc_guard = FRAME_ALLOCATOR.lock();
    let frames = alloc_guard.as_mut().unwrap();
    let Some(frame) = frames.allocate_frame() else { return false };
    memory::zero_frame(frame);
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE;
    match unsafe { mapper.map_to(page, frame, flags, frames) } {
        Ok(flush) => {
            flush.flush();
            true
        }
        Err(_) => {
            unsafe { x86_64::structures::paging::FrameDeallocator::deallocate_frame(frames, frame) };
            false
        }
    }
}

/// Called by the page fault handler for a fault in ring 3 on a page that
/// isn't present. Returns true if it was the stack growing (the program
/// then simply continues).
pub fn handle_stack_fault(addr: u64) -> bool {
    map_stack_page(addr)
}

/// Before a syscall uses user memory: map any untouched stack pages in
/// [ptr, ptr + len), so a buffer on a not-yet-used part of the stack works.
pub fn populate_stack(ptr: u64, len: u64) {
    let Some(end) = ptr.checked_add(len) else { return };
    if len == 0 || end <= STACK_TOP - STACK_MAX || ptr >= STACK_TOP {
        return;
    }
    let mut page = ptr.max(STACK_TOP - STACK_MAX) & !0xfff;
    while page < end.min(STACK_TOP) {
        map_stack_page(page);
        page += 4096;
    }
}

// Copyright (c) 2026 tre4surehunter9
//! mmap, munmap, mprotect: memory for programs beyond the brk heap.
//!
//! musl's malloc gets most of its memory this way. Mappings are private:
//! anonymous ones are zero-filled, file ones are a copy of the file's
//! bytes (changes aren't written back, so shared writable file mappings
//! are refused). Anonymous memory gets its pages when first touched (the
//! page fault handler calls fault_in), so reserving gigabytes of address
//! space costs nothing until it's used, as on Linux (V8, Go and others
//! reserve far more than they use). File mappings are read in right away,
//! a page at a time.
//!
//! Without MAP_FIXED, addresses are picked from [MMAP_BASE, MMAP_END),
//! between the brk heap and the stack.

use alloc::vec::Vec;
use x86_64::structures::paging::{FrameAllocator, FrameDeallocator, Mapper, Page, PageTableFlags as F, Size4KiB, Translate};
use x86_64::VirtAddr;

use crate::memory::{self, FRAME_ALLOCATOR};
use crate::process;
use crate::syscall::{EEXIST, EINVAL, ENODEV, ENOMEM};

const PROT_READ: u64 = 1;
const PROT_WRITE: u64 = 2;
const PROT_EXEC: u64 = 4;
const MAP_SHARED: u64 = 0x01;
const MAP_PRIVATE: u64 = 0x02;
const MAP_FIXED: u64 = 0x10;
const MAP_ANONYMOUS: u64 = 0x20;
const MAP_FIXED_NOREPLACE: u64 = 0x10_0000;

pub const MMAP_BASE: u64 = 0x2000_0000_0000;
const MMAP_END: u64 = 0x7000_0000_0000;
/// The largest single mapping (address space; memory comes when used).
const MAX_MAP: u64 = 1 << 40;

fn page_up(x: u64) -> u64 {
    (x + 4095) & !4095
}

fn flags_for(prot: u64) -> Option<F> {
    if prot & (PROT_READ | PROT_WRITE | PROT_EXEC) == 0 {
        return None; // PROT_NONE: no access
    }
    let mut f = F::PRESENT | F::USER_ACCESSIBLE;
    if prot & PROT_WRITE != 0 {
        f |= F::WRITABLE;
    }
    Some(f)
}

/// May [start, end) hold user mappings at all?
fn in_user_space(start: u64, end: u64) -> bool {
    start >= 0x1000
        && start < end
        && end <= memory::USER_SPACE_END
        && (start..end).step_by(1 << 39).chain(core::iter::once(end - 1)).all(|a| memory::is_user_slot(memory::p4_index(a)))
}

fn page(addr: u64) -> Page<Size4KiB> {
    Page::containing_address(VirtAddr::new(addr))
}

type Maps = Vec<(u64, u64, u64)>;

/// Is any page in [start, end) mapped, or reserved by an earlier mmap?
fn in_use(maps: &[(u64, u64, u64)], start: u64, end: u64) -> bool {
    if maps.iter().any(|&(s, e, _)| s < end && start < e) {
        return true;
    }
    let mut any = false;
    memory::for_each_mapped(process::current_p4(), start, end, |_| any = true);
    any
}

/// The lowest free range of `len` bytes in the mmap area (or at `hint`).
fn find_free(maps: &[(u64, u64, u64)], hint: u64, len: u64) -> Option<u64> {
    if hint != 0 && hint & 0xfff == 0 && in_user_space(hint, hint + len) && !in_use(maps, hint, hint + len) {
        return Some(hint);
    }
    let mut cur = MMAP_BASE;
    for &(s, e, _) in maps {
        if e <= cur {
            continue;
        }
        if s >= cur + len {
            break;
        }
        cur = page_up(e);
    }
    (cur + len <= MMAP_END && in_user_space(cur, cur + len)).then_some(cur)
}

/// Unmap [start, end) and free its memory. Pages that aren't mapped are fine.
fn unmap_pages(start: u64, end: u64) {
    let p4 = process::current_p4();
    let mut pages = Vec::new();
    memory::for_each_mapped(p4, start, end, |a| pages.push(a));
    let mut mapper = memory::mapper_for(p4);
    let mut alloc = FRAME_ALLOCATOR.lock();
    let frames = alloc.as_mut().unwrap();
    for a in pages {
        if let Ok((frame, flush)) = mapper.unmap(page(a)) {
            flush.flush();
            frames.release(frame); // freed when no fork shares it any more
        }
    }
}

/// Map a zeroed page at `addr` holding `data` (at most a page).
fn map_page(addr: u64, flags: F, data: &[u8]) -> Result<(), ()> {
    let mut mapper = memory::mapper_for(process::current_p4());
    let mut alloc = FRAME_ALLOCATOR.lock();
    let frames = alloc.as_mut().unwrap();
    let frame = frames.allocate_frame().ok_or(())?;
    memory::zero_frame(frame);
    if !data.is_empty() {
        let dst = memory::phys_ptr(frame.start_address());
        unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), dst, data.len()) };
    }
    match unsafe { mapper.map_to(page(addr), frame, flags, frames) } {
        Ok(flush) => {
            flush.flush();
            Ok(())
        }
        Err(_) => {
            unsafe { frames.deallocate_frame(frame) };
            Err(())
        }
    }
}

/// Remove [start, end) from the list of mappings (splitting as needed).
fn forget(maps: &mut Maps, start: u64, end: u64) {
    let mut out = Vec::with_capacity(maps.len() + 1);
    for &(s, e, p) in maps.iter() {
        if e <= start || s >= end {
            out.push((s, e, p));
            continue;
        }
        if s < start {
            out.push((s, start, p));
        }
        if e > end {
            out.push((end, e, p));
        }
    }
    *maps = out;
}

fn remember(maps: &mut Maps, start: u64, end: u64, prot: u64) {
    let i = maps.partition_point(|&(s, _, _)| s < start);
    maps.insert(i, (start, end, prot));
}

/// The page at `addr` was touched for the first time: if it's mmap'd
/// memory the program may use, give it a zeroed page. False if it isn't
/// (a real segmentation fault).
pub fn fault_in(addr: u64) -> bool {
    let addr = addr & !0xfff;
    let Some(prot) = process::try_with_current_mm(|mm| mm.maps.iter().find(|&&(s, e, _)| s <= addr && addr < e).map(|m| m.2)) else {
        return false;
    };
    match prot.and_then(flags_for) {
        Some(f) => {
            let mapper = memory::mapper_for(process::current_p4());
            mapper.translate_addr(VirtAddr::new(addr)).is_some() || map_page(addr, f, &[]).is_ok()
        }
        None => false,
    }
}

/// mmap(addr, length, prot, flags, fd, offset)
pub fn sys_mmap(addr: u64, len: u64, prot: u64, flags: u64, fd: u64, offset: u64) -> i64 {
    let fixed = flags & (MAP_FIXED | MAP_FIXED_NOREPLACE) != 0;
    let anon = flags & MAP_ANONYMOUS != 0;
    if len == 0 || len > MAX_MAP || offset & 0xfff != 0 || (fixed && addr & 0xfff != 0) {
        return -EINVAL;
    }
    if flags & (MAP_SHARED | MAP_PRIVATE) == 0 {
        return -EINVAL;
    }
    if !anon && flags & MAP_SHARED != 0 && prot & PROT_WRITE != 0 {
        return -ENODEV; // writing through to the file isn't supported
    }
    let len = page_up(len);
    // A bad fd leaves nothing mapped.
    if !anon {
        let n = crate::vfs::sys_pread(fd, &mut [0u8; 1], offset);
        if n < 0 {
            return n;
        }
    }

    let mut maps = process::with_current_mm(|mm| core::mem::take(&mut mm.maps));
    let result = (|| {
        let start = if fixed {
            if !in_user_space(addr, addr + len) {
                return Err(ENOMEM);
            }
            if flags & MAP_FIXED_NOREPLACE != 0 && in_use(&maps, addr, addr + len) {
                return Err(EEXIST);
            }
            unmap_pages(addr, addr + len);
            forget(&mut maps, addr, addr + len);
            addr
        } else {
            find_free(&maps, addr & !0xfff, len).ok_or(ENOMEM)?
        };
        if !anon {
            // A file: its bytes now, a page at a time (past its end: zeros).
            // PROT_NONE pages keep the data, without access for now.
            let f = flags_for(prot).unwrap_or(F::PRESENT);
            let mut buf = [0u8; 4096];
            let mut at_end = false;
            for a in (start..start + len).step_by(4096) {
                crate::interrupts::let_in(); // (big mappings: keys typed meanwhile aren't lost)
                let mut n = 0usize;
                while !at_end && n < 4096 {
                    let r = crate::vfs::sys_pread(fd, &mut buf[n..], offset + (a - start) + n as u64);
                    if r <= 0 {
                        at_end = true;
                    } else {
                        n += r as usize;
                    }
                }
                if map_page(a, f, &buf[..n]).is_err() {
                    unmap_pages(start, a);
                    return Err(ENOMEM);
                }
            }
        }
        remember(&mut maps, start, start + len, prot);
        Ok(start)
    })();
    process::with_current_mm(|mm| mm.maps = maps);
    match result {
        Ok(start) => start as i64,
        Err(e) => -e,
    }
}

/// munmap(addr, length)
pub fn sys_munmap(addr: u64, len: u64) -> i64 {
    if addr & 0xfff != 0 || len == 0 {
        return -EINVAL;
    }
    let end = addr.saturating_add(page_up(len));
    if !in_user_space(addr, end) {
        return -EINVAL;
    }
    unmap_pages(addr, end);
    process::with_current_mm(|mm| forget(&mut mm.maps, addr, end));
    0
}

/// mprotect(addr, length, prot): change access to memory. Pages already
/// there change now; mmap'd pages not touched yet just get the new
/// protection (memory comes when they're used).
pub fn sys_mprotect(addr: u64, len: u64, prot: u64) -> i64 {
    if addr & 0xfff != 0 {
        return -EINVAL;
    }
    let end = addr.saturating_add(page_up(len));
    if len == 0 {
        return 0;
    }
    if !in_user_space(addr, end) {
        return -ENOMEM;
    }
    let p4 = process::current_p4();
    let mut mapped = Vec::new();
    memory::for_each_mapped(p4, addr, end, |a| mapped.push(a));
    // Everything in the range must be mmap'd or mapped (the program, brk).
    let ok = process::with_current_mm(|mm| {
        let mut cur = addr;
        let mut covered = true;
        let mut check_gap = |from: u64, to: u64| {
            let pages = (to - from) / 4096;
            let there = mapped.iter().filter(|&&a| from <= a && a < to).count() as u64;
            if there != pages {
                covered = false;
            }
        };
        for &(s, e, _) in mm.maps.iter() {
            if e <= cur || s >= end {
                continue;
            }
            if s > cur {
                check_gap(cur, s);
            }
            cur = cur.max(e);
        }
        if cur < end {
            check_gap(cur, end);
        }
        if covered {
            // The new protection for the mmap'd parts.
            let parts: Vec<(u64, u64)> = mm.maps.iter().filter(|&&(s, e, _)| s < end && addr < e).map(|&(s, e, _)| (s.max(addr), e.min(end))).collect();
            for (s, e) in parts {
                forget(&mut mm.maps, s, e);
                remember(&mut mm.maps, s, e, prot);
            }
        }
        covered
    });
    if !ok {
        return -ENOMEM;
    }
    let flags = flags_for(prot);
    let mut mapper = memory::mapper_for(p4);
    for a in mapped {
        // PROT_NONE keeps the page (and its data) but denies user access.
        let mut f = flags.unwrap_or(F::PRESENT);
        // A page shared with a fork stays read-only (copy-on-write):
        // the first write gets a private copy.
        let frame = mapper.translate_page(page(a)).ok();
        let shared = frame.is_some_and(|fr| FRAME_ALLOCATOR.lock().as_ref().is_some_and(|fa| fa.users(fr) > 1));
        if shared && f.contains(F::WRITABLE) {
            f = (f - F::WRITABLE) | memory::COW;
        }
        if let Ok(flush) = unsafe { mapper.update_flags(page(a), f) } {
            flush.flush();
        }
    }
    0
}

/// mremap(addr, old_len, new_len, flags): resizing isn't supported (libc
/// falls back to allocating and copying): ENOMEM, or EFAULT if the old
/// range isn't memory at all, as on Linux. (musl measures the main
/// thread's stack by probing downwards until EFAULT.)
pub fn sys_mremap(addr: u64, old_len: u64) -> i64 {
    if addr & 0xfff != 0 {
        return -EINVAL;
    }
    let end = addr.saturating_add(page_up(old_len.max(1)));
    if !in_user_space(addr, end) {
        return -crate::syscall::EFAULT;
    }
    let mut mapped = 0u64;
    memory::for_each_mapped(process::current_p4(), addr, end, |_| mapped += 1);
    let reserved = process::with_current_mm(|mm| mm.maps.iter().any(|&(s, e, _)| s <= addr && end <= e));
    if mapped == (end - addr) / 4096 || reserved {
        -ENOMEM
    } else {
        -crate::syscall::EFAULT
    }
}

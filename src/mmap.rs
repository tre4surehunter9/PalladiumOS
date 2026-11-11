// Copyright (c) 2026 tre4surehunter9
//! mmap, munmap, mprotect, msync: memory for programs beyond the brk heap.
//!
//! musl's malloc gets most of its memory this way.
//!
//! - Private anonymous memory is zero-filled and gets its pages when first
//!   touched (the page fault handler calls fault_in), so reserving
//!   gigabytes of address space costs nothing until it's used, as on Linux
//!   (V8, Go and others reserve far more than they use). After fork, the
//!   pages are copy-on-write.
//! - Private file mappings are a copy of the file's bytes, read in right
//!   away, a page at a time.
//! - Shared mappings (MAP_SHARED) get their pages from a SharedMem object
//!   that every mapping of the same memory refers to: the anonymous memory
//!   of one mmap call (shared with the children it's forked to), or a file
//!   (one object per file, for every process that maps it: /dev/shm,
//!   named semaphores). Pages come on first touch; a file's are read from
//!   it, and written back on msync and when its last mapping goes away.
//!   Their page table entries are marked memory::SHARED, so fork leaves
//!   them writable in both processes instead of copy-on-write. read() and
//!   write() of a mapped file stay in step with the mapping: writes go to
//!   the file and into the pages in memory, and reads see the pages (with
//!   changes not written back yet) over the file (see file_read_overlay,
//!   file_written, file_truncated).
//!
//! Without MAP_FIXED, addresses are picked from [MMAP_BASE, MMAP_END),
//! between the brk heap and the stack.

use alloc::collections::BTreeMap;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use spin::Mutex;
use x86_64::structures::paging::{
    mapper::TranslateResult, FrameAllocator, FrameDeallocator, Mapper, Page, PageTableFlags as F, PhysFrame, Size4KiB,
    Translate,
};
use x86_64::VirtAddr;

use crate::memory::{self, FRAME_ALLOCATOR};
use crate::process;
use crate::syscall::{EACCES, EEXIST, EINVAL, ENODEV, ENOMEM};

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
/// The largest single mapping: the whole mmap area (address space only:
/// memory comes when used). MariaDB reserves 8 TiB for its buffer pool.
const MAX_MAP: u64 = MMAP_END - MMAP_BASE;

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

// ---------------------------------------------------------------- shared memory

/// Memory that several mappings share: its pages by index, each held here
/// (one reference) and by every page table entry mapping it (one each).
pub struct SharedMem {
    pages: Mutex<BTreeMap<u64, PhysFrame>>,
    /// The file it mirrors: (mount, inode).
    file: Option<(usize, crate::mounts::Ino)>,
}

/// Every file's SharedMem, so all processes mapping a file share one.
static SHARED_FILES: Mutex<Vec<((usize, crate::mounts::Ino), Weak<SharedMem>)>> = Mutex::new(Vec::new());

/// Frames to free once the frame allocator is free (a SharedMem dropped
/// while it was busy, e.g. as a process exits).
static DEFERRED: Mutex<Vec<PhysFrame>> = Mutex::new(Vec::new());

fn drain_deferred() {
    let frames: Vec<PhysFrame> = core::mem::take(&mut *DEFERRED.lock());
    if !frames.is_empty() {
        let mut alloc = FRAME_ALLOCATOR.lock();
        let fa = alloc.as_mut().unwrap();
        for f in frames {
            fa.release(f);
        }
    }
}

impl SharedMem {
    fn anonymous() -> Arc<SharedMem> {
        Arc::new(SharedMem { pages: Mutex::new(BTreeMap::new()), file: None })
    }

    /// The SharedMem of a file (made if no mapping of it exists).
    fn of_file(mount: usize, ino: crate::mounts::Ino) -> Arc<SharedMem> {
        let mut files = SHARED_FILES.lock();
        files.retain(|(_, w)| w.strong_count() > 0);
        if let Some(m) = files.iter().find(|(k, _)| *k == (mount, ino)).and_then(|(_, w)| w.upgrade()) {
            return m;
        }
        let m = Arc::new(SharedMem { pages: Mutex::new(BTreeMap::new()), file: Some((mount, ino)) });
        files.push(((mount, ino), Arc::downgrade(&m)));
        m
    }

    /// Page `index`, made on first use (zeros, or the file's bytes).
    fn page(&self, index: u64) -> Option<PhysFrame> {
        let mut pages = self.pages.lock();
        if let Some(&f) = pages.get(&index) {
            return Some(f);
        }
        let frame = FRAME_ALLOCATOR.lock().as_mut()?.allocate_frame()?;
        memory::zero_frame(frame);
        if let Some((mount, ino)) = self.file {
            let buf = unsafe { core::slice::from_raw_parts_mut(memory::phys_ptr(frame.start_address()), 4096) };
            let _ = crate::mounts::with_fs(mount, |fs| fs.read(ino, index * 4096, buf)); // (past the end: zeros)
        }
        pages.insert(index, frame);
        Some(frame)
    }

    /// Write a file's pages back to it (within its current size: a mapping
    /// doesn't make a file longer).
    fn write_back(&self) {
        let Some((mount, ino)) = self.file else { return };
        let pages = self.pages.lock();
        let _ = crate::mounts::with_fs(mount, |fs| {
            let size = fs.meta(ino)?.size;
            for (&index, &frame) in pages.iter() {
                let off = index * 4096;
                if off >= size {
                    continue;
                }
                let n = (size - off).min(4096) as usize;
                let data = unsafe { core::slice::from_raw_parts(memory::phys_ptr(frame.start_address()), n) };
                fs.write(ino, off, data)?;
            }
            Ok::<(), i64>(())
        });
    }
}

impl Drop for SharedMem {
    fn drop(&mut self) {
        self.write_back();
        let pages = core::mem::take(&mut *self.pages.lock());
        match FRAME_ALLOCATOR.try_lock() {
            Some(mut alloc) => {
                let fa = alloc.as_mut().unwrap();
                for (_, f) in pages {
                    fa.release(f);
                }
            }
            None => DEFERRED.lock().extend(pages.into_values()),
        }
    }
}

/// The shared memory of a file, if something maps it right now.
fn mapped_file(mount: usize, ino: crate::mounts::Ino) -> Option<Arc<SharedMem>> {
    let files = SHARED_FILES.lock();
    files.iter().find(|(k, _)| *k == (mount, ino)).and_then(|(_, w)| w.upgrade())
}

/// The pages of `mem` overlapping [offset, offset + len): (index, frame).
fn pages_in(mem: &SharedMem, offset: u64, len: usize) -> Vec<(u64, PhysFrame)> {
    if len == 0 {
        return Vec::new();
    }
    let (first, last) = (offset / 4096, (offset + len as u64 - 1) / 4096);
    mem.pages.lock().range(first..=last).map(|(&i, &f)| (i, f)).collect()
}

/// read() of a file just put `buf` (from `offset`): if the file is mapped,
/// the mapping's pages in memory are newer, so they go over it.
pub fn file_read_overlay(mount: usize, ino: crate::mounts::Ino, offset: u64, buf: &mut [u8]) {
    let Some(mem) = mapped_file(mount, ino) else { return };
    for (index, frame) in pages_in(&mem, offset, buf.len()) {
        let page_start = index * 4096;
        let from = offset.max(page_start);
        let to = (offset + buf.len() as u64).min(page_start + 4096);
        let src = unsafe { core::slice::from_raw_parts(memory::phys_ptr(frame.start_address()), 4096) };
        buf[(from - offset) as usize..(to - offset) as usize].copy_from_slice(&src[(from - page_start) as usize..(to - page_start) as usize]);
    }
}

/// write() put `data` in a file at `offset`: a mapping of it sees it too.
pub fn file_written(mount: usize, ino: crate::mounts::Ino, offset: u64, data: &[u8]) {
    let Some(mem) = mapped_file(mount, ino) else { return };
    for (index, frame) in pages_in(&mem, offset, data.len()) {
        let page_start = index * 4096;
        let from = offset.max(page_start);
        let to = (offset + data.len() as u64).min(page_start + 4096);
        let dst = unsafe { core::slice::from_raw_parts_mut(memory::phys_ptr(frame.start_address()), 4096) };
        dst[(from - page_start) as usize..(to - page_start) as usize].copy_from_slice(&data[(from - offset) as usize..(to - offset) as usize]);
    }
}

/// A file was truncated to `len`: what a mapping holds past the end is
/// gone (zeros if the file grows again).
pub fn file_truncated(mount: usize, ino: crate::mounts::Ino, len: u64) {
    let Some(mem) = mapped_file(mount, ino) else { return };
    for (&index, &frame) in mem.pages.lock().range(len / 4096..) {
        let page_start = index * 4096;
        let keep = len.saturating_sub(page_start).min(4096) as usize;
        let page = unsafe { core::slice::from_raw_parts_mut(memory::phys_ptr(frame.start_address()), 4096) };
        page[keep..].fill(0);
    }
}

// ---------------------------------------------------------------- mappings

/// An mmap'd range [start, end) and its protection; for a shared mapping,
/// its SharedMem and the index of the page at `start` in it.
#[derive(Clone)]
pub struct Map {
    pub start: u64,
    pub end: u64,
    pub prot: u64,
    shared: Option<(Arc<SharedMem>, u64)>,
}

type Maps = Vec<Map>;

/// Is any page in [start, end) mapped, or reserved by an earlier mmap?
fn in_use(maps: &[Map], start: u64, end: u64) -> bool {
    if maps.iter().any(|m| m.start < end && start < m.end) {
        return true;
    }
    let mut any = false;
    memory::for_each_mapped(process::current_p4(), start, end, |_| any = true);
    any
}

/// The lowest free range of `len` bytes in the mmap area (or at `hint`).
fn find_free(maps: &[Map], hint: u64, len: u64) -> Option<u64> {
    if hint != 0 && hint & 0xfff == 0 && in_user_space(hint, hint + len) && !in_use(maps, hint, hint + len) {
        return Some(hint);
    }
    let mut cur = MMAP_BASE;
    for m in maps {
        if m.end <= cur {
            continue;
        }
        if m.start >= cur + len {
            break;
        }
        cur = page_up(m.end);
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
        let device = matches!(mapper.translate(VirtAddr::new(a)), TranslateResult::Mapped { flags, .. } if flags.contains(memory::DEVICE));
        if let Ok((frame, flush)) = mapper.unmap(page(a)) {
            flush.flush();
            if !device {
                frames.release(frame); // freed when nothing else uses it
            }
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

/// Map a shared page (it gets another user).
fn map_shared_frame(addr: u64, frame: PhysFrame, flags: F) -> Result<(), ()> {
    let mut mapper = memory::mapper_for(process::current_p4());
    let mut alloc = FRAME_ALLOCATOR.lock();
    let frames = alloc.as_mut().unwrap();
    match unsafe { mapper.map_to(page(addr), frame, flags | memory::SHARED, frames) } {
        Ok(flush) => {
            flush.flush();
            frames.share(frame);
            Ok(())
        }
        Err(_) => Err(()),
    }
}

/// Split the mapping containing `x` (if any) in two at `x`.
fn split_at(maps: &mut Maps, x: u64) {
    if let Some(i) = maps.iter().position(|m| m.start < x && x < m.end) {
        let mut right = maps[i].clone();
        right.start = x;
        if let Some((_, base)) = right.shared.as_mut() {
            *base += (x - maps[i].start) / 4096;
        }
        maps[i].end = x;
        maps.insert(i + 1, right);
    }
}

/// Remove [start, end) from the list of mappings (splitting as needed).
fn forget(maps: &mut Maps, start: u64, end: u64) {
    split_at(maps, start);
    split_at(maps, end);
    maps.retain(|m| m.end <= start || m.start >= end);
}

fn remember(maps: &mut Maps, map: Map) {
    let i = maps.partition_point(|m| m.start < map.start);
    maps.insert(i, map);
}

/// The page at `addr` was touched for the first time: if it's mmap'd
/// memory the program may use, give it its page (zeroed, or the shared
/// one). False if it isn't (a real segmentation fault).
pub fn fault_in(addr: u64) -> bool {
    let addr = addr & !0xfff;
    let Some(found) = process::try_with_current_mm(|mm| mm.maps.iter().find(|m| m.start <= addr && addr < m.end).cloned()) else {
        return false;
    };
    let Some(map) = found else { return false };
    let Some(f) = flags_for(map.prot) else { return false };
    let mapper = memory::mapper_for(process::current_p4());
    if mapper.translate_addr(VirtAddr::new(addr)).is_some() {
        return true;
    }
    drain_deferred();
    match &map.shared {
        Some((mem, base)) => match mem.page(base + (addr - map.start) / 4096) {
            Some(frame) => map_shared_frame(addr, frame, f).is_ok(),
            None => false,
        },
        None => map_page(addr, f, &[]).is_ok(),
    }
}

/// mmap(addr, length, prot, flags, fd, offset)
pub fn sys_mmap(addr: u64, len: u64, prot: u64, flags: u64, fd: u64, offset: u64) -> i64 {
    let fixed = flags & (MAP_FIXED | MAP_FIXED_NOREPLACE) != 0;
    let anon = flags & MAP_ANONYMOUS != 0;
    let shared = flags & MAP_SHARED != 0;
    if len == 0 || len > MAX_MAP || offset & 0xfff != 0 || (fixed && addr & 0xfff != 0) {
        return -EINVAL;
    }
    if flags & (MAP_SHARED | MAP_PRIVATE) == 0 {
        return -EINVAL;
    }
    let len = page_up(len);
    drain_deferred();
    // The framebuffer (/dev/fb0): its device memory, mapped as it is.
    if let Some((phys, size)) = crate::vfs::fd_framebuffer(fd).filter(|_| !anon) {
        if !shared || offset.saturating_add(len) > page_up(size) {
            return -EINVAL;
        }
        return map_device(addr, len, prot, flags, phys + offset);
    }
    // The shared memory to map, if shared: a bad fd leaves nothing mapped.
    let mem = if shared && anon {
        Some((SharedMem::anonymous(), 0))
    } else if shared {
        match crate::vfs::fd_file(fd) {
            Ok((loc, writable)) => {
                if prot & PROT_WRITE != 0 && !writable {
                    return -EACCES; // writing to the file needs it open for writing
                }
                Some((SharedMem::of_file(loc.mount, loc.ino), offset / 4096))
            }
            Err(_) => return -ENODEV,
        }
    } else {
        if !anon {
            let n = crate::vfs::sys_pread(fd, &mut [0u8; 1], offset);
            if n < 0 {
                return n;
            }
        }
        None
    };

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
        if !anon && !shared {
            // A private file mapping: its bytes now, a page at a time (past
            // its end: zeros). PROT_NONE pages keep the data, without access.
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
        remember(&mut maps, Map { start, end: start + len, prot, shared: mem });
        Ok(start)
    })();
    process::with_current_mm(|mm| mm.maps = maps);
    match result {
        Ok(start) => start as i64,
        Err(e) => -e,
    }
}

/// Map device memory at `phys` (the framebuffer): eagerly, uncounted.
fn map_device(addr: u64, len: u64, prot: u64, flags: u64, phys: u64) -> i64 {
    let fixed = flags & (MAP_FIXED | MAP_FIXED_NOREPLACE) != 0;
    let f = flags_for(prot).unwrap_or(F::PRESENT) | memory::DEVICE | F::NO_CACHE | F::WRITE_THROUGH;
    let mut maps = process::with_current_mm(|mm| core::mem::take(&mut mm.maps));
    let result = (|| {
        let start = if fixed {
            if !in_user_space(addr, addr + len) {
                return Err(ENOMEM);
            }
            unmap_pages(addr, addr + len);
            forget(&mut maps, addr, addr + len);
            addr
        } else {
            find_free(&maps, addr & !0xfff, len).ok_or(ENOMEM)?
        };
        let mut mapper = memory::mapper_for(process::current_p4());
        let mut alloc = FRAME_ALLOCATOR.lock();
        let frames = alloc.as_mut().unwrap();
        for i in 0..len / 4096 {
            let frame = PhysFrame::containing_address(x86_64::PhysAddr::new(phys + i * 4096));
            match unsafe { mapper.map_to(page(start + i * 4096), frame, f, frames) } {
                Ok(flush) => flush.flush(),
                Err(_) => return Err(ENOMEM),
            }
        }
        remember(&mut maps, Map { start, end: start + len, prot, shared: None });
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
    drain_deferred();
    unmap_pages(addr, end);
    // (Taken out first: a SharedMem dropped here writes its file back,
    // which mustn't happen with the memory description locked.)
    let mut maps = process::with_current_mm(|mm| core::mem::take(&mut mm.maps));
    forget(&mut maps, addr, end);
    process::with_current_mm(|mm| mm.maps = maps);
    0
}

/// msync(addr, length, flags): write the shared file mappings in the range
/// back to their files.
pub fn sys_msync(addr: u64, len: u64) -> i64 {
    if addr & 0xfff != 0 {
        return -EINVAL;
    }
    let end = addr.saturating_add(page_up(len));
    let mems: Vec<Arc<SharedMem>> = process::with_current_mm(|mm| {
        mm.maps.iter().filter(|m| m.start < end && addr < m.end).filter_map(|m| m.shared.as_ref().map(|(s, _)| s.clone())).collect()
    });
    for m in mems {
        m.write_back();
    }
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
        for m in mm.maps.iter() {
            if m.end <= cur || m.start >= end {
                continue;
            }
            if m.start > cur {
                check_gap(cur, m.start);
            }
            cur = cur.max(m.end);
        }
        if cur < end {
            check_gap(cur, end);
        }
        if covered {
            // The new protection for the mmap'd parts.
            split_at(&mut mm.maps, addr);
            split_at(&mut mm.maps, end);
            for m in mm.maps.iter_mut().filter(|m| addr <= m.start && m.end <= end) {
                m.prot = prot;
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
        let current = match mapper.translate(VirtAddr::new(a)) {
            TranslateResult::Mapped { flags, .. } => flags,
            _ => continue,
        };
        if current.contains(memory::SHARED) {
            // Shared memory stays shared: never copy-on-write.
            f |= memory::SHARED;
        } else {
            // A page shared with a fork stays read-only (copy-on-write):
            // the first write gets a private copy.
            let frame = mapper.translate_page(page(a)).ok();
            let forked = frame.is_some_and(|fr| FRAME_ALLOCATOR.lock().as_ref().is_some_and(|fa| fa.users(fr) > 1));
            if forked && f.contains(F::WRITABLE) {
                f = (f - F::WRITABLE) | memory::COW;
            }
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
    let reserved = process::with_current_mm(|mm| mm.maps.iter().any(|m| m.start <= addr && end <= m.end));
    if mapped == (end - addr) / 4096 || reserved {
        -ENOMEM
    } else {
        -crate::syscall::EFAULT
    }
}

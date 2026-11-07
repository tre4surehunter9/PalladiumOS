// Copyright (c) 2026 tre4surehunter9
use x86_64::{
    structures::paging::PageTable,
    VirtAddr,
};

use x86_64::PhysAddr;


use x86_64::structures::paging::OffsetPageTable;


pub unsafe fn init(physical_memory_offset: VirtAddr) -> OffsetPageTable<'static> {
    unsafe {
        let level_4_table = active_level_4_table(physical_memory_offset);
        OffsetPageTable::new(level_4_table, physical_memory_offset)
    }
}

use spin::Mutex;

pub static MAPPER: Mutex<Option<OffsetPageTable<'static>>> = Mutex::new(None);
pub static FRAME_ALLOCATOR: Mutex<Option<BootInfoFrameAllocator>> = Mutex::new(None);

/// Virtual address where the bootloader mapped all of physical memory.
/// Set once at boot by kernel_main; used to walk page tables later.
pub static PHYS_MEM_OFFSET: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

unsafe fn active_level_4_table(physical_memory_offset: VirtAddr)
-> &'static mut PageTable
{
    use x86_64::registers::control::Cr3;

    let (level_4_table_frame, _) = Cr3::read();

    let phys = level_4_table_frame.start_address();
    let virt = physical_memory_offset + phys.as_u64();
    let page_table_ptr: *mut PageTable = virt.as_mut_ptr();

    unsafe { &mut *page_table_ptr }
}



pub unsafe fn translate_addr(addr: VirtAddr, physical_memory_offset: VirtAddr)
-> Option<PhysAddr>
{
    translate_addr_inner(addr, physical_memory_offset)
}


fn translate_addr_inner(addr: VirtAddr, physical_memory_offset: VirtAddr)
-> Option<PhysAddr>
{
    use x86_64::structures::paging::page_table::FrameError;
    use x86_64::structures::paging::PageTable;
    use x86_64::registers::control::Cr3;




    let (level_4_table_frame, _) = Cr3::read();

    let table_indexes = [
        addr.p4_index(), addr.p3_index(), addr.p2_index(), addr.p1_index()
    ];
    let mut frame = level_4_table_frame;

    for &index in &table_indexes {
        let virt = physical_memory_offset + frame.start_address().as_u64();
        let table_ptr: *const PageTable = virt.as_ptr();
        let table = unsafe {&*table_ptr};
        let entry = &table[index];
        frame = match entry.frame() {
            Ok(frame) => frame,
            Err(FrameError::FrameNotPresent) => return None,
            Err(FrameError::HugeFrame) => panic!("huge pages not supported"),
        };
    }

    Some(frame.start_address() + u64::from(addr.page_offset()))
}

use x86_64::structures::paging::{Page, PhysFrame, Mapper, Size4KiB, FrameAllocator, FrameDeallocator, PageTableFlags};

pub fn create_example_mapping(
    page: Page,
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) {
    use x86_64::structures::paging::PageTableFlags as Flags;

    let frame = PhysFrame::containing_address(PhysAddr::new(0xb8000));
    let flags = Flags::PRESENT | Flags::WRITABLE;

    let map_to_result = unsafe {
        mapper.map_to(page, frame, flags, frame_allocator)
    };
    map_to_result.expect("map_to failed").flush();
}

pub struct EmptyFrameAllocator;

unsafe impl FrameAllocator<Size4KiB> for EmptyFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        None
    }
}


use bootloader::bootinfo::MemoryMap;

pub struct BootInfoFrameAllocator {
    memory_map: &'static MemoryMap,
    /// Frames handed out from the memory map so far (not from the free list).
    next: usize,
    /// Where the next never-used frame comes from: a memory map region and
    /// an address in it. (Searching from the start each time made handing
    /// out n frames take n^2 steps: seconds at boot for the heap alone.)
    region: usize,
    next_addr: u64,
    /// Physical address of the first freed frame. Each free frame stores the
    /// address of the next one in its first 8 bytes (an intrusive linked list).
    free_list: Option<u64>,
    free_count: usize,
    total: usize,
    /// How many address spaces use each frame shared by fork (copy-on-
    /// write), indexed by frame number; 0 means "not shared" (one user).
    /// A flat array: fork and exit touch every page, so this must be O(1).
    shared: alloc::vec::Vec<u16>,
}

impl BootInfoFrameAllocator {

    pub unsafe fn init(memory_map: &'static MemoryMap) -> Self {
        BootInfoFrameAllocator {
            memory_map,
            next: 0,
            region: 0,
            next_addr: memory_map.first().map_or(0, |r| r.range.start_addr()),
            free_list: None,
            free_count: 0,
            total: 0,
            shared: alloc::vec::Vec::new(),
        }
    }

    /// Number of frames handed out and not yet freed.
    pub fn frames_in_use(&self) -> usize {
        self.next - self.free_count
    }

    fn index(frame: PhysFrame) -> usize {
        (frame.start_address().as_u64() / 4096) as usize
    }

    /// One more address space uses `frame` (fork shared it).
    pub fn share(&mut self, frame: PhysFrame) {
        if self.shared.is_empty() {
            // One counter per frame of usable RAM (allocated on the first
            // fork). Only usable regions hand out frames; the map also lists
            // device regions far above the RAM.
            let top = self
                .memory_map
                .iter()
                .filter(|r| r.region_type == MemoryRegionType::Usable)
                .map(|r| r.range.end_addr())
                .max()
                .unwrap_or(0);
            self.shared = alloc::vec![0; (top / 4096) as usize];
        }
        let n = &mut self.shared[Self::index(frame)];
        *n = if *n == 0 { 2 } else { *n + 1 };
    }

    /// How many address spaces use `frame`.
    pub fn users(&self, frame: PhysFrame) -> u32 {
        self.shared.get(Self::index(frame)).map_or(1, |&n| (n as u32).max(1))
    }

    /// An address space stopped using a user page's frame: free it if it
    /// was the last one. (Use this, not deallocate_frame, for user pages.)
    pub fn release(&mut self, frame: PhysFrame) {
        if let Some(n) = self.shared.get_mut(Self::index(frame)) {
            if *n >= 2 {
                *n = if *n == 2 { 0 } else { *n - 1 };
                return;
            }
        }
        unsafe { self.deallocate_frame(frame) };
    }

    /// Number of usable frames in the machine (counted once, then cached).
    pub fn total_frames(&mut self) -> usize {
        if self.total == 0 {
            self.total = self.usable_frames().count();
        }
        self.total
    }
}


use bootloader::bootinfo::MemoryRegionType;

impl BootInfoFrameAllocator {
    fn usable_frames(&self) -> impl Iterator<Item = PhysFrame> {
        let regions = self.memory_map.iter();
        let usable_regions = regions
        .filter(|r| r.region_type == MemoryRegionType::Usable);
        let addr_ranges = usable_regions
        .map(|r| r.range.start_addr()..r.range.end_addr());
        let frame_addresses = addr_ranges.flat_map(|r| r.step_by(4096));
        frame_addresses.map(|addr| PhysFrame::containing_address(PhysAddr::new(addr)))
    }
}

/// Pointer to a physical address through the bootloader's physical memory map.
fn phys_to_virt(phys: u64) -> *mut u64 {
    let offset = PHYS_MEM_OFFSET.load(core::sync::atomic::Ordering::Relaxed);
    assert!(offset != 0, "PHYS_MEM_OFFSET not set");
    (offset + phys) as *mut u64
}

unsafe impl FrameAllocator<Size4KiB> for BootInfoFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        if let Some(phys) = self.free_list {
            self.free_list = match unsafe { phys_to_virt(phys).read() } {
                u64::MAX => None,
                next => Some(next),
            };
            self.free_count -= 1;
            return Some(PhysFrame::containing_address(PhysAddr::new(phys)));
        }
        while let Some(r) = self.memory_map.get(self.region) {
            if r.region_type == MemoryRegionType::Usable && self.next_addr < r.range.end_addr() {
                let frame = PhysFrame::containing_address(PhysAddr::new(self.next_addr));
                self.next_addr += 4096;
                self.next += 1;
                return Some(frame);
            }
            self.region += 1;
            self.next_addr = self.memory_map.get(self.region).map_or(0, |r| r.range.start_addr());
        }
        None
    }
}

impl FrameDeallocator<Size4KiB> for BootInfoFrameAllocator {
    unsafe fn deallocate_frame(&mut self, frame: PhysFrame) {
        let phys = frame.start_address().as_u64();
        let next = self.free_list.unwrap_or(u64::MAX);
        unsafe { phys_to_virt(phys).write(next) };
        self.free_list = Some(phys);
        self.free_count += 1;
    }
}

/// Fill a physical frame with zeros (through the physical memory map).
pub fn zero_frame(frame: PhysFrame) {
    let ptr = phys_to_virt(frame.start_address().as_u64()) as *mut u8;
    unsafe { core::ptr::write_bytes(ptr, 0, 4096) };
}

/// Kernel-virtual pointer to a physical address, for writing into user
/// frames regardless of the user mapping's permissions.
pub fn phys_ptr(phys: PhysAddr) -> *mut u8 {
    phys_to_virt(phys.as_u64()) as *mut u8
}



/// Highest address (exclusive) of the lower, user half of the address space.
pub const USER_SPACE_END: u64 = 0x0000_8000_0000_0000;

/// Check that `[ptr, ptr + len)` is entirely user memory: every page must be
/// present and USER_ACCESSIBLE at every page-table level (and WRITABLE too
/// if `write` is set). Syscalls must call this before touching user pointers,
/// otherwise a ring-3 program could make the kernel read or write kernel memory.
pub fn is_user_range(ptr: u64, len: u64, write: bool) -> bool {
    if len == 0 {
        return true;
    }
    let end = match ptr.checked_add(len) {
        Some(end) if end <= USER_SPACE_END => end,
        _ => return false,
    };

    let mut page = ptr & !0xfff;
    while page < end {
        if !is_user_page(VirtAddr::new(page), write) {
            // A shared copy-on-write page: the kernel is about to write it
            // for the program (read() into a buffer...), so copy it first.
            let cow = write && is_user_page(VirtAddr::new(page), false) && break_cow_current(page);
            if !cow {
                return false;
            }
        }
        page += 4096;
    }
    true
}

fn is_user_page(addr: VirtAddr, write: bool) -> bool {
    use x86_64::registers::control::Cr3;
    use x86_64::structures::paging::PageTableFlags as Flags;
    use core::sync::atomic::Ordering;

    let offset = PHYS_MEM_OFFSET.load(Ordering::Relaxed);
    if offset == 0 {
        return false;
    }

    let mut required = Flags::PRESENT | Flags::USER_ACCESSIBLE;
    if write {
        required |= Flags::WRITABLE;
    }

    let (level_4_frame, _) = Cr3::read();
    let indexes = [addr.p4_index(), addr.p3_index(), addr.p2_index(), addr.p1_index()];
    let mut table_phys = level_4_frame.start_address().as_u64();

    for (level, &index) in indexes.iter().enumerate() {
        let table = unsafe { &*((offset + table_phys) as *const PageTable) };
        let entry = &table[index];
        let flags = entry.flags();
        if !flags.contains(required) {
            return false;
        }
        // A huge page at the P3 (1 GiB) or P2 (2 MiB) level ends the walk.
        if level > 0 && level < 3 && flags.contains(Flags::HUGE_PAGE) {
            return true;
        }
        table_phys = entry.addr().as_u64();
    }
    true
}

// ---- per-process address spaces ----
//
// Every process has its own level-4 page table. The entries the kernel uses
// (kernel image, heap, physical memory map, ...) are copied from the kernel's
// table, so they point at the same lower-level tables and the kernel is
// mapped identically in every process. All other lower-half entries are
// "user slots": they belong to the process alone and are freed with it.

use x86_64::structures::paging::{OffsetPageTable as Opt, PageTableIndex};

/// Physical address of the kernel's own level-4 table (set by init_address_spaces).
static KERNEL_P4: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Remember the boot page table as the kernel's. Call once after PHYS_MEM_OFFSET is set.
pub fn init_address_spaces() {
    use x86_64::registers::control::Cr3;
    let (frame, _) = Cr3::read();
    KERNEL_P4.store(frame.start_address().as_u64(), core::sync::atomic::Ordering::Relaxed);
}

pub fn kernel_p4() -> PhysFrame {
    PhysFrame::containing_address(PhysAddr::new(KERNEL_P4.load(core::sync::atomic::Ordering::Relaxed)))
}

fn table_at(frame: PhysFrame) -> &'static mut PageTable {
    unsafe { &mut *(phys_to_virt(frame.start_address().as_u64()) as *mut PageTable) }
}

/// True if level-4 index `i` may hold user mappings (lower half, unused by the kernel).
pub fn is_user_slot(i: usize) -> bool {
    i < 256 && table_at(kernel_p4())[i].is_unused()
}

/// A page-table mapper for the address space rooted at `p4`
/// (it doesn't have to be the active one).
pub fn mapper_for(p4: PhysFrame) -> Opt<'static> {
    let offset = VirtAddr::new(PHYS_MEM_OFFSET.load(core::sync::atomic::Ordering::Relaxed));
    unsafe { Opt::new(table_at(p4), offset) }
}

/// Create an empty address space: only the kernel's mappings.
pub fn new_address_space(frames: &mut BootInfoFrameAllocator) -> Option<PhysFrame> {
    let frame = frames.allocate_frame()?;
    zero_frame(frame);
    let new = table_at(frame);
    let kernel = table_at(kernel_p4());
    for i in 0..512 {
        if !kernel[i].is_unused() {
            new[i] = kernel[i].clone();
        }
    }
    Some(frame)
}

/// Free every user page and page table of an address space, then the
/// level-4 table itself. It must not be the active address space.
pub fn free_address_space(p4: PhysFrame, frames: &mut BootInfoFrameAllocator) {
    fn free_table(table: PhysFrame, level: u8, frames: &mut BootInfoFrameAllocator) {
        for entry in table_at(table).iter() {
            if entry.is_unused() {
                continue;
            }
            let child = PhysFrame::containing_address(entry.addr());
            if level > 1 {
                free_table(child, level - 1, frames);
            } else {
                frames.release(child); // shared pages stay until their last user
            }
        }
        unsafe { frames.deallocate_frame(table) };
    }

    let root = table_at(p4);
    for i in 0..256 {
        if is_user_slot(i) && !root[i].is_unused() {
            free_table(PhysFrame::containing_address(root[i].addr()), 3, frames);
        }
    }
    unsafe { frames.deallocate_frame(p4) };
}

/// Call `f(virtual address, frame, flags)` for every user page in an address space.
fn for_each_user_page(p4: PhysFrame, mut f: impl FnMut(u64, PhysFrame, PageTableFlags) -> Result<(), &'static str>) -> Result<(), &'static str> {
    let l4 = table_at(p4);
    for i4 in 0..256 {
        if !is_user_slot(i4) || l4[i4].is_unused() {
            continue;
        }
        let l3 = table_at(PhysFrame::containing_address(l4[i4].addr()));
        for i3 in 0..512 {
            if l3[i3].is_unused() {
                continue;
            }
            let l2 = table_at(PhysFrame::containing_address(l3[i3].addr()));
            for i2 in 0..512 {
                if l2[i2].is_unused() {
                    continue;
                }
                let l1 = table_at(PhysFrame::containing_address(l2[i2].addr()));
                for i1 in 0..512 {
                    let entry = &l1[i1];
                    if entry.is_unused() {
                        continue;
                    }
                    let va = ((i4 as u64) << 39) | ((i3 as u64) << 30) | ((i2 as u64) << 21) | ((i1 as u64) << 12);
                    f(va, PhysFrame::containing_address(entry.addr()), entry.flags())?;
                }
            }
        }
    }
    Ok(())
}

/// Page-table bit 9 (free for the OS): this page was writable, and is
/// shared copy-on-write. It's mapped read-only; the first write copies it.
pub const COW: PageTableFlags = PageTableFlags::BIT_9;

/// The address space for a fork: the same pages, shared. Writable pages
/// become read-only + COW in both (the first write to one makes a private
/// copy, see break_cow); read-only pages are simply shared. Only the page
/// tables are new. The source must be the active address space (its TLB
/// is flushed).
pub fn copy_address_space(src: PhysFrame, frames: &mut BootInfoFrameAllocator) -> Result<PhysFrame, &'static str> {
    /// Mirror `table` (at `level`, 4 = the top) into a new table: lower
    /// tables are copied, pages are shared (writable ones become COW).
    fn copy_table(table: PhysFrame, level: u8, frames: &mut BootInfoFrameAllocator) -> Result<PhysFrame, &'static str> {
        let new = frames.allocate_frame().ok_or("out of memory")?;
        zero_frame(new);
        let (from, to) = (table_at(table), table_at(new));
        for i in 0..512 {
            let entry = &mut from[i];
            if entry.is_unused() {
                continue;
            }
            let child = PhysFrame::containing_address(entry.addr());
            if level > 1 {
                match copy_table(child, level - 1, frames) {
                    Ok(copy) => to[i].set_addr(copy.start_address(), entry.flags()),
                    Err(e) => {
                        // Free what this table got so far, then give up.
                        free_partial(new, level, frames);
                        return Err(e);
                    }
                }
            } else {
                let mut flags = entry.flags();
                if flags.contains(PageTableFlags::WRITABLE) {
                    flags = (flags - PageTableFlags::WRITABLE) | COW;
                    entry.set_flags(flags); // the parent's copy is read-only now too
                }
                to[i].set_addr(entry.addr(), flags);
                frames.share(child);
            }
        }
        Ok(new)
    }

    /// Undo a partly copied table (its pages were shared, its tables new).
    fn free_partial(table: PhysFrame, level: u8, frames: &mut BootInfoFrameAllocator) {
        for entry in table_at(table).iter() {
            if entry.is_unused() {
                continue;
            }
            let child = PhysFrame::containing_address(entry.addr());
            if level > 1 {
                free_partial(child, level - 1, frames);
            } else {
                frames.release(child);
            }
        }
        unsafe { frames.deallocate_frame(table) };
    }

    let dst = new_address_space(frames).ok_or("out of memory")?;
    let (from, to) = (table_at(src), table_at(dst));
    let mut result = Ok(());
    for i in 0..256 {
        if !is_user_slot(i) || from[i].is_unused() {
            continue;
        }
        match copy_table(PhysFrame::containing_address(from[i].addr()), 3, frames) {
            Ok(copy) => to[i].set_addr(copy.start_address(), from[i].flags()),
            Err(e) => {
                result = Err(e);
                break;
            }
        }
    }
    // The parent's pages just became read-only: drop stale TLB entries.
    x86_64::instructions::tlb::flush_all();
    match result {
        Ok(()) => Ok(dst),
        Err(e) => {
            free_address_space(dst, frames);
            Err(e)
        }
    }
}

/// The current address space's page-table entry for `addr`, if mapped.
fn current_entry(addr: u64) -> Option<&'static mut x86_64::structures::paging::page_table::PageTableEntry> {
    use x86_64::registers::control::Cr3;
    let va = VirtAddr::new(addr);
    let mut table = table_at(Cr3::read().0);
    for index in [va.p4_index(), va.p3_index(), va.p2_index()] {
        let entry = &table[index];
        if entry.is_unused() || entry.flags().contains(PageTableFlags::HUGE_PAGE) {
            return None;
        }
        table = table_at(PhysFrame::containing_address(entry.addr()));
    }
    let entry = &mut table[va.p1_index()];
    (!entry.is_unused()).then_some(entry)
}

/// A write to a copy-on-write page at `addr` (current address space):
/// give this process its own writable copy (or just make it writable if
/// nobody else uses the frame any more). Returns false if the page isn't
/// copy-on-write (a real protection fault) or memory ran out.
pub fn break_cow(addr: u64, frames: &mut BootInfoFrameAllocator) -> bool {
    let Some(entry) = current_entry(addr) else { return false };
    let flags = entry.flags();
    if !flags.contains(COW) {
        return false;
    }
    let old = PhysFrame::containing_address(entry.addr());
    let writable = (flags - COW) | PageTableFlags::WRITABLE;
    if frames.users(old) == 1 {
        entry.set_flags(writable); // the last user: keep the frame
    } else {
        let Some(copy) = frames.allocate_frame() else { return false };
        unsafe {
            core::ptr::copy_nonoverlapping(phys_ptr(old.start_address()) as *const u8, phys_ptr(copy.start_address()), 4096);
        }
        entry.set_addr(copy.start_address(), writable);
        frames.release(old);
    }
    x86_64::instructions::tlb::flush(VirtAddr::new(addr & !0xfff));
    true
}

/// break_cow with the global frame allocator. try_lock: this is also
/// called from the page fault handler, which must not wait for a lock the
/// interrupted code may hold.
pub fn break_cow_current(addr: u64) -> bool {
    match FRAME_ALLOCATOR.try_lock() {
        Some(mut guard) => guard.as_mut().is_some_and(|frames| break_cow(addr, frames)),
        None => false,
    }
}

/// How many user pages are mapped in an address space.
pub fn count_user_pages(p4: PhysFrame) -> u64 {
    let mut n = 0;
    let _ = for_each_user_page(p4, |_, _, _| {
        n += 1;
        Ok(())
    });
    n
}

/// Level-4 index of a virtual address.
pub fn p4_index(addr: u64) -> usize {
    usize::from(PageTableIndex::new(((addr >> 39) & 0x1ff) as u16))
}

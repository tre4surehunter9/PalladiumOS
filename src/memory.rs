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
    next: usize,
    /// Physical address of the first freed frame. Each free frame stores the
    /// address of the next one in its first 8 bytes (an intrusive linked list).
    free_list: Option<u64>,
    free_count: usize,
    total: usize,
}

impl BootInfoFrameAllocator {

    pub unsafe fn init(memory_map: &'static MemoryMap) -> Self {
        BootInfoFrameAllocator {
            memory_map,
            next: 0,
            free_list: None,
            free_count: 0,
            total: 0,
        }
    }

    /// Number of frames handed out and not yet freed.
    pub fn frames_in_use(&self) -> usize {
        self.next - self.free_count
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
        let frame = self.usable_frames().nth(self.next);
        if frame.is_some() {
            self.next += 1;
        }
        frame
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
            return false;
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
                unsafe { frames.deallocate_frame(child) };
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

/// Make a full copy of an address space (for fork): every user page is
/// duplicated into a new frame with the same permissions.
pub fn copy_address_space(src: PhysFrame, frames: &mut BootInfoFrameAllocator) -> Result<PhysFrame, &'static str> {
    let dst = new_address_space(frames).ok_or("out of memory")?;
    let mut mapper = mapper_for(dst);
    let keep = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE | PageTableFlags::NO_EXECUTE;

    let result = for_each_user_page(src, |va, frame, flags| {
        let copy = frames.allocate_frame().ok_or("out of memory")?;
        unsafe {
            core::ptr::copy_nonoverlapping(
                phys_ptr(frame.start_address()) as *const u8,
                phys_ptr(copy.start_address()),
                4096,
            );
            let page = Page::<Size4KiB>::containing_address(VirtAddr::new(va));
            match mapper.map_to(page, copy, flags & keep, frames) {
                // The new address space is not active, so no TLB flush is needed.
                Ok(flush) => flush.ignore(),
                Err(_) => {
                    frames.deallocate_frame(copy);
                    return Err("map_to failed");
                }
            }
        }
        Ok(())
    });

    match result {
        Ok(()) => Ok(dst),
        Err(e) => {
            free_address_space(dst, frames);
            Err(e)
        }
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

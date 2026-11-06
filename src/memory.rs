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
}

impl BootInfoFrameAllocator {

    pub unsafe fn init(memory_map: &'static MemoryMap) -> Self {
        BootInfoFrameAllocator {
            memory_map,
            next: 0,
            free_list: None,
            free_count: 0,
        }
    }

    /// Number of frames handed out and not yet freed.
    pub fn frames_in_use(&self) -> usize {
        self.next - self.free_count
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



pub fn map_user_vga(mapper: &mut impl Mapper<Size4KiB>) {
    let page = Page::<Size4KiB>::containing_address(VirtAddr::new(0xb8000));
    unsafe {
        mapper
        .update_flags(
            page,
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE,
        )
        .expect("update_flags failed")
        .flush();
    }
}

pub fn ensure_user_accessible(addr: VirtAddr, physical_memory_offset: VirtAddr) {
    use x86_64::registers::control::Cr3;
    use x86_64::structures::paging::PageTableFlags as Flags;

    let (level_4_frame, _) = Cr3::read();
    let table_indexes = [addr.p4_index(), addr.p3_index(), addr.p2_index()];
    let mut frame = level_4_frame;

    for &index in &table_indexes {
        let virt = physical_memory_offset + frame.start_address().as_u64();
        let table_ptr: *mut PageTable = virt.as_mut_ptr();
        let table = unsafe { &mut *table_ptr };
        let entry = &mut table[index];

        let flags = entry.flags() | Flags::USER_ACCESSIBLE;
        entry.set_flags(flags);

        frame = entry.frame().expect("expected a table frame, found huge page or missing entry");
    }
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

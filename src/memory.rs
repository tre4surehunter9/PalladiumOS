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

use x86_64::structures::paging::{Page, PhysFrame, Mapper, Size4KiB, FrameAllocator, PageTableFlags};

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
}

impl BootInfoFrameAllocator {

    pub unsafe fn init(memory_map: &'static MemoryMap) -> Self {
        BootInfoFrameAllocator {
            memory_map,
            next: 0,
        }
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

unsafe impl FrameAllocator<Size4KiB> for BootInfoFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        let frame = self.usable_frames().nth(self.next);
        self.next += 1;
        frame
    }
}



pub fn map_user_page(
    mapper: &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut impl x86_64::structures::paging::FrameAllocator<Size4KiB>,
    page: Page<Size4KiB>,
) {
    let frame = frame_allocator.allocate_frame().expect("no frames left");
    let flags = PageTableFlags::PRESENT
    | PageTableFlags::WRITABLE
    | PageTableFlags::USER_ACCESSIBLE;

    unsafe {
        mapper.map_to(page, frame, flags, frame_allocator)
        .expect("map_to failed")
        .flush();
    }
}

const USER_PAYLOAD: [u8; 2] = [0xeb, 0xfe];

pub fn load_payload(target_addr: VirtAddr) {
    unsafe {
        let dest = target_addr.as_mut_ptr::<u8>();
        core::ptr::copy_nonoverlapping(USER_PAYLOAD.as_ptr(), dest, USER_PAYLOAD.len());
    }
}


pub fn load_payload_bytes(target_addr: VirtAddr, bytes: &[u8]) {
    assert!(
        bytes.len() <= 4096,
            "payload does not fit in a single page — multi-page loading needed"
    );
    unsafe {
        let dest = target_addr.as_mut_ptr::<u8>();
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), dest, bytes.len());
    }
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

// Copyright (c) 2026 tre4surehunter9
//! Tests for the AHCI (SATA) driver. The runner boots this test kernel on
//! QEMU's q35 machine, where every disk is on an AHCI controller: the boot
//! disk on port 0, the test disk (target/test-disk.img) on port 1, the
//! system disk (target/usr.img) on port 2.
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(palladiumos::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;
use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;
use palladiumos::block::{self, BlockDevice};
use palladiumos::ext2::Ext2;
use palladiumos::mounts::{FileSystem, FileType};
use palladiumos::{ahci, memory};

entry_point!(main, config = &palladiumos::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut BootInfo) -> ! {
    let boot_info: &'static BootInfo = boot_info;
    use memory::BootInfoFrameAllocator;
    use palladiumos::allocator;
    use x86_64::VirtAddr;

    palladiumos::init();
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset.into_option().unwrap());
    let mut mapper = unsafe { memory::init(phys_mem_offset) };
    let mut frame_allocator = unsafe { BootInfoFrameAllocator::init(&boot_info.memory_regions) };
    allocator::init_heap(&mut mapper, &mut frame_allocator).expect("heap initialization failed");
    memory::PHYS_MEM_OFFSET.store(phys_mem_offset.as_u64(), core::sync::atomic::Ordering::Relaxed);
    *memory::MAPPER.lock() = Some(mapper);
    *memory::FRAME_ALLOCATOR.lock() = Some(frame_allocator);
    palladiumos::process::init();

    test_main();
    palladiumos::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    palladiumos::test_panic_handler(info)
}

#[test_case]
fn finds_the_disks() {
    let ports = ahci::ports_with_disks();
    assert!(ports.contains(&0) && ports.contains(&1) && ports.contains(&2), "ports with disks: {:?}", ports);
    let boot = ahci::disk(0).expect("boot disk on port 0");
    assert!(boot.sectors() > 0);
    assert!(ahci::disk(0).is_none(), "a port is handed out once");
}

#[test_case]
fn reads_writes_and_flushes() {
    // block::disk(1): no IDE on q35, so this is AHCI port 1.
    let mut disk = block::disk(1).expect("test disk on AHCI port 1");
    assert_eq!(disk.sectors(), 4 * 1024 * 1024 / 512);
    let mut sb = [0u8; 1024];
    disk.read(2, &mut sb).unwrap();
    assert_eq!(u16::from_le_bytes([sb[56], sb[57]]), 0xEF53, "ext2 magic");

    // 600 sectors at the end: more than one command's worth (256), and
    // restored afterwards.
    let n = 600;
    let start = disk.sectors() - n;
    let mut saved = vec![0u8; n as usize * 512];
    disk.read(start, &mut saved).unwrap();
    let pattern: Vec<u8> = (0..saved.len()).map(|i| (i * 7 + i / 512) as u8).collect();
    disk.write(start, &pattern).unwrap();
    disk.flush().unwrap();
    let mut back = vec![0u8; saved.len()];
    disk.read(start, &mut back).unwrap();
    assert!(back == pattern, "read back what was written");
    // An unaligned piece of it.
    let mut piece = vec![0u8; 3 * 512];
    disk.read(start + 255, &mut piece).unwrap();
    assert!(piece[..] == pattern[255 * 512..258 * 512]);
    disk.write(start, &saved).unwrap();
    disk.flush().unwrap();

    let mut one = [0u8; 512];
    assert!(disk.read(disk.sectors(), &mut one).is_err(), "past the end");
    disk.read(0, &mut one).unwrap(); // still works after that
}

#[test_case]
fn ext2_on_sata() {
    let usr = Ext2::mount(block::disk(2).expect("system disk on port 2")).unwrap();
    let _ = usr; // mounts: the superblock and group descriptors read fine
    let mut fs = Ext2::mount(block::disk(1).expect("test disk")).unwrap();
    let root = fs.root();
    let f = match fs.lookup(root, "sata-test.txt") {
        Ok(f) => f,
        Err(_) => fs.create(root, "sata-test.txt", FileType::File, 0o644).unwrap(),
    };
    fs.truncate(f, 0).unwrap();
    let data: Vec<u8> = (0..200_000).map(|i| (i % 251) as u8).collect();
    fs.write(f, 0, &data).unwrap();
    fs.sync().unwrap();
    let mut back = vec![0u8; data.len()];
    assert_eq!(fs.read(f, 0, &mut back).unwrap(), data.len());
    assert!(back == data);
}

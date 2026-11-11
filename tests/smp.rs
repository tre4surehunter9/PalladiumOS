// Copyright (c) 2026 tre4surehunter9
//! Tests for starting more CPU cores. The runner boots this test kernel
//! with four (`-smp 4`).
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(palladiumos::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;
use palladiumos::memory;

entry_point!(main, config = &palladiumos::BOOTLOADER_CONFIG);

static mut RSDP: Option<u64> = None;

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
    unsafe { RSDP = boot_info.rsdp_addr.into_option() };

    test_main();
    palladiumos::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    palladiumos::test_panic_handler(info)
}

#[test_case]
fn all_four_cores_start() {
    let rsdp = unsafe { RSDP };
    assert!(rsdp.is_some(), "the bootloader passes the ACPI root");
    assert_eq!(palladiumos::smp::init(rsdp), 4);
    assert_eq!(palladiumos::smp::cores(), 4);
}

#[test_case]
fn the_timer_still_interrupts_the_first_core() {
    // Its interrupts now come through its local APIC (virtual wire mode).
    let start = palladiumos::process::ticks();
    for _ in 0..100 {
        if palladiumos::process::ticks() >= start + 3 {
            return;
        }
        x86_64::instructions::hlt();
    }
    panic!("the timer stopped: {} ticks", palladiumos::process::ticks() - start);
}

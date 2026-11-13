// Copyright (c) 2026 tre4surehunter9
//! Tests for the USB keyboard and pointer driver. The runner boots this
//! test kernel with QEMU's xHCI controller, a USB keyboard, a tablet and a
//! mouse (-device qemu-xhci -device usb-kbd -device usb-tablet -device
//! usb-mouse).
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(palladiumos::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;
use palladiumos::{memory, xhci};

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
fn finds_the_usb_keyboard_and_pointers() {
    assert_eq!(xhci::init(), Some(3), "a keyboard, a tablet and a mouse on the xHCI controller");
    assert_eq!(xhci::keyboards(), 1);
    // The tablet (an absolute pointer, from its report descriptor) and the
    // mouse (boot protocol).
    assert_eq!(xhci::pointers(), 2);
    // Polling with no keys pressed: nothing typed, nothing breaks.
    palladiumos::task::keyboard::init_queue();
    for _ in 0..100 {
        xhci::poll();
    }
    assert_eq!(palladiumos::task::keyboard::pop_scancode(), None);
}

// Copyright (c) 2026 tre4surehunter9
#![no_std]

#![cfg_attr(test, no_main)]
#![feature(custom_test_frameworks)]
#![test_runner(crate::test_runner)]
#![reexport_test_harness_main = "test_main"]
#![feature(abi_x86_interrupt)]

use core::panic::PanicInfo;

pub trait Testable {
    fn run (&self) -> ();
}

impl<T> Testable for T
where
    T: Fn(),
{
    fn run(&self) {
        serial_print!("{}...\t", core::any::type_name::<T>());
        self();
        serial_println!("[ok]")
    }
}

pub fn test_runner(tests: &[&dyn Testable]) {
    serial_println!("Running {} tests", tests.len());
    for test in tests {
        test.run();
    }
    exit_qemu(QemuExitCode::Success)

}

pub fn test_panic_handler(info: &PanicInfo) -> ! {
    serial_println!("[failed]\n");
    serial_println!("Error {}\n", info);
    exit_qemu(QemuExitCode::Failed);
    hlt_loop();
}

/// How the bootloader (0.11) should set things up, for the kernel and every
/// test kernel: all physical memory mapped at PHYS_OFFSET, and its own
/// mappings (framebuffer, boot information, the boot stack, anything else)
/// in the upper half. The lower half belongs to programs: the kernel clears
/// it.
pub const BOOTLOADER_CONFIG: bootloader_api::BootloaderConfig = {
    use bootloader_api::config::Mapping;
    let mut c = bootloader_api::BootloaderConfig::new_default();
    c.mappings.physical_memory = Some(Mapping::FixedAddress(memory::PHYS_OFFSET));
    c.mappings.kernel_stack = Mapping::FixedAddress(0xFFFF_FE80_0000_0000);
    c.mappings.boot_info = Mapping::FixedAddress(0xFFFF_FF00_0000_0000);
    c.mappings.framebuffer = Mapping::FixedAddress(0xFFFF_FD00_0000_0000);
    c.mappings.dynamic_range_start = Some(0xFFFF_A000_0000_0000);
    c.mappings.dynamic_range_end = Some(0xFFFF_BFFF_FFFF_F000);
    c.kernel_stack_size = 512 * 1024;
    // No minimum screen size: under UEFI the bootloader then keeps the
    // firmware's mode (QEMU: the EDID resolution the runner asks for, 720p;
    // real PCs: usually the screen's own). With a minimum it would switch
    // to the biggest mode there is. (Its BIOS stage picks up to 1280x720.)
    c
};

#[cfg(test)]
use bootloader_api::{entry_point, BootInfo};

#[cfg(test)]
entry_point!(test_kernel_main, config = &BOOTLOADER_CONFIG);

#[cfg(test)]
fn test_kernel_main(_boot_info: &'static mut BootInfo) -> ! {
    init();
    test_main();
    hlt_loop();
}


#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    test_panic_handler(info)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum QemuExitCode {
    Success = 0x10,
    Failed = 0x11,
}

pub fn exit_qemu(exit_code: QemuExitCode) {
    use x86_64::instructions::port::Port;

    unsafe {
        let mut port = Port::new(0xf4);
        port.write(exit_code as u32);
    }
}


#[test_case]
fn test_breakpoint_exception() {
    // invoke a breakpoint exception
    x86_64::instructions::interrupts::int3();
}

pub fn init() {
    gdt::init();
    boot_ok!("Loaded the GDT and TSS (kernel/user segments, interrupt stacks)");
    interrupts::init_idt();
    boot_ok!("Set up the IDT (exceptions, timer, keyboard, int 0x80 syscalls)");
    fpu::init();
    boot_ok!("Enabled SSE and FPU state for programs");
    // Write-protect: the kernel can't write read-only pages either, so a
    // copy-on-write page can never be changed behind a program's back.
    unsafe {
        use x86_64::registers::control::{Cr0, Cr0Flags};
        Cr0::update(|f| f.insert(Cr0Flags::WRITE_PROTECT));
    }
    syscall::init_fast_syscalls();
    boot_ok!("Enabled the syscall instruction (Linux x86_64 ABI)");
    interrupts::disable_local_apic();
    interrupts::start_timer();
    unsafe {
        let mut pics = interrupts::PICS.lock();
        pics.initialize();
        // Exactly the timer (0), the keyboard (1) and the cascade to the
        // second PIC (2); the network card's line is let through when its
        // driver starts. (initialize() keeps the firmware's masks: a BIOS
        // leaves the timer and keyboard on, UEFI firmware masks them.)
        pics.write_masks(0b1111_1000, 0b1111_1111);
    }
    x86_64::instructions::interrupts::enable();
    boot_ok!("Started interrupts (PIC remapped, timer at 100 Hz)");
}

pub fn hlt_loop() -> ! {
    loop {
        x86_64::instructions::hlt();
    }
}

/// The PalladiumOS version (from Cargo.toml).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod serial;
pub mod vga_buffer;
pub mod virtio;
pub mod virtio_gpu;
pub mod virtio_blk;
pub mod fw_cfg;
pub mod virtio_rng;
pub mod virtio_input;
pub mod virtio_console;
pub mod interrupts;
pub mod gdt;
pub mod memory;
pub mod allocator;
pub mod task;
pub mod shell;
pub mod fdfs;
pub mod filesystem;
pub mod editor;
pub mod usermode;
pub mod syscall;
pub mod elf;
pub mod tty;
pub mod programs;
pub mod vfs;
pub mod process;
pub mod pipe;
pub mod pty;
pub mod block;
pub mod ahci;
pub mod xhci;
pub mod inotify;
pub mod mouse;
pub mod mounts;
pub mod ext2;
pub mod devfs;
pub mod procfs;
pub mod perm;
pub mod signal;
pub mod fpu;
pub mod mmap;
pub mod shm;
pub mod random;
pub mod boot;
pub mod net;
pub mod rtc;
pub mod smp;
pub mod sound;


extern crate alloc;





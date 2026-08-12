// Copyright (c) 2026 tre4surehunter9
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(crate::test_runner)]
#![reexport_test_harness_main = "test_main"]

use core::panic::PanicInfo;

#[cfg(test)]
pub fn test_runner(tests: &[&dyn Testable]) {
    serial_println!("Running {} tests", tests.len());
    for test in tests {
        test.run();
    }
    exit_qemu(QemuExitCode::Success);
}

#[test_case]
fn trivial_assertion() {
    assert_eq!(1, 1);
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
        port.write(exit_code as u32)
    }
}

pub trait Testable {
    fn run(&self) -> ();
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

mod vga_buffer;
mod serial;
mod memory;

use bootloader::{BootInfo, entry_point};
use palladiumos::hlt_loop;
use palladiumos::task::{Task, simple_executor::SimpleExecutor};

entry_point!(kernel_main);
extern crate alloc;
use alloc::{boxed::Box, vec, vec::Vec, rc::Rc};

// start function
fn kernel_main(boot_info: &'static BootInfo) -> ! {
    use palladiumos::allocator;
    use palladiumos::memory;
    use palladiumos::memory::translate_addr;
    use palladiumos::memory::BootInfoFrameAllocator;
    use palladiumos::task::executor::Executor;
    use palladiumos::task::keyboard;
    use palladiumos::task::keyboard::run_shell;
    use palladiumos::usermode::enter_usermode;
    use x86_64::structures::paging::Page;
    use x86_64::{structures::paging::Translate, VirtAddr};

    // welcome message
    palladiumos::shell::print_welcome();
    palladiumos::init();

    let mut frame_allocator = unsafe { BootInfoFrameAllocator::init(&boot_info.memory_map) };
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    let mut mapper = unsafe { memory::init(phys_mem_offset) };

    allocator::init_heap(&mut mapper, &mut frame_allocator).expect("heap initialization failed");

    static HELLO_PROGRAM: &[u8] = include_bytes!("user_progs/hello.bin");

    palladiumos::filesystem::init_default_files();

    // --- ring 3 test: this block must run BEFORE the executor, since
    // executor.run() and enter_usermode() both never return. Whatever is
    // placed after either of them is dead code. ---

    let user_page = Page::containing_address(VirtAddr::new(0x_6666_6666_0000));
    memory::map_user_page(&mut mapper, &mut frame_allocator, user_page);
    memory::load_payload_bytes(user_page.start_address(), HELLO_PROGRAM);

    memory::ensure_user_accessible(VirtAddr::new(0xb8000), phys_mem_offset);
    memory::map_user_vga(&mut mapper);

    let user_stack_page = Page::containing_address(VirtAddr::new(0x_5555_5555_0000));
    memory::map_user_page(&mut mapper, &mut frame_allocator, user_stack_page);

    enter_usermode(
        user_page.start_address(),
                   user_stack_page.start_address() + 4096u64, // stack grows down, start at top
    );

    // everything below here is currently unreachable, since enter_usermode
    // never returns (-> !). Left in place for when the syscall/return path
    // exists and this becomes reachable again.
    #[allow(unreachable_code)]
    {
        let mut executor = Executor::new();
        executor.spawn(Task::new(run_shell()));
        executor.run();
        palladiumos::hlt_loop();
    }
}

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("{}", info);
    palladiumos::hlt_loop();
}

#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    palladiumos::test_panic_handler(info)
}

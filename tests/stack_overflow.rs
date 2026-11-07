#![feature(abi_x86_interrupt)]
#![no_std]
#![no_main]

use core::panic::PanicInfo;

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    palladiumos::test_panic_handler(info)
}

use palladiumos::serial_print;

bootloader_api::entry_point!(kernel_main, config = &palladiumos::BOOTLOADER_CONFIG);

fn kernel_main(_boot_info: &'static mut bootloader_api::BootInfo) -> ! {
    serial_print!("stack_overflow::stack_overflow...\t");

    palladiumos::gdt::init();
    init_test_idt();

    stack_overflow();

    panic!("Exception continued after stack overflow");
}

#[allow(unconditional_recursion)]
fn stack_overflow() {
    stack_overflow();
    unsafe { core::ptr::read_volatile(&0u8) }; // (keeps the recursion from becoming a loop)
}

use lazy_static::lazy_static;

lazy_static! {
    static ref TEST_IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();
        unsafe {
            idt.double_fault
            .set_handler_fn(test_double_fault_handler)
            .set_stack_index(palladiumos::gdt::DOUBLE_FAULT_IST_INDEX);
        }

        idt
    };
}

pub fn init_test_idt() {
    TEST_IDT.load();
}

use palladiumos::{exit_qemu, QemuExitCode, serial_println};
use x86_64::structures::idt::InterruptStackFrame;
use x86_64::structures::gdt::SegmentSelector;
use x86_64::structures::idt::InterruptDescriptorTable;


extern "x86-interrupt" fn test_double_fault_handler(
    _stack_frame: InterruptStackFrame,
    _error_code: u64,
) -> ! {
    serial_println!("[ok]");
    exit_qemu(QemuExitCode::Success);
    loop {}
}


#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(palladiumos::test_runner)]
#![reexport_test_harness_main= "test_main"]

use core::panic::PanicInfo;

bootloader_api::entry_point!(kernel_main, config = &palladiumos::BOOTLOADER_CONFIG);

fn kernel_main(_boot_info: &'static mut bootloader_api::BootInfo) -> ! {
    test_main();

    loop {}
}

use palladiumos::exit_qemu;
use palladiumos::Testable;
use palladiumos::QemuExitCode;
use palladiumos::serial_println;



#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    palladiumos::test_panic_handler(info)
}

use palladiumos::println;

#[test_case]
fn test_println() {
    println!("test_println output");
}

 #![no_std]
 #![no_main]


 use core::panic::PanicInfo;
 use palladiumos::{QemuExitCode, exit_qemu, serial_println, serial_print};

bootloader_api::entry_point!(kernel_main, config = &palladiumos::BOOTLOADER_CONFIG);

fn kernel_main(_boot_info: &'static mut bootloader_api::BootInfo) -> ! {
     should_fail();
     serial_println!("[test did not panic]");
     exit_qemu(QemuExitCode::Failed);
     loop {}
 }

 fn should_fail() {
     serial_print!("should_panic::should_fail...\t");
     assert_eq!(0, 1);
 }

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    serial_println!("[ok]");
    exit_qemu(QemuExitCode::Success);
    loop {}
}



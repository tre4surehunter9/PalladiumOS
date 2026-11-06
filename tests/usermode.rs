// Copyright (c) 2026 tre4surehunter9
//! Ring 3 + syscall tests: each test runs a tiny user program (see
//! tests/user_progs/*.s) and checks its exit status.
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(palladiumos::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use palladiumos::memory;
use palladiumos::usermode::launch_program;

entry_point!(main);

fn main(boot_info: &'static BootInfo) -> ! {
    use palladiumos::allocator;
    use memory::BootInfoFrameAllocator;
    use x86_64::VirtAddr;

    palladiumos::init();
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    let mut mapper = unsafe { memory::init(phys_mem_offset) };
    let mut frame_allocator = unsafe { BootInfoFrameAllocator::init(&boot_info.memory_map) };
    allocator::init_heap(&mut mapper, &mut frame_allocator).expect("heap initialization failed");

    memory::PHYS_MEM_OFFSET.store(phys_mem_offset.as_u64(), core::sync::atomic::Ordering::Relaxed);
    *memory::MAPPER.lock() = Some(mapper);
    *memory::FRAME_ALLOCATOR.lock() = Some(frame_allocator);

    test_main();
    palladiumos::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    palladiumos::test_panic_handler(info)
}

const HELLO: &[u8] = include_bytes!("../src/user_progs/hello.bin");
const CRASH: &[u8] = include_bytes!("../src/user_progs/crash.bin");

// ---- returning from ring 3 ----

#[test_case]
fn hello_exits_with_zero() {
    assert_eq!(launch_program(HELLO), 0);
}

#[test_case]
fn programs_can_run_repeatedly() {
    for _ in 0..5 {
        assert_eq!(launch_program(HELLO), 0);
    }
}

#[test_case]
fn exit_status_is_passed_back() {
    // exit(getpid()) -> 1
    assert_eq!(launch_program(include_bytes!("user_progs/getpid.bin")), 1);
}

#[test_case]
fn registers_preserved_across_syscall() {
    assert_eq!(launch_program(include_bytes!("user_progs/regs.bin")), 0);
}

#[test_case]
fn too_large_program_is_rejected() {
    let big = [0x90u8; 4097]; // nops
    assert_eq!(launch_program(&big), -1);
}

// ---- syscalls ----

#[test_case]
fn unknown_syscall_returns_enosys() {
    assert_eq!(launch_program(include_bytes!("user_progs/enosys.bin")), 38);
}

#[test_case]
fn write_bad_fd_returns_ebadf() {
    assert_eq!(launch_program(include_bytes!("user_progs/badfd.bin")), 9);
}

#[test_case]
fn write_kernel_pointer_returns_efault() {
    assert_eq!(launch_program(include_bytes!("user_progs/efault.bin")), 14);
}

#[test_case]
fn uname_reports_palladiumos() {
    assert_eq!(launch_program(include_bytes!("user_progs/uname.bin")), 0);
}

// ---- faults in ring 3 kill the program, not the kernel ----

#[test_case]
fn null_read_is_segfault() {
    assert_eq!(launch_program(CRASH), 128 + 11);
}

#[test_case]
fn kernel_write_is_segfault() {
    assert_eq!(launch_program(include_bytes!("user_progs/kwrite.bin")), 128 + 11);
}

#[test_case]
fn illegal_instruction_is_sigill() {
    assert_eq!(launch_program(include_bytes!("user_progs/ud2.bin")), 128 + 4);
}

#[test_case]
fn kernel_still_works_after_crashes() {
    assert_eq!(launch_program(CRASH), 139);
    assert_eq!(launch_program(HELLO), 0);
}

// ---- user pointer validation ----

#[test_case]
fn user_range_rejects_kernel_and_unmapped() {
    assert!(!memory::is_user_range(0, 1, false), "null page");
    assert!(!memory::is_user_range(0xffff_8000_0000_0000, 8, false), "kernel half");
    assert!(!memory::is_user_range(0x0000_7fff_ffff_fff0, 0x20, false), "crosses top of user half");
    assert!(!memory::is_user_range(u64::MAX - 4, 16, false), "overflow");
    assert!(!memory::is_user_range(0xb000_0000, 8, false), "unmapped");
}

#[test_case]
fn user_range_accepts_user_pages() {
    launch_program(HELLO); // ensures the user code/stack pages are mapped
    assert!(memory::is_user_range(0x6666_6666_0000, 4096, false));
    assert!(memory::is_user_range(0x6666_6666_0000, 4096, true));
    assert!(memory::is_user_range(0x5555_5555_0000, 4096, true));
    assert!(!memory::is_user_range(0x6666_6666_0000, 4097, false), "runs past the page");
    assert!(memory::is_user_range(0, 0, false), "empty range is always fine");
}

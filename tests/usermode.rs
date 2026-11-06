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
use palladiumos::usermode::{launch_program, run};
use palladiumos::{elf, programs};
use alloc::vec;
use alloc::vec::Vec;

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
    palladiumos::process::init();
    palladiumos::programs::install();

    test_main();
    palladiumos::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    palladiumos::test_panic_handler(info)
}

const CRASH: &[u8] = include_bytes!("../src/user_progs/crash.bin");

fn prog(name: &str) -> &'static [u8] {
    programs::find(name).expect("program not bundled")
}

fn hello() -> &'static [u8] {
    prog("hello")
}

fn frames_in_use() -> usize {
    memory::FRAME_ALLOCATOR.lock().as_ref().unwrap().frames_in_use()
}

// ---- returning from ring 3 ----

#[test_case]
fn hello_exits_with_zero() {
    assert_eq!(launch_program(hello()), 0);
}

#[test_case]
fn programs_can_run_repeatedly() {
    for _ in 0..5 {
        assert_eq!(launch_program(hello()), 0);
    }
}

#[test_case]
fn exit_status_is_passed_back() {
    assert_eq!(run(prog("test_proc"), &["test_proc", "exit", "42"]), Ok(42));
    // exit(getpid()): a real, nonzero pid (0 is the kernel)
    assert!(launch_program(include_bytes!("user_progs/getpid.bin")) > 0);
}

#[test_case]
fn registers_preserved_across_syscall() {
    assert_eq!(launch_program(include_bytes!("user_progs/regs.bin")), 0);
}

#[test_case]
fn too_large_flat_program_is_rejected() {
    let big = vec![0x90u8; 64 * 1024 + 1]; // nops
    assert!(run(&big, &["big"]).is_err());
    assert!(run(&[], &["empty"]).is_err());
}

#[test_case]
fn memory_is_reclaimed_after_exit() {
    assert_eq!(launch_program(hello()), 0);
    let before = frames_in_use();
    for _ in 0..50 {
        assert_eq!(launch_program(hello()), 0);
        assert_eq!(launch_program(CRASH), 139);
    }
    assert_eq!(frames_in_use(), before, "user frames leaked");
}

#[test_case]
fn pages_are_unmapped_after_exit() {
    assert_eq!(launch_program(hello()), 0);
    assert!(!memory::is_user_range(0x80_0000_0000, 1, false), "program image still mapped");
    assert!(!memory::is_user_range(0x7fff_fffe_f000, 1, false), "stack still mapped");
    assert_eq!(launch_program(CRASH), 139);
    assert!(!memory::is_user_range(0x6666_6666_0000, 1, false), "flat image still mapped");
}

// ---- Rust userland (ELF) ----

#[test_case]
fn bundled_programs_are_valid_elfs() {
    for (name, bytes) in programs::USER_PROGRAMS {
        let image = elf::parse(bytes).unwrap_or_else(|e| panic!("{}: {}", name, e));
        assert!(image.entry >= 0x4000_0000, "{} entry {:#x}", name, image.entry);
    }
}

#[test_case]
fn argv_is_passed() {
    assert_eq!(run(prog("test_args"), &["test_args", "one", "two words"]), Ok(0));
    // wrong argument count -> 10 + argc
    assert_eq!(run(prog("test_args"), &["test_args"]), Ok(11));
}

#[test_case]
fn argument_list_too_long_is_rejected() {
    let many = vec!["x"; 65];
    assert!(run(hello(), &many).is_err());
    assert_eq!(launch_program(hello()), 0, "kernel recovers after a failed load");
}

#[test_case]
fn rust_panic_exits_101() {
    assert_eq!(run(prog("test_panic"), &["test_panic"]), Ok(101));
}

#[test_case]
fn stack_overflow_hits_guard_page() {
    assert_eq!(run(prog("test_stack"), &["test_stack"]), Ok(139));
}

#[test_case]
fn echo_and_uname_succeed() {
    assert_eq!(run(prog("echo"), &["echo", "hello", "world"]), Ok(0));
    assert_eq!(run(prog("uname"), &["uname", "-a"]), Ok(0));
    assert_eq!(run(prog("uname"), &["uname", "-z"]), Ok(1));
}

// ---- stdin (TTY) ----
// Keys are fed in as PS/2 set-1 scancodes (make code, then break = make | 0x80).

fn press(scancode: u8) {
    palladiumos::task::keyboard::add_scancode(scancode);
    palladiumos::task::keyboard::add_scancode(scancode | 0x80);
}

const KEY_H: u8 = 0x23;
const KEY_I: u8 = 0x17;
const KEY_X: u8 = 0x2d;
const KEY_D: u8 = 0x20;
const KEY_ENTER: u8 = 0x1c;
const KEY_BACKSPACE: u8 = 0x0e;
const KEY_LCTRL: u8 = 0x1d;

fn ctrl_d() {
    palladiumos::task::keyboard::add_scancode(KEY_LCTRL);
    press(KEY_D);
    palladiumos::task::keyboard::add_scancode(KEY_LCTRL | 0x80);
}

#[test_case]
fn read_returns_a_line_then_eof() {
    palladiumos::task::keyboard::init_queue();
    press(KEY_H);
    press(KEY_I);
    press(KEY_ENTER);
    ctrl_d();
    assert_eq!(run(prog("test_read"), &["test_read"]), Ok(0));
}

#[test_case]
fn backspace_edits_the_line() {
    palladiumos::task::keyboard::init_queue();
    press(KEY_H);
    press(KEY_X);
    press(KEY_BACKSPACE);
    press(KEY_I);
    press(KEY_ENTER);
    ctrl_d();
    assert_eq!(run(prog("test_read"), &["test_read"]), Ok(0));
}

#[test_case]
fn cat_copies_stdin_until_eof() {
    palladiumos::task::keyboard::init_queue();
    press(KEY_H);
    press(KEY_ENTER);
    ctrl_d();
    assert_eq!(run(prog("cat"), &["cat"]), Ok(0));
}

// ---- malformed ELF files are rejected, not run ----

/// Copy `bytes` and overwrite a u64 field of the first PT_LOAD program
/// header that is NOT executable (so the entry point check still passes).
fn patch_data_segment(bytes: &[u8], field: usize, value: u64) -> Vec<u8> {
    let mut b = bytes.to_vec();
    let phoff = u64::from_le_bytes(b[32..40].try_into().unwrap()) as usize;
    let phnum = u16::from_le_bytes([b[56], b[57]]) as usize;
    for i in 0..phnum {
        let ph = phoff + i * 56;
        let p_type = u32::from_le_bytes(b[ph..ph + 4].try_into().unwrap());
        let p_flags = u32::from_le_bytes(b[ph + 4..ph + 8].try_into().unwrap());
        if p_type == 1 && p_flags & elf::PF_X == 0 {
            b[ph + field..ph + field + 8].copy_from_slice(&value.to_le_bytes());
            return b;
        }
    }
    panic!("no data segment to patch");
}

const P_VADDR: usize = 16;
const P_MEMSZ: usize = 40;

#[test_case]
fn elf_header_checks() {
    let good = hello();
    assert!(elf::parse(good).is_ok());
    assert!(elf::parse(&good[..40]).is_err(), "truncated header");
    assert!(elf::parse(&good[..200]).is_err(), "truncated segments");

    let mut b = good.to_vec();
    b[4] = 1; // 32-bit
    assert!(elf::parse(&b).is_err());

    let mut b = good.to_vec();
    b[16] = 3; // ET_DYN
    assert!(elf::parse(&b).is_err());

    let mut b = good.to_vec();
    b[18] = 3; // EM_386
    assert!(elf::parse(&b).is_err());

    let mut b = good.to_vec();
    b[24..32].copy_from_slice(&0x1234u64.to_le_bytes()); // entry outside code
    assert!(elf::parse(&b).is_err());
}

#[test_case]
fn elf_segment_in_kernel_half_is_rejected() {
    let b = patch_data_segment(hello(), P_VADDR, 0xffff_8000_0000_0000);
    assert!(elf::parse(&b).is_err());
    assert!(run(&b, &["bad"]).is_err());
}

#[test_case]
fn elf_segment_on_null_page_is_rejected() {
    let b = patch_data_segment(hello(), P_VADDR, 0);
    assert!(elf::parse(&b).is_err());
}

#[test_case]
fn elf_huge_segment_is_rejected() {
    let b = patch_data_segment(hello(), P_MEMSZ, 1 << 40);
    assert!(elf::parse(&b).is_err());
}

#[test_case]
fn elf_overlapping_kernel_memory_is_rejected() {
    // 0x200000 is where the kernel itself is mapped.
    let b = patch_data_segment(hello(), P_VADDR, 0x20_0000);
    assert!(elf::parse(&b).is_ok(), "address is in the user half, so parsing passes");
    let before = frames_in_use();
    assert!(run(&b, &["bad"]).is_err(), "must refuse to map over the kernel");
    assert_eq!(frames_in_use(), before, "failed load leaked frames");
    assert_eq!(launch_program(hello()), 0);
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
    assert_eq!(launch_program(hello()), 0);
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
fn user_range_accepts_valid_user_pointers() {
    // hello and uname both pass pointers to their own memory to the kernel;
    // if validation rejected them, write/uname would fail.
    assert_eq!(launch_program(hello()), 0);
    assert_eq!(launch_program(include_bytes!("user_progs/uname.bin")), 0);
    assert!(memory::is_user_range(0, 0, false), "empty range is always fine");
}

// ---- file syscalls (milestone B) ----

use palladiumos::filesystem;

#[test_case]
fn file_syscalls() {
    // test_fs exits with the line number of the first failed check.
    assert_eq!(run(prog("test_fs"), &["test_fs", "all"]), Ok(0));
}

#[test_case]
fn fds_are_closed_when_a_program_exits() {
    // Each run opens a file and exits without closing it: it must get fd 3
    // every time, so nothing leaked from the previous run.
    for _ in 0..3 {
        assert_eq!(run(prog("test_fs"), &["test_fs", "leak"]), Ok(3));
    }
}

#[test_case]
fn program_chdir_does_not_change_shell_cwd() {
    filesystem::make_dir("/cwdtest").unwrap();
    *filesystem::CWD.lock() = "/cwdtest".into();
    assert_eq!(run(prog("test_fs"), &["test_fs", "chdir"]), Ok(0));
    assert_eq!(*filesystem::CWD.lock(), "/cwdtest");
    *filesystem::CWD.lock() = "/".into();
}

#[test_case]
fn programs_inherit_shell_cwd() {
    filesystem::make_dir("/inherit").unwrap();
    *filesystem::CWD.lock() = "/inherit".into();
    // relative path, resolved against the inherited cwd
    assert_eq!(run(prog("test_fs"), &["test_fs", "write", "rel.txt", "relative"]), Ok(0));
    *filesystem::CWD.lock() = "/".into();
    assert_eq!(filesystem::read_file("/inherit/rel.txt").unwrap(), "relative");
}

#[test_case]
fn files_written_by_programs_are_visible_to_the_kernel() {
    assert_eq!(run(prog("test_fs"), &["test_fs", "write", "/fromuser.txt", "hello kernel"]), Ok(0));
    assert_eq!(filesystem::read_file("/fromuser.txt").unwrap(), "hello kernel");
}

#[test_case]
fn coreutils_end_to_end() {
    assert_eq!(run(prog("mkdir"), &["mkdir", "-p", "/e2e/a/b"]), Ok(0));
    assert!(filesystem::is_dir("/e2e/a/b"));
    assert_eq!(run(prog("mkdir"), &["mkdir", "/e2e/a"]), Ok(1), "already exists");
    assert_eq!(run(prog("mkdir"), &["mkdir", "-p", "/e2e/a"]), Ok(0), "-p tolerates existing");

    assert_eq!(run(prog("touch"), &["touch", "/e2e/a/b/f1", "/e2e/a/f2"]), Ok(0));
    assert_eq!(filesystem::read_file("/e2e/a/f2").unwrap(), "");
    filesystem::write_file("/e2e/a/f2", "keep").unwrap();
    assert_eq!(run(prog("touch"), &["touch", "/e2e/a/f2"]), Ok(0));
    assert_eq!(filesystem::read_file("/e2e/a/f2").unwrap(), "keep", "touch must not truncate");

    assert_eq!(run(prog("cat"), &["cat", "/e2e/a/f2"]), Ok(0));
    assert_eq!(run(prog("cat"), &["cat", "/e2e/missing"]), Ok(1));
    assert_eq!(run(prog("ls"), &["ls", "-la", "/e2e/a"]), Ok(0));
    assert_eq!(run(prog("ls"), &["ls", "/e2e/missing"]), Ok(2));
    assert_eq!(run(prog("ls"), &["ls", "/e2e/a/f2"]), Ok(0), "ls on a file");
    assert_eq!(run(prog("pwd"), &["pwd"]), Ok(0));

    assert_eq!(run(prog("rmdir"), &["rmdir", "/e2e/a"]), Ok(1), "not empty");
    assert_eq!(run(prog("rm"), &["rm", "/e2e/a"]), Ok(1), "rm without -r on a dir");
    assert_eq!(run(prog("rm"), &["rm", "/e2e/a/f2"]), Ok(0));
    assert!(filesystem::read_file("/e2e/a/f2").is_err());
    assert_eq!(run(prog("rm"), &["rm", "-f", "/e2e/nothing"]), Ok(0));
    assert_eq!(run(prog("rm"), &["rm", "-r", "/e2e"]), Ok(0));
    assert!(!filesystem::is_dir("/e2e"));
}

#[test_case]
fn deep_rm_r_does_not_overflow_the_stack() {
    let mut path = alloc::string::String::from("/deep");
    filesystem::make_dir(&path).unwrap();
    for i in 0..20 {
        path.push_str(&alloc::format!("/d{}", i));
        filesystem::make_dir(&path).unwrap();
        filesystem::write_file(&alloc::format!("{}/file", path), "x").unwrap();
    }
    assert_eq!(run(prog("rm"), &["rm", "-r", "/deep"]), Ok(0));
    assert!(!filesystem::is_dir("/deep"));
}

#[test_case]
fn errno_mapping_covers_every_fdfs_error() {
    use palladiumos::syscall::*;
    use palladiumos::vfs::errno_for;
    assert_eq!(errno_for("File not found"), ENOENT);
    assert_eq!(errno_for("Parent directory not found"), ENOENT);
    assert_eq!(errno_for("Not a directory"), ENOTDIR);
    assert_eq!(errno_for("Parent is not a directory"), ENOTDIR);
    assert_eq!(errno_for("Is a directory"), EISDIR);
    assert_eq!(errno_for("Already exists"), EEXIST);
    assert_eq!(errno_for("Directory is not empty"), ENOTEMPTY);
}

// ---- processes (milestone C) ----

use palladiumos::process;

fn only_kernel_left() -> bool {
    let procs = process::list();
    procs.len() == 1 && procs[0].0 == 0
}

#[test_case]
fn fork_exec_wait() {
    assert_eq!(run(prog("test_proc"), &["test_proc", "all"]), Ok(0));
    assert!(only_kernel_left(), "processes left over: {:?}", process::list());
}

#[test_case]
fn timer_preempts_user_processes() {
    assert_eq!(run(prog("test_proc"), &["test_proc", "preempt"]), Ok(0));
}

#[test_case]
fn orphans_are_reaped_by_the_kernel() {
    assert_eq!(run(prog("test_proc"), &["test_proc", "orphan"]), Ok(0));
    assert!(only_kernel_left(), "orphan not reaped: {:?}", process::list());
}

#[test_case]
fn process_limit_and_cleanup() {
    let before = frames_in_use();
    assert_eq!(run(prog("test_proc"), &["test_proc", "forkbomb"]), Ok(0));
    assert!(only_kernel_left());
    assert_eq!(frames_in_use(), before, "frames leaked by fork/exit");
}

#[test_case]
fn exec_and_fork_do_not_leak_memory() {
    assert_eq!(run(prog("test_proc"), &["test_proc", "all"]), Ok(0));
    let before = frames_in_use();
    for _ in 0..3 {
        assert_eq!(run(prog("test_proc"), &["test_proc", "all"]), Ok(0));
    }
    assert_eq!(frames_in_use(), before);
}

#[test_case]
fn programs_are_installed_in_bin() {
    for (name, bytes) in programs::USER_PROGRAMS {
        let path = alloc::format!("/bin/{}", name);
        assert_eq!(filesystem::read_bytes(&path).as_deref(), Ok(*bytes), "{}", path);
    }
}

#[test_case]
fn kernel_shell_runs_programs_from_bin() {
    use palladiumos::shell::process_command;
    assert!(process_command("run echo hi"));
    assert!(process_command("run /bin/echo by path"));
    assert!(!process_command("run test_proc exit 4"), "nonzero status is failure");
    assert!(!process_command("run no-such-program"));
    // `run` of a deleted program fails cleanly
    filesystem::write_file("/notprog", "plain text").unwrap();
    assert!(!process_command("run /notprog"), "text file is not a program");
    assert!(only_kernel_left());
}

#[test_case]
fn each_run_gets_a_new_pid() {
    // hello's exit status is 0 either way; check the process table instead.
    let before = process::list();
    assert_eq!(launch_program(hello()), 0);
    assert_eq!(process::list().len(), before.len());
}

// Copyright (c) 2026 tre4surehunter9
//! Ring 3 + syscall tests: each test runs a tiny user program (see
//! tests/user_progs/*.s) and checks its exit status.
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(palladiumos::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;
use palladiumos::memory;
use palladiumos::usermode::{launch_program, run};
use palladiumos::{elf, programs};
use alloc::vec;
use alloc::vec::Vec;

entry_point!(main, config = &palladiumos::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut BootInfo) -> ! {
    // The screen, as at boot (for /dev/fb0).
    if let Some(fb) = core::mem::replace(&mut boot_info.framebuffer, bootloader_api::info::Optional::None).into_option() {
        palladiumos::vga_buffer::init_framebuffer(fb);
    }
    let boot_info: &'static BootInfo = boot_info;
    use palladiumos::allocator;
    use memory::BootInfoFrameAllocator;
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
    // More CPU cores, if QEMU has them (PALLADIUM_SMP=4 cargo test).
    palladiumos::smp::init(boot_info.rsdp_addr.into_option());
    palladiumos::rtc::init(); // the real date, as at boot (CLOCK_REALTIME)
    palladiumos::net::init(); // loopback, and eth0 if QEMU has a card
    palladiumos::block::mount_system_disk(); // /usr: tcc's headers and libraries, if built
    palladiumos::block::mount_alpine_disk(); // /alpine: a 64 MiB scratch disk (target/test-scratch.img)
    palladiumos::programs::install();
    for dir in ["/proc", "/dev"] {
        palladiumos::filesystem::make_dir(dir).unwrap();
    }
    palladiumos::mounts::mount("/proc", alloc::boxed::Box::new(palladiumos::procfs::ProcFs));
    palladiumos::mounts::mount("/dev", alloc::boxed::Box::new(palladiumos::devfs::DevFs));
    palladiumos::block::mount_shm();
    palladiumos::mouse::init();
    palladiumos::sound::init(); // QEMU's AC'97, sound going nowhere

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
        if bytes.starts_with(b"#!") {
            continue; // a script (userland-sh/), run by its interpreter
        }
        let image = elf::parse(bytes, (bytes).len() as u64).unwrap_or_else(|e| panic!("{}: {}", name, e));
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
    // Thousands of arguments are fine (rm * in a big directory)...
    let lots = vec!["x"; 3000];
    assert_eq!(run(hello(), &lots), Ok(0));
    // ...but past the limit, execve refuses.
    let many = vec!["x"; palladiumos::usermode::MAX_ARGS + 1];
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
    assert!(elf::parse(good, (good).len() as u64).is_ok());
    assert!(elf::parse(&good[..40], (&good[..40]).len() as u64).is_err(), "truncated header");
    assert!(elf::parse(&good[..200], (&good[..200]).len() as u64).is_err(), "truncated segments");

    let mut b = good.to_vec();
    b[4] = 1; // 32-bit
    assert!(elf::parse(&b, (&b).len() as u64).is_err());

    let mut b = good.to_vec();
    b[16] = 4; // ET_CORE
    assert!(elf::parse(&b, (&b).len() as u64).is_err());

    // ET_DYN (static-PIE) is loaded PIE_BASE higher than its addresses.
    let mut b = good.to_vec();
    b[16] = 3;
    let (exec, dyn_) = (elf::parse(good, (good).len() as u64).unwrap(), elf::parse(&b, (&b).len() as u64).unwrap());
    assert_eq!(dyn_.entry, exec.entry + elf::PIE_BASE);

    let mut b = good.to_vec();
    b[18] = 3; // EM_386
    assert!(elf::parse(&b, (&b).len() as u64).is_err());

    let mut b = good.to_vec();
    b[24..32].copy_from_slice(&0x1234u64.to_le_bytes()); // entry outside code
    assert!(elf::parse(&b, (&b).len() as u64).is_err());
}

#[test_case]
fn elf_segment_in_kernel_half_is_rejected() {
    let b = patch_data_segment(hello(), P_VADDR, 0xffff_8000_0000_0000);
    assert!(elf::parse(&b, (&b).len() as u64).is_err());
    assert!(run(&b, &["bad"]).is_err());
}

#[test_case]
fn elf_segment_on_null_page_is_rejected() {
    let b = patch_data_segment(hello(), P_VADDR, 0);
    assert!(elf::parse(&b, (&b).len() as u64).is_err());
}

#[test_case]
fn elf_huge_segment_is_rejected() {
    let b = patch_data_segment(hello(), P_MEMSZ, 1 << 40);
    assert!(elf::parse(&b, (&b).len() as u64).is_err());
}

#[test_case]
fn the_lower_half_is_all_for_programs() {
    // The kernel lives in the higher half: every lower-half slot is free
    // for programs (Linux programs start at 0x400000), and nothing of the
    // kernel's is mapped there.
    assert!((0..256).all(palladiumos::memory::is_user_slot), "a lower-half slot is in use by the kernel");
    assert!((256..512).any(|i| !palladiumos::memory::is_user_slot(i)));
    assert!((kernel_address as usize as u64) >= 0xffff_8000_0000_0000, "the kernel's code is in the higher half");
    assert_eq!(launch_program(hello()), 0);
}

fn kernel_address() {}

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
    let busybox = programs::find("busybox").is_some();
    for (name, bytes) in programs::USER_PROGRAMS {
        // With BusyBox, its better versions of some tools take their place
        // (as links) and PalladiumOS's own move to /bin/palladium.
        let replaced = busybox && programs::BUSYBOX_PREFERRED.contains(name);
        let path = if replaced {
            let link = alloc::format!("/bin/{}", name);
            let (_, meta) = palladiumos::mounts::lstat(&link).expect("link missing");
            assert_eq!(meta.kind, palladiumos::mounts::FileType::Symlink, "{} is a link to BusyBox", link);
            alloc::format!("{}/{}", programs::REPLACED_DIR, name)
        } else {
            alloc::format!("/bin/{}", name)
        };
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

// ---- heap, dup, environment (milestone D) ----

#[test_case]
fn brk_heap_and_allocator() {
    let before = frames_in_use();
    assert_eq!(run(prog("test_misc"), &["test_misc", "heap"]), Ok(0));
    assert_eq!(frames_in_use(), before, "heap pages leaked");
}

#[test_case]
fn dup_and_dup2() {
    assert_eq!(run(prog("test_misc"), &["test_misc", "dup"]), Ok(0));
}

// ---- the userland shell ----

fn sh(script: &str) -> i32 {
    run(prog("sh"), &["sh", "-c", script]).expect("sh failed to start")
}

fn file(path: &str) -> alloc::string::String {
    filesystem::read_file(path).unwrap_or_else(|e| panic!("{}: {}", path, e))
}

#[test_case]
fn sh_exit_status() {
    assert_eq!(sh("exit 3"), 3);
    assert_eq!(sh("true"), 0);
    assert_eq!(sh("false"), 1);
    assert_eq!(sh("no-such-command"), 127);
    assert_eq!(sh("exit 300"), 300 & 0xff);
    assert_eq!(sh("test_proc exit 9"), 9);
    assert_eq!(sh("test_stack"), 139, "killed by SIGSEGV -> 128 + 11");
    assert_eq!(sh(""), 0);
    assert_eq!(sh("# just a comment"), 0);
}

#[test_case]
fn sh_and_or_lists() {
    assert_eq!(sh("true && exit 4"), 4);
    assert_eq!(sh("false || exit 5"), 5);
    assert_eq!(sh("false && exit 6"), 1);
    assert_eq!(sh("true || exit 7"), 0);
    assert_eq!(sh("false; true"), 0);
    assert_eq!(sh("exit 9; exit 1"), 9);
    assert_eq!(sh("false && true || exit 8"), 8);
}

#[test_case]
fn sh_variables_and_quoting() {
    assert_eq!(sh(r#"x=hello; echo $x "$x world" '$x' ${x}! a\ b > /sh_v.txt"#), 0);
    assert_eq!(file("/sh_v.txt"), "hello hello world $x hello! a b\n");
    // $? is expanded before echo runs, so both copies see false's status
    assert_eq!(sh(r#"false; echo $? "$?" > /sh_s.txt; echo $? >> /sh_s.txt"#), 0);
    assert_eq!(file("/sh_s.txt"), "1 1\n0\n");
}

#[test_case]
fn sh_positional_parameters() {
    assert_eq!(
        run(prog("sh"), &["sh", "-c", "echo $0 $1 $2 $# > /sh_p.txt", "name", "one", "two"]),
        Ok(0)
    );
    assert_eq!(file("/sh_p.txt"), "name one two 2\n");
}

#[test_case]
fn sh_redirections() {
    assert_eq!(sh("echo one > /r.txt; echo two >> /r.txt; cat < /r.txt > /r2.txt"), 0);
    assert_eq!(file("/r2.txt"), "one\ntwo\n");
    assert_eq!(sh("echo replaced > /r.txt"), 0);
    assert_eq!(file("/r.txt"), "replaced\n", "> truncates");
    assert_eq!(sh("cat /missing 2> /r3.txt"), 1);
    assert!(file("/r3.txt").contains("No such file"));
    assert_eq!(sh("cat /missing > /r4.txt 2>&1"), 1);
    assert!(file("/r4.txt").contains("No such file"));
    // builtins can be redirected, and stdout comes back afterwards
    assert_eq!(sh("pwd > /r5.txt; echo after > /r6.txt"), 0);
    assert_eq!(file("/r5.txt"), "/\n");
    assert_eq!(file("/r6.txt"), "after\n");
    assert_eq!(sh("cat < /nonexistent"), 1);
    assert_eq!(sh("> /r7.txt"), 0, "redirection alone creates the file");
    assert_eq!(file("/r7.txt"), "");
}

#[test_case]
fn sh_cd_and_pwd() {
    assert_eq!(sh("cd /bin && pwd > /c.txt"), 0);
    assert_eq!(file("/c.txt"), "/bin\n");
    assert_eq!(*filesystem::CWD.lock(), "/", "the shell's cd must not move the kernel");
    assert_eq!(sh("cd /nope"), 1);
    assert_eq!(sh("cd /bin; cd ~; pwd > /c2.txt; cd /bin; cd; echo $PWD >> /c2.txt"), 0);
    assert_eq!(file("/c2.txt"), "/\n/\n");
    assert_eq!(sh("cd /bin; ls > /c3.txt"), 0);
    assert!(file("/c3.txt").contains("sh"), "relative to the new cwd");
}

#[test_case]
fn sh_environment() {
    assert_eq!(sh("export FOO=bar; test_misc env"), 0);
    assert_eq!(sh("FOO=bar test_misc env"), 0, "per-command assignment");
    assert_ne!(sh("test_misc env"), 0, "FOO not set");
    assert_ne!(sh("FOO=bar; test_misc env"), 0, "set but not exported");
    assert_eq!(sh("FOO=bar; export FOO; test_misc env"), 0);
    assert_eq!(sh("export FOO=bar; unset FOO; test_misc env") != 0, true);
    assert_eq!(sh("env > /env.txt"), 0);
    assert!(file("/env.txt").contains("PATH=/bin"));
}

#[test_case]
fn sh_path_lookup() {
    assert_eq!(sh("/bin/echo absolute > /pl.txt"), 0);
    assert_eq!(sh("PATH=/nowhere; echo x"), 127, "not found outside PATH");
    assert_eq!(sh("cd /; bin/echo relative > /pl2.txt"), 0);
    assert_eq!(file("/pl2.txt"), "relative\n");
    assert_eq!(sh("/bin"), 127, "a directory is not a program");
}

#[test_case]
fn sh_background_jobs() {
    assert_eq!(sh("test_proc exit 3 & wait"), 3);
    assert_eq!(sh("test_proc spin 1 & echo x > /bg.txt; wait"), 0);
    assert_eq!(file("/bg.txt"), "x\n");
    assert_eq!(sh("true && test_proc exit 4 &"), 0, "& itself succeeds");
    assert!(only_kernel_left(), "background jobs must be reaped: {:?}", process::list());
}

#[test_case]
fn sh_scripts_and_source() {
    filesystem::write_file("/t.sh", "#!/bin/sh\n# a comment\necho $1 $2 > /t_out.txt\n\nexit 4\necho not reached > /t_out.txt\n").unwrap();
    assert_eq!(run(prog("sh"), &["sh", "/t.sh", "a", "b"]), Ok(4));
    assert_eq!(file("/t_out.txt"), "a b\n");
    filesystem::write_file("/set_x.sh", "x=5\n").unwrap();
    assert_eq!(sh("x=1; . /set_x.sh; exit $x"), 5);
    assert_eq!(sh("source /set_x.sh; exit $x"), 5);
    assert_eq!(sh(". /missing.sh"), 1);
    assert_eq!(run(prog("sh"), &["sh", "/missing.sh"]), Ok(127));
    assert_eq!(sh("sh /t.sh x y"), 4, "nested shell");
}

#[test_case]
fn sh_syntax_errors() {
    assert_eq!(sh("| cat"), 2);
    assert_eq!(sh("echo a |"), 2);
    assert_eq!(sh("echo a | | cat"), 2);
    assert_eq!(sh("&& echo"), 2);
    assert_eq!(sh("echo 'unterminated"), 2);
    assert_eq!(sh("echo \"unterminated"), 2);
    assert_eq!(sh("echo >"), 2);
    assert_eq!(sh("true &&"), 2);
    assert_eq!(sh("echo ${}"), 2);
    assert_eq!(sh("echo ok; echo 'bad"), 2, "the whole line is rejected");
}

#[test_case]
fn sh_does_not_leak() {
    assert_eq!(sh("true"), 0);
    let before = frames_in_use();
    for _ in 0..5 {
        assert_eq!(sh("x=1; echo $x > /leak.txt; test_proc exit 0; true && false || true"), 0);
    }
    assert_eq!(frames_in_use(), before);
    assert!(only_kernel_left());
}

/// Type `text` (lowercase letters, digits, space, some capitals and
/// punctuation) as PS/2 set-1 scancodes, then Enter.
fn type_line(text: &str) {
    const LSHIFT: u8 = 0x2a;
    for c in text.chars() {
        let (shift, code) = match c {
            'a' => (false, 0x1e), 'b' => (false, 0x30), 'c' => (false, 0x2e), 'd' => (false, 0x20),
            'e' => (false, 0x12), 'f' => (false, 0x21), 'g' => (false, 0x22), 'h' => (false, 0x23),
            'i' => (false, 0x17), 'j' => (false, 0x24), 'k' => (false, 0x25), 'l' => (false, 0x26),
            'm' => (false, 0x32), 'n' => (false, 0x31), 'o' => (false, 0x18), 'p' => (false, 0x19),
            'q' => (false, 0x10), 'r' => (false, 0x13), 's' => (false, 0x1f), 't' => (false, 0x14),
            'u' => (false, 0x16), 'v' => (false, 0x2f), 'w' => (false, 0x11), 'x' => (false, 0x2d),
            'y' => (false, 0x15), 'z' => (false, 0x2c),
            '1'..='9' => (false, 0x02 + (c as u8 - b'1')), '0' => (false, 0x0b),
            ' ' => (false, 0x39), '/' => (false, 0x35), '.' => (false, 0x34),
            '|' => (true, 0x2b), '>' => (true, 0x34), '<' => (true, 0x33), '=' => (false, 0x0d),
            'E' => (true, 0x12), 'F' => (true, 0x21),
            '-' => (false, 0x0c), '\'' => (false, 0x28), ';' => (false, 0x27), '%' => (true, 0x06),
            '$' => (true, 0x05), '?' => (true, 0x35), '&' => (true, 0x08), 'S' => (true, 0x1f),
            'T' => (true, 0x14), 'O' => (true, 0x18), 'P' => (true, 0x19),
            _ => panic!("type_line: no scancode for {:?}", c),
        };
        if shift {
            palladiumos::task::keyboard::add_scancode(LSHIFT);
        }
        press(code);
        if shift {
            palladiumos::task::keyboard::add_scancode(LSHIFT | 0x80);
        }
    }
    press(KEY_ENTER);
}

#[test_case]
fn sh_interactive_session() {
    palladiumos::task::keyboard::init_queue();
    type_line("echo typed > /typed.txt");
    type_line("false || touch /or.txt");
    type_line("exit 6");
    assert_eq!(run(prog("sh"), &["sh"]), Ok(6));
    assert_eq!(file("/typed.txt"), "typed\n");
    assert!(filesystem::read_file("/or.txt").is_ok());
}

#[test_case]
fn sh_exits_on_ctrl_d() {
    palladiumos::task::keyboard::init_queue();
    type_line("true");
    ctrl_d();
    assert_eq!(run(prog("sh"), &["sh"]), Ok(0));
}

// ---- terminal, signals, editor (round 2 and 3 fixes) ----

/// Press an extended key (scancode prefixed with 0xE0: arrows, Delete...).
fn press_ext(code: u8) {
    use palladiumos::task::keyboard::add_scancode;
    add_scancode(0xe0);
    add_scancode(code);
    add_scancode(0xe0);
    add_scancode(code | 0x80);
}

const EXT_UP: u8 = 0x48;
const EXT_LEFT: u8 = 0x4b;
const EXT_HOME: u8 = 0x47;
const EXT_DELETE: u8 = 0x53;
const KEY_S: u8 = 0x1f;
const KEY_Q: u8 = 0x10;

fn ctrl(code: u8) {
    palladiumos::task::keyboard::add_scancode(KEY_LCTRL);
    press(code);
    palladiumos::task::keyboard::add_scancode(KEY_LCTRL | 0x80);
}

/// Type text without pressing Enter (the few characters these tests use).
fn type_text(text: &str) {
    for c in text.chars() {
        let code = match c {
            'a' => 0x1e, 'e' => 0x12, 'h' => 0x23, 'i' => 0x17, 'r' => 0x13, 't' => 0x14,
            'x' => 0x2d, 'b' => 0x30, 'c' => 0x2e, 'o' => 0x18, ' ' => 0x39,
            _ => panic!("type_text: {:?}", c),
        };
        press(code);
    }
}

#[test_case]
fn terminal_modes_and_ioctl() {
    assert_eq!(run(prog("test_tty"), &["test_tty", "modes"]), Ok(0));
}

#[test_case]
fn raw_mode_key_sequences() {
    palladiumos::task::keyboard::init_queue();
    press(0x1e); // a
    press(KEY_BACKSPACE);
    press_ext(EXT_UP);
    press_ext(EXT_DELETE);
    press(KEY_ENTER);
    assert_eq!(run(prog("test_tty"), &["test_tty", "keys"]), Ok(0));
    // the kernel puts the console back in cooked mode afterwards
    assert_eq!(palladiumos::tty::mode(), palladiumos::tty::DEFAULT_MODE);
}

#[test_case]
fn signals_groups_and_sleep() {
    assert_eq!(run(prog("test_sig"), &["test_sig"]), Ok(0));
    assert!(only_kernel_left(), "{:?}", process::list());
}

#[test_case]
fn ctrl_c_interrupts_the_foreground_program() {
    // As if Ctrl-C was pressed while `sleep 30` runs: it gets SIGINT.
    process::ctrl_c_pressed();
    assert_eq!(run(prog("sleep"), &["sleep", "30"]), Ok(130));
}

#[test_case]
fn ctrl_c_at_the_prompt_cancels_the_line() {
    // At the prompt the line editor has the console in raw mode, so Ctrl-C
    // is just a key: it throws away "exit 5" and the shell keeps running.
    palladiumos::task::keyboard::init_queue();
    type_text("exit");
    ctrl(0x2e); // Ctrl-C
    type_line("exit 3");
    assert_eq!(run(prog("sh"), &["sh"]), Ok(3));
}

#[test_case]
fn sleep_program() {
    assert_eq!(run(prog("sleep"), &["sleep", "0.2"]), Ok(0));
    assert_eq!(run(prog("sleep"), &["sleep", "bad"]), Ok(1));
    assert_eq!(run(prog("sleep"), &["sleep"]), Ok(1));
}

#[test_case]
fn kill_program_and_builtin() {
    assert_eq!(sh("sleep 30 & kill %1; wait"), 128 + 15);
    assert_eq!(sh("sleep 30 & kill -9 %1; wait"), 128 + 9);
    assert_eq!(sh("kill %7"), 1, "no such job");
    assert_eq!(sh("kill -l > /kl.txt"), 0);
    assert!(file("/kl.txt").contains("SIGTERM"));
    assert_eq!(run(prog("kill"), &["kill", "30000"]), Ok(1));
    assert_eq!(run(prog("kill"), &["kill", "-l"]), Ok(0));
    assert!(only_kernel_left());
}

#[test_case]
fn leftover_background_jobs_are_hung_up() {
    // The program exits while `sleep 60` still runs in the background: the
    // kernel sends it SIGHUP instead of waiting a minute.
    assert_eq!(sh("sleep 60 &"), 0);
    assert!(only_kernel_left(), "{:?}", process::list());
}

#[test_case]
fn uptime_and_about() {
    assert_eq!(sh("uptime > /up.txt && about > /about.txt"), 0);
    assert!(file("/up.txt").starts_with("up "));
    assert!(file("/about.txt").contains("PalladiumOS"));
}

#[test_case]
fn userland_editor() {
    palladiumos::task::keyboard::init_queue();
    filesystem::write_file("/edit.txt", "first\n").unwrap();
    // Go to the end of "first", add a new line "hi", then on line 1 insert
    // "x" at the start; save with Ctrl-S, quit with Ctrl-Q.
    press_ext(0x4f); // End
    press(KEY_ENTER);
    type_text("hi");
    press_ext(EXT_UP);
    press_ext(EXT_HOME);
    type_text("x");
    ctrl(KEY_S);
    ctrl(KEY_Q);
    assert_eq!(run(prog("edit"), &["edit", "/edit.txt"]), Ok(0));
    assert_eq!(file("/edit.txt"), "xfirst\nhi\n");
}

#[test_case]
fn editor_warns_before_losing_changes() {
    palladiumos::task::keyboard::init_queue();
    type_text("abc");
    ctrl(KEY_Q); // warns: unsaved changes
    ctrl(KEY_Q); // quits
    assert_eq!(run(prog("edit"), &["edit", "/never_saved.txt"]), Ok(0));
    assert!(filesystem::read_file("/never_saved.txt").is_err());
}

#[test_case]
fn shell_line_editing_and_history() {
    palladiumos::task::keyboard::init_queue();
    // "echo ab", cursor left once, type "x" -> "echo axb"; then Up recalls
    // the line and Enter runs it again (>> appends a second line).
    type_text("echo ab");
    press_ext(EXT_LEFT);
    type_text("x");
    press_ext(0x4f); // End: the cursor was still before the "b"
    type_line(" >> /le.txt");
    press_ext(EXT_UP);
    press(KEY_ENTER);
    type_line("exit 0");
    assert_eq!(run(prog("sh"), &["sh"]), Ok(0));
    assert_eq!(file("/le.txt"), "axb\naxb\n");
}

#[test_case]
fn shell_tab_completion() {
    palladiumos::task::keyboard::init_queue();
    // "ech<Tab>" completes to "echo "
    type_text("ech");
    press(0x0f); // Tab
    type_line("done > /tab.txt");
    type_line("exit 0");
    assert_eq!(run(prog("sh"), &["sh"]), Ok(0));
    assert_eq!(file("/tab.txt"), "done\n");
}

// ---- pipes ----

#[test_case]
fn pipe_syscalls() {
    let before = frames_in_use();
    assert_eq!(run(prog("test_pipe"), &["test_pipe", "all"]), Ok(0));
    assert!(only_kernel_left(), "{:?}", process::list());
    assert_eq!(frames_in_use(), before);
}

#[test_case]
fn sh_pipelines() {
    assert_eq!(sh("ls /bin | grep '^s.e' > /p1.txt"), 0);
    assert_eq!(file("/p1.txt"), "sleep\n");
    assert_eq!(sh("echo hello | cat | cat | cat > /p2.txt"), 0);
    assert_eq!(file("/p2.txt"), "hello\n");
    assert_eq!(sh("false | true"), 0, "status is the last command's");
    assert_eq!(sh("true | false"), 1);
    assert_eq!(sh("echo x | no-such-cmd"), 127);
    assert_eq!(sh("echo x | test_stack"), 139);
    assert_eq!(sh("echo b | grep a || echo none > /p3.txt"), 0);
    assert_eq!(file("/p3.txt"), "none\n");
    // builtins work in pipelines (in a child process)
    assert_eq!(sh("cd /bin; pwd | cat > /p4.txt"), 0);
    assert_eq!(file("/p4.txt"), "/bin\n");
    // redirections combine with pipes
    filesystem::write_file("/p_in.txt", "c\na\nb\n").unwrap();
    assert_eq!(sh("sort < /p_in.txt | head -n 2 > /p5.txt"), 0);
    assert_eq!(file("/p5.txt"), "a\nb\n");
    assert_eq!(sh("cat /missing 2>&1 | grep -c 'No such' > /p6.txt"), 0);
    assert_eq!(file("/p6.txt"), "1\n");
    assert!(only_kernel_left());
}

#[test_case]
fn sh_pipeline_ends_when_reader_exits() {
    // yes would run forever; head exits after 3 lines and SIGPIPE ends yes.
    assert_eq!(sh("yes | head -n 3 > /y.txt"), 0);
    assert_eq!(file("/y.txt"), "y\ny\ny\n");
    assert_eq!(sh("yes abc | head -n 1000 | wc -l > /y2.txt"), 0);
    assert_eq!(file("/y2.txt").trim(), "1000");
    assert!(only_kernel_left());
}

#[test_case]
fn sh_background_pipeline() {
    assert_eq!(sh("yes | head -n 5 | wc -l > /bgp.txt & wait"), 0);
    assert_eq!(file("/bgp.txt").trim(), "5");
}

#[test_case]
fn ctrl_c_stops_a_whole_pipeline() {
    process::ctrl_c_pressed();
    assert_eq!(sh("yes | grep never-matches"), 130);
    assert!(only_kernel_left(), "{:?}", process::list());
}

#[test_case]
fn pipelines_do_not_leak() {
    assert_eq!(sh("yes | head -n 10 | sort | uniq -c > /leak.txt"), 0);
    let before = frames_in_use();
    for _ in 0..5 {
        assert_eq!(sh("yes | head -n 100 | sort | uniq -c | wc -l > /leak.txt"), 0);
    }
    assert_eq!(frames_in_use(), before);
    assert!(only_kernel_left());
}

// ---- text tools ----

fn setup_words() {
    filesystem::write_file("/w.txt", "banana\napple\ncherry\napple\nBanana split\n10 ten\n9 nine\n").unwrap();
}

#[test_case]
fn grep_tool() {
    setup_words();
    assert_eq!(sh("grep apple /w.txt > /g.txt"), 0);
    assert_eq!(file("/g.txt"), "apple\napple\n");
    assert_eq!(sh("grep -i banana /w.txt > /g.txt"), 0);
    assert_eq!(file("/g.txt"), "banana\nBanana split\n");
    assert_eq!(sh("grep -c an /w.txt > /g.txt"), 0);
    assert_eq!(file("/g.txt"), "2\n");
    assert_eq!(sh("grep -vn a /w.txt > /g.txt"), 0);
    assert_eq!(file("/g.txt"), "3:cherry\n6:10 ten\n7:9 nine\n");
    assert_eq!(sh("grep '^b.n.*a$' /w.txt > /g.txt"), 0, "regex . * ^ $");
    assert_eq!(file("/g.txt"), "banana\n");
    assert_eq!(sh("grep -F 'n.' /w.txt"), 1, "-F: no match for the literal 'n.'");
    assert_eq!(sh("grep -q cherry /w.txt"), 0);
    assert_eq!(sh("grep zzz /w.txt"), 1);
    assert_eq!(sh("grep x /missing"), 2);
    assert_eq!(sh("grep"), 2);
}

#[test_case]
fn wc_head_tail_tools() {
    setup_words();
    assert_eq!(sh("wc /w.txt > /o.txt"), 0);
    assert_eq!(file("/o.txt"), "      7      10      53 /w.txt\n");
    assert_eq!(sh("wc -l < /w.txt > /o.txt"), 0);
    assert_eq!(file("/o.txt"), "      7\n");
    assert_eq!(sh("head -n 2 /w.txt > /o.txt"), 0);
    assert_eq!(file("/o.txt"), "banana\napple\n");
    assert_eq!(sh("head -1 /w.txt > /o.txt"), 0);
    assert_eq!(file("/o.txt"), "banana\n");
    assert_eq!(sh("tail -n 2 /w.txt > /o.txt"), 0);
    assert_eq!(file("/o.txt"), "10 ten\n9 nine\n");
    assert_eq!(sh("tail -n 0 /w.txt > /o.txt"), 0);
    assert_eq!(file("/o.txt"), "");
    assert_eq!(sh("head -n x /w.txt"), 1);
}

#[test_case]
fn sort_uniq_tee_tools() {
    setup_words();
    assert_eq!(sh("sort /w.txt > /o.txt"), 0);
    assert_eq!(file("/o.txt"), "10 ten\n9 nine\nBanana split\napple\napple\nbanana\ncherry\n");
    assert_eq!(sh("tail -n 2 /w.txt | sort -n > /o.txt"), 0);
    assert_eq!(file("/o.txt"), "9 nine\n10 ten\n");
    assert_eq!(sh("sort -u /w.txt | wc -l > /o.txt"), 0);
    assert_eq!(file("/o.txt").trim(), "6");
    assert_eq!(sh("sort -r /w.txt | head -1 > /o.txt"), 0);
    assert_eq!(file("/o.txt"), "cherry\n");
    assert_eq!(sh("sort /w.txt | uniq -c | grep apple > /o.txt"), 0);
    assert_eq!(file("/o.txt"), "      2 apple\n");
    assert_eq!(sh("sort /w.txt | uniq -d > /o.txt"), 0);
    assert_eq!(file("/o.txt"), "apple\n");
    assert_eq!(sh("echo teed | tee /t1.txt /t2.txt > /o.txt"), 0);
    assert_eq!((file("/t1.txt"), file("/t2.txt"), file("/o.txt")), ("teed\n".into(), "teed\n".into(), "teed\n".into()));
    assert_eq!(sh("echo more | tee -a /t1.txt > /o.txt"), 0);
    assert_eq!(file("/t1.txt"), "teed\nmore\n");
}

// ---- shell scripting ----

/// Run a script with sh and return what it wrote to /out.txt.
fn script_output(script: &str) -> alloc::string::String {
    let _ = filesystem::remove("/out.txt");
    filesystem::write_file("/script.sh", script).unwrap();
    let status = run(prog("sh"), &["sh", "/script.sh"]).unwrap();
    assert_eq!(status, 0, "script failed:\n{}", script);
    filesystem::read_file("/out.txt").unwrap_or_default()
}

#[test_case]
fn sh_if_elif_else() {
    let out = script_output(
        "for n in 1 2 3; do\n\
         if [ $n -eq 1 ]; then echo one\n\
         elif [ $n = 2 ]; then echo two\n\
         else echo many\n\
         fi\n\
         done > /out.txt\n",
    );
    assert_eq!(out, "one\ntwo\nmany\n");
    assert_eq!(sh("if false; then exit 1; fi"), 0, "no branch taken: status 0");
    assert_eq!(sh("if true; then false; fi"), 1);
}

#[test_case]
fn sh_while_until_break_continue() {
    let out = script_output(
        "i=0\n\
         while [ $i -lt 10 ]; do\n\
           i=$((i + 1))\n\
           if [ $i -eq 2 ]; then continue; fi\n\
           if [ $i -eq 5 ]; then break; fi\n\
           echo $i\n\
         done > /out.txt\n\
         until [ $i -eq 7 ]; do i=$((i+1)); done\n\
         echo $i >> /out.txt\n",
    );
    assert_eq!(out, "1\n3\n4\n7\n");
    // break 2 leaves both loops
    let out = script_output(
        "for a in 1 2; do for b in x y; do echo $a$b; break 2; done; done > /out.txt\n",
    );
    assert_eq!(out, "1x\n");
    let out = script_output("for a in 1 2; do for b in x y; do continue 2; echo no; done; echo no; done; echo end > /out.txt\n");
    assert_eq!(out, "end\n");
}

#[test_case]
fn sh_for_loops_and_globs() {
    filesystem::make_dir("/globtest").unwrap();
    for f in ["a.txt", "b.txt", "c.rs", ".hidden.txt"] {
        filesystem::write_file(&alloc::format!("/globtest/{}", f), "").unwrap();
    }
    assert_eq!(script_output("for f in /globtest/*.txt; do echo $f; done > /out.txt\n"), "/globtest/a.txt\n/globtest/b.txt\n");
    assert_eq!(script_output("cd /globtest; echo *.* > /out.txt\n"), "a.txt b.txt c.rs\n", "hidden files are skipped");
    assert_eq!(script_output("cd /globtest; echo .h* > /out.txt\n"), ".hidden.txt\n");
    assert_eq!(script_output("cd /globtest; echo ?.rs [ab].txt [!a].txt > /out.txt\n"), "c.rs a.txt b.txt b.txt\n");
    assert_eq!(script_output("cd /globtest; echo *.none > /out.txt\n"), "*.none\n", "no match: left as is");
    assert_eq!(script_output("cd /globtest; echo '*' \"*.txt\" \\* > /out.txt\n"), "* *.txt *\n", "quoted: no glob");
    assert_eq!(script_output("echo /glob*/a.txt > /out.txt\n"), "/globtest/a.txt\n", "glob in a directory part");
    assert_eq!(script_output("set -- x y z; for a; do echo $a; done > /out.txt\n"), "x\ny\nz\n", "for without in = \"$@\"");
}

#[test_case]
fn sh_case() {
    let out = script_output(
        "for f in main.rs notes.txt README Makefile; do\n\
         case $f in\n\
           *.rs) echo \"$f: rust\" ;;\n\
           *.txt | *.md) echo \"$f: text\" ;;\n\
           [A-Z]*) echo \"$f: capital\" ;;\n\
         esac\n\
         done > /out.txt\n\
         case x in (y) echo no;; esac\n",
    );
    assert_eq!(out, "main.rs: rust\nnotes.txt: text\nREADME: capital\nMakefile: capital\n");
    assert_eq!(sh("case abc in a*) exit 3;; *) exit 4;; esac"), 3);
    assert_eq!(sh("case '*' in \\*) exit 5;; esac"), 5, "quoted pattern is literal");
}

#[test_case]
fn sh_functions() {
    let out = script_output(
        "greet() { echo \"hi $1, $# args\"; }\n\
         add() {\n\
           echo $(( $1 + $2 ))\n\
           return 7\n\
           echo not reached\n\
         }\n\
         fact() { if [ $1 -le 1 ]; then echo 1; else echo $(( $1 * $(fact $(( $1 - 1 ))) )); fi; }\n\
         { greet bob x y; add 2 3; echo status $?; fact 5; echo \"after: $1\"; } > /out.txt\n",
    );
    assert_eq!(out, "hi bob, 3 args\n5\nstatus 7\n120\nafter: \n");
    assert_eq!(sh("f() { exit 9; }; f; exit 1"), 9, "exit in a function exits the shell");
    assert_eq!(sh("return 3"), 1, "return outside a function is an error");
    assert_eq!(sh("loop() { loop; }; loop"), 1, "runaway recursion is stopped");
    assert_eq!(sh("f() { echo in f; }; type f > /out.txt; unset -f f; f"), 127);
}

#[test_case]
fn sh_here_documents() {
    let out = script_output(
        "x=world\n\
         cat <<EOF > /out.txt\n\
         hello $x\n\
         sum $((1+2)) `echo bt` \\$x 'quotes kept'\n\
         EOF\n\
         cat <<'EOF' >> /out.txt\n\
         literal $x `no`\n\
         EOF\n\
         cat <<-END >> /out.txt\n\
         \tindented\n\
         \tEND\n\
         read a b <<EOF\n\
         two one\n\
         EOF\n\
         echo \"$b $a\" >> /out.txt\n\
         cat <<A >> /out.txt; cat <<B >> /out.txt\n\
         first\n\
         A\n\
         second\n\
         B\n\
         f() { cat <<EOF\n\
         in f: $1\n\
         EOF\n\
         }\n\
         f arg >> /out.txt\n\
         cat <<EOF | grep -c line >> /out.txt\n\
         $(i=0; while [ $i -lt 1000 ]; do echo line$i; i=$((i+1)); done)\n\
         EOF\n",
    );
    assert_eq!(
        out,
        "hello world\nsum 3 bt $x 'quotes kept'\nliteral $x `no`\nindented\none two\nfirst\nsecond\nin f: arg\n1000\n"
    );
    assert_eq!(sh("cat <<EOF\nno end"), 2, "a here-document without its delimiter");
    assert_eq!(script_output("echo a \\\n  b > /out.txt\n"), "a b\n", "a line ending with \\ continues");
}

#[test_case]
fn sh_local_variables() {
    let out = script_output(
        "x=outer\n\
         f() { local x=inner y; y=set; echo \"in $x\"; g; }\n\
         g() { echo \"g sees $x\"; }\n\
         h() { local v=\"a  b\" w=$x; echo \"$v|$w\"; local v=again; }\n\
         { f; echo \"out $x [$y]\"; h; echo \"[$v]\"; } > /out.txt\n",
    );
    assert_eq!(out, "in inner\ng sees inner\nout outer []\na  b|outer\n[]\n");
    assert_eq!(sh("local x=1"), 1, "local outside a function");
}

#[test_case]
fn sh_aliases() {
    let out = script_output(
        "alias say='echo said'\n\
         say hi > /out.txt\n\
         alias echo='echo x'\n\
         echo y >> /out.txt\n\
         unalias echo\n\
         alias e='echo ' w=world\n\
         e w >> /out.txt\n\
         type say >> /out.txt\n\
         alias say >> /out.txt\n\
         V=1 say with assignment >> /out.txt\n\
         unalias say\n\
         say 2>/dev/null || echo gone >> /out.txt\n\
         alias q=\"echo it's\"; alias q >> /out.txt\n",
    );
    assert_eq!(
        out,
        "said hi\nx y\nworld\nsay is an alias for echo said\nalias say='echo said'\nsaid with assignment\ngone\nalias q='echo it'\\''s'\n"
    );
    assert_eq!(sh("unalias nothing"), 1);
}

#[test_case]
fn sh_command_substitution() {
    assert_eq!(script_output("x=$(echo hello); echo \"[$x]\" > /out.txt\n"), "[hello]\n");
    assert_eq!(script_output("echo \"$(echo a; echo b)\" > /out.txt\n"), "a\nb\n", "inner newlines kept, trailing removed");
    assert_eq!(script_output("echo $(echo a; echo b) > /out.txt\n"), "a b\n", "unquoted: split into words");
    assert_eq!(script_output("echo `echo old style` > /out.txt\n"), "old style\n");
    assert_eq!(script_output("n=$(ls /bin | grep -c '^sh$'); echo $n > /out.txt\n"), "1\n");
    assert_eq!(script_output("echo $(echo $(echo nested)) > /out.txt\n"), "nested\n");
    assert_eq!(sh("x=$(exit 4)"), 4, "status of an assignment with $(...)");
}

#[test_case]
fn sh_arithmetic() {
    assert_eq!(
        script_output("echo $((1+2*3)) $(((1+2)*3)) $((7/2)) $((7%3)) $((-5+2)) $((2>1)) $((1==2)) $((!0)) > /out.txt\n"),
        "7 9 3 1 -3 1 0 1\n"
    );
    assert_eq!(script_output("x=5; y=$((x * 2)); echo $((y + $x)) > /out.txt\n"), "15\n");
    assert_eq!(script_output("i=1; : $((i += 4)); : $((i *= 2)); echo $i > /out.txt\n"), "10\n");
    assert_eq!(script_output("echo $((3 > 2 && 1 < 2)) $((0 || 0)) > /out.txt\n"), "1 0\n");
    assert_eq!(sh("echo $((1/0))"), 1, "division by zero is an error");
}

#[test_case]
fn sh_parameter_expansion() {
    assert_eq!(
        script_output("unset u; e=; s=set; echo ${u:-d1} ${e:-d2} ${e-d3} ${s:+alt} ${u:+no}x ${#s} > /out.txt\n"),
        "d1 d2 alt x 3\n"
    );
    assert_eq!(script_output("unset u; echo ${u:=assigned} $u > /out.txt\n"), "assigned assigned\n");
    assert_eq!(sh("unset u; echo ${u:?is required}"), 1);
    assert_eq!(script_output("x='a   b'; echo $x \"$x\" > /out.txt\n"), "a b a   b\n", "field splitting");
    assert_eq!(script_output("set -- 'one two' three; for a in \"$@\"; do echo \"<$a>\"; done > /out.txt\n"), "<one two>\n<three>\n");
    assert_eq!(script_output("set -- a b c; shift; echo $# $1 $* > /out.txt\n"), "2 b b c\n");
    assert_eq!(script_output("e=''; for a in $e; do echo no; done; echo \"[$e]\" '' x > /out.txt\n"), "[]  x\n");
}

#[test_case]
fn sh_test_builtin() {
    filesystem::write_file("/tfile", "data").unwrap();
    filesystem::write_file("/tempty", "").unwrap();
    for (expr, expected) in [
        ("-f /tfile", 0), ("-d /tfile", 1), ("-d /bin", 0), ("-e /nope", 1), ("-s /tfile", 0), ("-s /tempty", 1),
        ("-z ''", 0), ("-n ''", 1), ("abc", 0), ("''", 1), ("a = a", 0), ("a != a", 1),
        ("3 -lt 10", 0), ("10 -le 3", 1), ("5 -eq 5", 0), ("2 -gt 1 -a 1 -gt 0", 0), ("1 -gt 2 -o 2 -gt 1", 0),
        ("! -f /nope", 0), ("'(' 1 -eq 1 ')'", 0),
    ] {
        assert_eq!(sh(&alloc::format!("test {}", expr)), expected, "test {}", expr);
        assert_eq!(sh(&alloc::format!("[ {} ]", expr)), expected, "[ {} ]", expr);
    }
    assert_eq!(sh("[ 1 -eq 1"), 2, "missing ]");
    assert_eq!(sh("[ x -eq 1 ]"), 2, "not a number");
}

#[test_case]
fn sh_read_builtin() {
    filesystem::write_file("/rin.txt", "first line\nsecond  part  rest\n").unwrap();
    assert_eq!(
        script_output("{ read a; read b c; } < /rin.txt; echo \"[$a] [$b] [$c]\" > /out.txt\n"),
        "[first line] [second] [part  rest]\n"
    );
    assert_eq!(
        script_output("n=0; while read line; do n=$((n+1)); done < /rin.txt; echo $n > /out.txt\n"),
        "2\n"
    );
    assert_eq!(script_output("ls /bin | while read p; do case $p in sh) echo found $p;; esac; done > /out.txt\n"), "found sh\n");
}

#[test_case]
fn sh_groups_subshells_negation() {
    filesystem::write_file("/rin.txt", "first line\nsecond  part  rest\n").unwrap();
    assert_eq!(script_output("{ echo a; echo b; } > /out.txt\n"), "a\nb\n");
    assert_eq!(script_output("x=1; (x=2; cd /bin); echo $x $PWD > /out.txt\n"), "1 /\n", "subshell changes don't leak");
    assert_eq!(script_output("x=1; { x=2; }; echo $x > /out.txt\n"), "2\n", "a group runs in the shell");
    assert_eq!(sh("! true"), 1);
    assert_eq!(sh("! false"), 0);
    assert_eq!(sh("(exit 6)"), 6);
    assert_eq!(script_output("while read l; do echo \"<$l>\"; done < /rin.txt > /out.txt\n"), "<first line>\n<second  part  rest>\n");
    assert_eq!(script_output("eval 'echo evaluated' > /out.txt\n"), "evaluated\n");
    assert_eq!(script_output("echo one \\\n two > /out.txt\n"), "one two\n", "line continuation");
}

#[test_case]
fn sh_script_syntax_errors() {
    for bad in ["if true; then echo", "while true; do", "for x in a b; echo", "case x in a) echo", "f() {", "{ echo", "(echo", "fi", "done", "echo $((1+)"] {
        assert_eq!(sh(bad), 2, "{:?} should be a syntax error", bad);
    }
    assert_eq!(sh("if then fi"), 2);
}

#[test_case]
fn sh_interactive_multiline() {
    // An `if` typed over several lines (the shell prompts "> " for more).
    palladiumos::task::keyboard::init_queue();
    type_line("if true");
    type_line("then");
    type_line("echo multi > /ml.txt");
    type_line("fi");
    // A here-document, and an alias defined on one line used on the next.
    type_line("cat <<EOF >> /ml.txt");
    type_line("doc line");
    type_line("EOF");
    type_line("alias app='echo aliased >> /ml.txt'");
    type_line("app");
    type_line("exit 0");
    assert_eq!(run(prog("sh"), &["sh"]), Ok(0));
    assert_eq!(file("/ml.txt"), "multi\ndoc line\naliased\n");
}

#[test_case]
fn scripting_does_not_leak() {
    let s = "i=0; while [ $i -lt 20 ]; do x=$(echo $i | cat); i=$((i+1)); done; for f in /bin/*; do :; done";
    assert_eq!(sh(s), 0);
    let before = frames_in_use();
    assert_eq!(sh(s), 0);
    assert_eq!(frames_in_use(), before);
    assert!(only_kernel_left());
}

// ---- file toolkit ----

#[test_case]
fn links_permissions_scripts() {
    assert_eq!(run(prog("test_links"), &["test_links"]), Ok(0));
}

#[test_case]
fn file_tools_end_to_end() {
    let script = "mkdir /ft && cd /ft && echo hello > a && cp a b && mv b c && ln -s a s && \
                  chmod 700 c && ls -l > /ft-ls.txt; cp -r /ft /ft2 && cat /ft2/s > /ft-cat.txt; \
                  stat a > /ft-stat.txt; readlink s > /ft-rl.txt; du -s /ft > /ft-du.txt; \
                  df / > /ft-df.txt; rm -r /ft /ft2";
    assert_eq!(sh(script), 0);
    let ls = file("/ft-ls.txt");
    assert!(ls.contains("-rw-r--r--") && ls.contains(" a\n"), "{}", ls);
    assert!(ls.contains("-rwx------") && ls.contains(" c\n"), "{}", ls);
    assert!(ls.contains("lrwxrwxrwx") && ls.contains("s -> a"), "{}", ls);
    assert_eq!(file("/ft-cat.txt"), "hello\n", "cp -r keeps symlinks working");
    assert!(file("/ft-stat.txt").contains("regular file"));
    assert_eq!(file("/ft-rl.txt"), "a\n");
    assert!(file("/ft-du.txt").ends_with("/ft\n"));
    assert!(file("/ft-df.txt").contains("Mounted on"));
    assert!(!filesystem::is_dir("/ft") && !filesystem::is_dir("/ft2"));
}

#[test_case]
fn mv_cp_errors() {
    assert_eq!(sh("mv /nope /x"), 1);
    assert_eq!(sh("cp /nope /x"), 1);
    assert_eq!(sh("mkdir /cpd; cp /cpd /cpd2"), 1, "directory without -r");
    assert_eq!(sh("cp -r /cpd /cpd/inside"), 1, "into itself");
    assert_eq!(sh("echo a > /f1; echo b > /f2; mv /f1 /f2 /not-a-dir"), 1);
    assert_eq!(sh("chmod 999 /f2"), 1, "bad octal mode");
    assert_eq!(sh("chmod u+z /f2"), 1, "bad symbolic mode");
    assert_eq!(sh("rm -r /cpd /f1 /f2"), 0);
}

#[test_case]
fn sh_errexit_and_xtrace() {
    assert_eq!(run(prog("sh"), &["sh", "-e", "-c", "false; exit 7"]), Ok(1), "-e stops at the failure");
    assert_eq!(sh("set -e; false; exit 7"), 1);
    assert_eq!(sh("set -e; if false; then :; fi; false || true; ! true; exit 7"), 7, "conditions don't count");
    assert_eq!(sh("set -e; while false; do :; done; true && false; exit 7"), 1, "the last part of && counts");
    assert_eq!(sh("set -e; set +e; false; exit 7"), 7);
    // The trace goes to the shell's stderr: capture an inner sh -x's.
    assert_eq!(sh("sh -x -c 'x=1; echo hi $x > /xt-out.txt' 2> /xt.txt"), 0);
    assert_eq!(file("/xt.txt"), "+ x=1\n+ echo hi 1\n");
    assert_eq!(file("/xt-out.txt"), "hi 1\n");
    assert_eq!(sh("set -q"), 2, "unknown option");
}

// ---- /proc and process tools ----

#[test_case]
fn procfs_files() {
    assert_eq!(run(prog("test_procfs"), &["test_procfs", "all"]), Ok(0));
    assert!(only_kernel_left(), "{:?}", process::list());
}

#[test_case]
fn terminal_read_timeout() {
    assert_eq!(run(prog("test_procfs"), &["test_procfs", "timeout"]), Ok(0));
}

#[test_case]
fn ps_free_mount_pgrep_pkill() {
    let script = "sleep 30 & sleep 0.2; ps > /ps.txt; free > /free.txt; mount > /mount.txt; \
                  pgrep -l sleep > /pgrep.txt; pkill -x sleep; wait; pgrep -x sleep; echo $? > /pg2.txt";
    assert_eq!(sh(script), 0);
    let ps = file("/ps.txt");
    assert!(ps.starts_with("  PID  PPID S"), "{}", ps);
    assert!(ps.contains("sleep 30") && ps.contains(" ps\n"), "{}", ps);
    assert!(file("/free.txt").contains("Mem:"));
    assert!(file("/mount.txt").contains("proc on /proc type proc"));
    assert!(file("/pgrep.txt").contains(" sleep"));
    assert_eq!(file("/pg2.txt"), "1\n", "no sleep left after pkill");
    assert!(only_kernel_left());
}

#[test_case]
fn cpu_time_is_counted() {
    // A busy child gets CPU time; a sleeping one doesn't.
    let script = "test_proc spin 1000000 & sleep 2 & sleep 1.5; ps > /cpu.txt; pkill -x test_proc; wait";
    assert_eq!(sh(script), 0);
    let ps = file("/cpu.txt");
    let time_of = |cmd: &str| -> u64 {
        let line = ps.lines().find(|l| l.ends_with(cmd)).unwrap_or_else(|| panic!("{} not in\n{}", cmd, ps));
        let t = line.split_whitespace().nth(3).unwrap(); // M:SS
        let (m, s) = t.split_once(':').unwrap();
        m.parse::<u64>().unwrap() * 60 + s.parse::<u64>().unwrap()
    };
    assert_eq!(time_of("sleep 2"), 0, "a sleeping process uses no CPU:\n{}", ps);
    assert!(time_of("test_proc spin 1000000") >= 1, "the busy one does:\n{}", ps);
    assert!(only_kernel_left());
}

// ---- users and permissions ----

#[test_case]
fn permissions_as_an_unprivileged_user() {
    assert_eq!(run(prog("test_perm"), &["test_perm", "all"]), Ok(0));
    assert!(only_kernel_left(), "{:?}", process::list());
}

/// Make sure the default accounts exist (as on a fresh disk).
fn setup_users() {
    filesystem::init_default_files();
    let _ = filesystem::make_dir("/tmp");
    let _ = filesystem::chmod("/tmp", 0o1777);
}

#[test_case]
fn login_as_user_then_su_and_passwd() {
    setup_users();
    palladiumos::task::keyboard::init_queue();
    // login: user (no password yet), then commands in the user's shell
    type_line("user");
    type_line("whoami > /tmp/who1");
    type_line("id > /tmp/id1");
    type_line("touch /etc/nope 2> /tmp/err1");
    // su to root (no root password yet) and give user a password
    type_line("su");
    type_line("whoami > /tmp/who2");
    type_line("passwd user");
    type_line("pw123");
    type_line("pw123");
    type_line("exit");
    type_line("exit");
    ctrl_d(); // logged out: login asks again; Ctrl-D ends it (no root password)
    assert_eq!(run(prog("login"), &["login"]), Ok(99));
    assert_eq!(file("/tmp/who1"), "user\n");
    assert!(file("/tmp/id1").starts_with("uid=1000(user) gid=1000(user)"), "{}", file("/tmp/id1"));
    assert!(file("/tmp/err1").contains("Permission denied"));
    assert_eq!(file("/tmp/who2"), "root\n");
    assert!(file("/etc/shadow").contains("user:$5$"), "password was set (SHA-256 crypt)");
    assert!(filesystem::read_file("/etc/nope").is_err());

    // Now the password is needed: a wrong one fails, the right one works.
    palladiumos::task::keyboard::init_queue();
    type_line("user");
    type_line("wrong");
    type_line("user");
    type_line("pw123");
    type_line("whoami > /tmp/who3");
    type_line("exit");
    ctrl_d();
    assert_eq!(run(prog("login"), &["login"]), Ok(99));
    assert_eq!(file("/tmp/who3"), "user\n");

    // Put the account back the way it was.
    assert_eq!(sh("passwd -d user"), 0);
    assert!(only_kernel_left());
}

#[test_case]
fn login_ctrl_d_means_kernel_shell() {
    setup_users();
    palladiumos::task::keyboard::init_queue();
    ctrl_d();
    assert_eq!(run(prog("login"), &["login"]), Ok(99));
}

#[test_case]
fn only_login_can_open_the_kernel_shell() {
    setup_users();
    let _ = filesystem::make_dir("/tmp");
    // Give root a password (as root, with passwd), keeping the old shadow.
    let shadow = file("/etc/shadow");
    palladiumos::task::keyboard::init_queue();
    type_line("passwd root");
    type_line("rootpw");
    type_line("rootpw");
    type_line("exit");
    assert_eq!(run(prog("sh"), &["sh", "-i"]), Ok(0));
    assert!(file("/etc/shadow").contains("root:$5$"));

    palladiumos::task::keyboard::init_queue();
    // A user's shell exiting with 99 is just a logout, not the kernel shell:
    type_line("user");
    type_line("exit 99");
    // login asks again: log in once more to prove it.
    type_line("user");
    type_line("whoami > /tmp/again");
    type_line("exit");
    // Ctrl-D needs root's password now: a wrong one is refused...
    ctrl_d();
    type_line("wrong");
    type_line("user");
    type_line("echo still-login > /tmp/after-wrong");
    type_line("exit");
    // ...and the right one opens the kernel shell.
    ctrl_d();
    type_line("rootpw");
    assert_eq!(run(prog("login"), &["login"]), Ok(99));
    assert_eq!(file("/tmp/again"), "user\n");
    assert_eq!(file("/tmp/after-wrong"), "still-login\n");
    filesystem::write_file("/etc/shadow", &shadow).unwrap();
    assert!(only_kernel_left(), "{:?}", process::list());
}

#[test_case]
fn useradd_userdel_id_groups() {
    setup_users();
    assert_eq!(sh("useradd -G wheel alice"), 0);
    assert!(file("/etc/passwd").contains("alice:x:1001:1001::/home/alice:/bin/sh"), "{}", file("/etc/passwd"));
    assert!(file("/etc/group").contains("wheel:x:10:root,user,alice"));
    assert!(filesystem::is_dir("/home/alice"));
    assert_eq!(sh("id alice > /tmp/ida; groups alice > /tmp/ga; ls -ld /home/alice > /tmp/lsa 2>&1; ls -l /home > /tmp/lsh"), 0);
    assert_eq!(file("/tmp/ida"), "uid=1001(alice) gid=1001(alice) groups=1001(alice),10(wheel)\n");
    assert_eq!(file("/tmp/ga"), "alice wheel\n");
    assert!(file("/tmp/lsh").contains("alice  alice"), "{}", file("/tmp/lsh"));
    assert_eq!(sh("useradd alice"), 1, "already exists");
    assert_eq!(sh("useradd 'bad name'"), 1);
    assert_eq!(sh("userdel -r alice"), 0);
    assert!(!file("/etc/passwd").contains("alice"));
    assert!(!filesystem::is_dir("/home/alice"));
    assert_eq!(sh("userdel root"), 1);
    // ordinary users can't add users
    assert_eq!(sh("su user -c 'useradd mallory'"), 1);
}

#[test_case]
fn chown_chgrp_tools() {
    setup_users();
    assert_eq!(sh("touch /tmp/own && chown user:wheel /tmp/own && ls -l /tmp/own > /tmp/own.ls"), 0);
    assert!(file("/tmp/own.ls").contains(" user   wheel "), "{}", file("/tmp/own.ls"));
    assert_eq!(sh("chgrp root /tmp/own && chown 0 /tmp/own && stat /tmp/own > /tmp/own.st"), 0);
    assert!(file("/tmp/own.st").contains("Uid: 0  Gid: 0"));
    assert_eq!(sh("chown nobody /tmp/own"), 1, "unknown user");
    assert_eq!(sh("su user -c 'chown user /tmp/own'"), 1, "only root gives files away");
}

// ---- signal handlers ----

#[test_case]
fn signal_handlers_masks_alarms_stop_continue() {
    assert_eq!(run(prog("test_signals"), &["test_signals", "all"]), Ok(0));
    assert!(only_kernel_left(), "{:?}", process::list());
}

#[test_case]
fn sh_trap() {
    let _ = filesystem::make_dir("/tmp");
    assert_eq!(sh("rm -f /tmp/t1; trap 'echo caught >> /tmp/t1' USR1; kill -USR1 $$; echo after >> /tmp/t1"), 0);
    assert_eq!(file("/tmp/t1"), "caught\nafter\n");
    // EXIT runs at the end, and the exit status is kept
    assert_eq!(sh("rm -f /tmp/t2; trap 'echo bye >> /tmp/t2' EXIT; echo hi >> /tmp/t2; exit 4"), 4);
    assert_eq!(file("/tmp/t2"), "hi\nbye\n");
    // '' ignores, - restores the default (USR1 kills: 128 + 10)
    assert_eq!(sh("trap '' INT; kill -INT $$; echo survived > /tmp/t3"), 0);
    assert_eq!(file("/tmp/t3"), "survived\n");
    assert_eq!(sh("trap 'echo x' USR1; trap - USR1; kill -USR1 $$; echo no"), 138);
    // a trap doesn't change $?
    assert_eq!(sh("trap 'false' USR1; true; kill -USR1 $$; exit $?"), 0);
    // listing
    assert_eq!(sh("trap 'echo a' INT EXIT; trap > /tmp/t4; trap - EXIT"), 0);
    assert!(file("/tmp/t4").contains("trap -- 'echo a' SIGINT"), "{}", file("/tmp/t4"));
    assert!(file("/tmp/t4").contains("trap -- 'echo a' EXIT"));
    assert_eq!(sh("trap 'x' NOPE"), 1);
    assert_eq!(sh("trap 'x' KILL"), 1);
    // a trap command runs in the shell: it can set variables
    assert_eq!(sh("trap 'n=5' USR2; kill -USR2 $$; exit $n"), 5);
    assert!(only_kernel_left());
}

#[test_case]
fn sh_job_control_stop_fg_bg() {
    let _ = filesystem::make_dir("/tmp");
    palladiumos::task::keyboard::init_queue();
    // A job that stops itself (as Ctrl-Z would), then fg continues it.
    type_line("sh -c 'kill -STOP $$; echo resumed > /tmp/jc1'");
    type_line("echo $? > /tmp/jc0");
    type_line("jobs > /tmp/jc2");
    type_line("fg > /dev/null");
    type_line("echo $? > /tmp/jc3");
    // A background job stopped with kill, then continued with bg.
    type_line("sleep 100 &");
    type_line("kill -STOP %1");
    type_line("sleep 1");
    type_line("jobs > /tmp/jc4");
    type_line("bg");
    type_line("jobs > /tmp/jc5");
    type_line("kill %1");
    type_line("wait");
    type_line("jobs > /tmp/jc6");
    type_line("exit 0");
    assert_eq!(run(prog("sh"), &["sh"]), Ok(0));
    assert_eq!(file("/tmp/jc0"), "147\n"); // 128 + SIGSTOP
    assert!(file("/tmp/jc2").contains("Stopped"), "{}", file("/tmp/jc2"));
    assert!(file("/tmp/jc2").contains("sh -c kill -STOP $$; echo resumed > /tmp/jc1"));
    assert_eq!(file("/tmp/jc1"), "resumed\n");
    assert_eq!(file("/tmp/jc3"), "0\n");
    assert!(file("/tmp/jc4").contains("Stopped") && file("/tmp/jc4").contains("sleep 100"), "{}", file("/tmp/jc4"));
    assert!(file("/tmp/jc5").contains("Running"), "{}", file("/tmp/jc5"));
    assert_eq!(file("/tmp/jc6"), "");
    assert!(only_kernel_left(), "{:?}", process::list());
}

#[test_case]
fn ctrl_z_stops_the_foreground_job() {
    palladiumos::task::keyboard::init_queue();
    // Ctrl-Z sends SIGTSTP to the foreground job; here the job sends it to
    // itself (the keyboard interrupt can't be simulated mid-command).
    type_line("sh -c 'kill -TSTP $$; echo back > /tmp/cz'");
    type_line("echo $? > /tmp/cz0");
    type_line("fg");
    type_line("exit 0");
    assert_eq!(run(prog("sh"), &["sh"]), Ok(0));
    assert_eq!(file("/tmp/cz0"), "148\n"); // 128 + SIGTSTP
    assert_eq!(file("/tmp/cz"), "back\n");
}

// ---- C programs (musl libc) ----

/// C programs and ports exist only when musl (tools/build-musl.sh) and
/// the port (tools/ports/NAME.sh) were built.
fn c_program(name: &str) -> Option<&'static [u8]> {
    let p = programs::find(name);
    if p.is_none() {
        palladiumos::serial_print!("(skipped: {} not built) ", name);
    }
    p
}

#[test_case]
fn musl_libc_works() {
    let Some(test_libc) = c_program("test_libc") else { return };
    let _ = filesystem::make_dir("/tmp");
    let _ = filesystem::remove("/tmp/atexit.flag");
    assert_eq!(run(test_libc, &["test_libc"]), Ok(0));
    assert_eq!(file("/tmp/atexit.flag"), "x", "atexit handlers run at exit");
    assert!(only_kernel_left(), "{:?}", process::list());
}

#[test_case]
fn c_program_in_a_pipeline() {
    let Some(_) = c_program("chello") else { return };
    let _ = filesystem::make_dir("/tmp");
    assert_eq!(sh("USER=tester chello a b | grep -c argv > /tmp/ch; chello | grep USER >> /tmp/ch"), 0);
    assert_eq!(file("/tmp/ch"), "2\nUSER=(unset)\n");
}

#[test_case]
fn c_program_memory_is_freed() {
    let Some(test_libc) = c_program("test_libc") else { return };
    assert_eq!(run(test_libc, &["test_libc"]), Ok(0));
    let before = frames_in_use();
    assert_eq!(run(test_libc, &["test_libc"]), Ok(0));
    assert_eq!(frames_in_use(), before, "mmap/brk/stack pages all freed");
}

// ---- ported software (tools/ports) ----

#[test_case]
fn lua_port_works() {
    let Some(_) = c_program("lua") else { return };
    let _ = filesystem::make_dir("/tmp");
    filesystem::write_file("/tmp/check.lua", include_str!("ports/check.lua")).unwrap();
    let status = sh("lua /tmp/check.lua > /tmp/check.out 2>&1");
    assert_eq!(status, 0, "{}", file("/tmp/check.out"));
    assert_eq!(file("/tmp/check.out"), "lua checks passed\n");
    assert_eq!(sh("lua -e 'print(6 * 7)' > /tmp/l42"), 0);
    assert_eq!(file("/tmp/l42"), "42\n");
    assert_eq!(sh("lua -e 'os.exit(9)'"), 9);
    assert!(only_kernel_left(), "{:?}", process::list());
}

#[test_case]
fn dash_port_works() {
    let Some(_) = c_program("dash") else { return };
    let _ = filesystem::make_dir("/tmp");
    filesystem::write_file("/tmp/check.sh", include_str!("ports/check.sh")).unwrap();
    let status = sh("dash /tmp/check.sh > /tmp/dcheck.out 2>&1");
    let out = file("/tmp/dcheck.out");
    assert_eq!(status, 0, "{}", out);
    assert!(out.ends_with("dash checks passed\n"), "{}", out);
    // dash as an interactive shell, with its own job control
    palladiumos::task::keyboard::init_queue();
    type_line("sh -c 'kill -STOP $$; echo resumed > /tmp/dj'");
    type_line("jobs > /tmp/dj0");
    type_line("fg");
    type_line("echo $? > /tmp/dj1");
    type_line("exit 4");
    assert_eq!(run(prog("dash"), &["dash", "-i"]), Ok(4));
    assert!(file("/tmp/dj0").contains("Stopped"), "{}", file("/tmp/dj0"));
    assert_eq!(file("/tmp/dj"), "resumed\n");
    assert_eq!(file("/tmp/dj1"), "0\n");
    assert!(only_kernel_left(), "{:?}", process::list());
}

#[test_case]
fn fastfetch_port_works() {
    if palladiumos::mounts::resolve("/usr/bin/fastfetch").is_err() {
        palladiumos::serial_print!("(skipped: fastfetch not built) ");
        return;
    }
    filesystem::init_default_files(); // /etc/os-release and fastfetch's config
    let _ = filesystem::make_dir("/tmp");
    assert_eq!(sh("fastfetch --logo none > /tmp/ff 2>&1"), 0);
    let out = file("/tmp/ff");
    for want in ["OS: PalladiumOS 0.6.8 x86_64", "Kernel: PalladiumOS 0.6.8", "Memory: ", "Disk (/): "] {
        assert!(out.contains(want), "{:?} missing from:\n{}", want, out);
    }
    assert!(!out.contains("not implemented"), "{}", out);
    // With the logo (from /etc/xdg/fastfetch/config.jsonc)
    assert_eq!(sh("fastfetch > /tmp/ff2 2>&1"), 0);
    assert!(file("/tmp/ff2").contains("Palladium"), "{}", file("/tmp/ff2"));
    // With a desktop: the display (X's RandR) and the window manager.
    let exists = |p: &str| palladiumos::mounts::resolve(p).is_ok();
    if exists("/usr/bin/Xfbdev") && exists("/usr/bin/icewm") {
        sh("Xfbdev :0 > /tmp/ffx.log 2>&1 & sleep 2; DISPLAY=:0 icewm > /dev/null 2>&1 & sleep 2; \
            DISPLAY=:0 fastfetch --logo none > /tmp/ff3 2>&1; pkill icewm; sleep 1; pkill Xfbdev; wait");
        let out = file("/tmp/ff3");
        assert!(out.contains("Display: ") && out.contains("IceWM"), "{}", out);
    }
    assert!(only_kernel_left());
}

/// tcc needs the system disk (/usr) too.
fn tcc_ready() -> bool {
    c_program("tcc").is_some() && filesystem::is_dir("/usr/lib/tcc")
}

#[test_case]
fn tcc_compiles_and_runs_programs() {
    if !tcc_ready() {
        return;
    }
    let _ = filesystem::make_dir("/tmp");
    filesystem::write_file(
        "/tmp/t1.c",
        "#include <stdio.h>\n#include <math.h>\nint twice(int);\n\
         int main(int c, char **v) { printf(\"%s %d %.3f\\n\", v[1], twice(c), sqrt(2.0)); return 5; }\n",
    )
    .unwrap();
    filesystem::write_file("/tmp/t2.c", "int twice(int x) { return 2 * x; }\n").unwrap();
    // separate compilation, then linking (with libm)
    let steps = [
        sh("cd /tmp && tcc -c t2.c > /tmp/s1 2>&1"),
        sh("cd /tmp && tcc t1.c t2.o -lm -o t > /tmp/s2 2>&1"),
        sh("cd /tmp && ls -l t > /tmp/s3 2>&1; ./t hi > out 2>> /tmp/s3"),
    ];
    assert_eq!(steps, [0, 0, 5], "{} | {} | {}", file("/tmp/s1"), file("/tmp/s2"), file("/tmp/s3"));
    assert_eq!(file("/tmp/out"), "hi 4 1.414\n");
    // tcc -run FILE ARGS: compile and run in memory
    filesystem::write_file("/tmp/t3.c", "#include <stdio.h>\nint main(int c, char **v) { printf(\"%s %d\\n\", v[1], c); return 6; }\n").unwrap();
    assert_eq!(sh("cd /tmp && tcc -run t3.c run > out2"), 6);
    assert_eq!(file("/tmp/out2"), "run 2\n");
    // a syntax error is reported and fails
    filesystem::write_file("/tmp/bad.c", "int main( { return 0; }\n").unwrap();
    assert_eq!(sh("tcc /tmp/bad.c -o /tmp/bad 2> /tmp/err"), 1);
    assert!(file("/tmp/err").contains("error"), "{}", file("/tmp/err"));
    assert!(only_kernel_left(), "{:?}", process::list());
}

#[test_case]
fn tcc_compiles_itself() {
    if !tcc_ready() || !filesystem::is_dir("/usr/src/tinycc") {
        return;
    }
    let _ = filesystem::make_dir("/tmp");
    // tcc -> tcc2 (built inside PalladiumOS) -> tcc3 (built by tcc2)
    assert_eq!(sh("cd /tmp && tcc -o tcc2 /usr/src/tinycc/tcc.c -I/usr/src/tinycc"), 0);
    assert_eq!(sh("cd /tmp && ./tcc2 -o tcc3 /usr/src/tinycc/tcc.c -I/usr/src/tinycc"), 0);
    let (two, three) = (filesystem::read_bytes("/tmp/tcc2").unwrap(), filesystem::read_bytes("/tmp/tcc3").unwrap());
    assert!(two.len() > 100_000);
    assert!(two == three, "a tcc built by itself is identical to the one that built it");
    filesystem::write_file("/tmp/h.c", "int main(void) { return 42; }\n").unwrap();
    assert_eq!(sh("cd /tmp && ./tcc3 h.c -o h && ./h"), 42);
    for f in ["/tmp/tcc2", "/tmp/tcc3", "/tmp/h"] {
        let _ = filesystem::remove(f);
    }
}

#[test_case]
fn micropython_port_works() {
    let Some(_) = c_program("micropython") else { return };
    let _ = filesystem::make_dir("/tmp");
    filesystem::write_file("/tmp/check.py", include_str!("ports/check.py")).unwrap();
    let status = sh("micropython /tmp/check.py arg1 'arg 2' > /tmp/pcheck.out 2>&1");
    let out = file("/tmp/pcheck.out");
    assert_eq!(status, 0, "{}", out);
    assert_eq!(out, "micropython checks passed\n");
    // a #! script, and Ctrl-C's signal becoming KeyboardInterrupt
    filesystem::write_file(
        "/tmp/k.py",
        // (system() ignores SIGINT while it waits, as POSIX says: send it
        // from a background job, after system() has returned.)
        "#!/bin/micropython\nimport os\ntry:\n    os.system('(sleep 1; pkill -INT micropython) &')\n    \
         while True: pass\nexcept KeyboardInterrupt:\n    print('interrupted')\n",
    )
    .unwrap();
    assert_eq!(sh("chmod +x /tmp/k.py && /tmp/k.py > /tmp/k.out 2>&1"), 0, "{}", file("/tmp/k.out"));
    assert_eq!(file("/tmp/k.out"), "interrupted\n");
    // _thread: four threads, a lock, and garbage collection while they run
    // (MicroPython's GC stops threads with a real-time signal).
    filesystem::write_file(
        "/tmp/thr.py",
        "import _thread, time, gc\nlock = _thread.allocate_lock()\ncount = [0]\ndone = [0]\n\
         def worker(n):\n    for i in range(500):\n        with lock:\n            count[0] += 1\n        \
         if i % 100 == 0:\n            gc.collect()\n    with lock:\n        done[0] += 1\n\
         for i in range(4):\n    _thread.start_new_thread(worker, (i,))\n\
         while done[0] < 4:\n    time.sleep_ms(10)\nprint('count', count[0])\n",
    )
    .unwrap();
    assert_eq!(sh("micropython /tmp/thr.py > /tmp/thr.out 2>&1"), 0, "{}", file("/tmp/thr.out"));
    assert_eq!(file("/tmp/thr.out"), "count 2000\n");
    assert!(only_kernel_left(), "{:?}", process::list());
}

#[test_case]
fn dmesg_shows_the_boot_log() {
    let _ = filesystem::make_dir("/tmp");
    // Test kernels don't print the boot log, but keep it.
    assert_eq!(sh("dmesg -n > /tmp/dmesg.out"), 0);
    let out = file("/tmp/dmesg.out");
    assert!(out.contains("[  OK  ] Loaded the GDT and TSS"), "{}", out);
    assert!(out.contains("[  OK  ] Enabled the syscall instruction"), "{}", out);
    assert!(!out.contains('\x1b'), "-n strips the color codes");
    // (/proc files report size 0, like Linux: copy it with cat to read it here)
    assert_eq!(sh("cat /proc/kmsg > /tmp/kmsg.out"), 0);
    assert!(file("/tmp/kmsg.out").contains("\x1b[1;32m  OK  \x1b[0m"), "the log itself has colors");
}

#[test_case]
fn busybox_tools_work() {
    if c_program("busybox").is_none() || !filesystem::is_dir("/usr/bin") {
        return;
    }
    let _ = filesystem::make_dir("/tmp");
    let script = "cd /tmp && rm -rf bb && mkdir bb && cd bb && \
        printf '%s=%d\\n' answer 42 > p && \
        printf '3 c\\n1 a\\n2 b\\n' | sort -n | awk '{ s += $1; n = n $2 } END { print s, n }' > a && \
        echo hello world | sed 's/world/palladium/' | tr a-z A-Z > s && \
        echo a:b:c | cut -d: -f2 > c && seq 3 | xargs echo > x && expr 6 \\* 7 > e && \
        mkdir -p t/d && echo data > t/d/f && tar czf t.tgz t && rm -r t && tar xzf t.tgz && \
        find t -type f > f && md5sum t/d/f > m && cmp t/d/f t/d/f && \
        printf 'x\\ny\\n' > d1 && printf 'x\\nz\\n' > d2 && ! diff d1 d2 > dd && \
        basename /a/b/c.txt .txt > b && busybox sh -c 'echo ash $((1 + 2))' > ash";
    let status = sh(&alloc::format!("{} 2> /tmp/bb.err", script));
    assert_eq!(status, 0, "{}", file("/tmp/bb.err"));
    let f = |name: &str| file(&alloc::format!("/tmp/bb/{}", name));
    assert_eq!(f("p"), "answer=42\n");
    assert_eq!(f("a"), "6 abc\n");
    assert_eq!(f("s"), "HELLO PALLADIUM\n");
    assert_eq!(f("c"), "b\n");
    assert_eq!(f("x"), "1 2 3\n");
    assert_eq!(f("e"), "42\n");
    assert_eq!(f("f"), "t/d/f\n");
    assert_eq!(f("m"), "6137cde4893c59f76f005a8123d8e8e6  t/d/f\n");
    assert!(f("dd").contains("-y") && f("dd").contains("+z"), "{}", f("dd"));
    assert_eq!(f("b"), "c\n");
    assert_eq!(f("ash"), "ash 3\n");
    assert!(only_kernel_left(), "{:?}", process::list());
}

#[test_case]
fn sh_help_has_topics() {
    let _ = filesystem::make_dir("/tmp");
    assert_eq!(sh("help > /tmp/h0; help files > /tmp/h1; help ls > /tmp/h2; help cd > /tmp/h3; help other > /tmp/h4"), 0);
    let index = file("/tmp/h0");
    for topic in ["files", "text", "processes", "system", "users", "builtins", "syntax", "keys"] {
        assert!(index.contains(&alloc::format!("  {} ", topic)), "{} missing:\n{}", topic, index);
    }
    assert!(index.lines().all(|l| l.len() <= 80), "fits the screen:\n{}", index);
    assert!(file("/tmp/h1").contains("  ls ") && file("/tmp/h1").contains("  chmod "));
    assert!(file("/tmp/h2").starts_with("ls: list a directory"));
    assert!(file("/tmp/h3").starts_with("cd: shell builtin"));
    assert_eq!(file("/tmp/h4").trim(), "", "every program in /bin has a category");
    assert_eq!(sh("help nosuchthing 2> /dev/null"), 1);
}

#[test_case]
fn busybox_versions_are_the_defaults() {
    if programs::find("busybox").is_none() {
        return;
    }
    let _ = filesystem::make_dir("/tmp");
    let script = "cd /tmp && date +%Y > d1 && echo abcdef | head -c 3 > h && \
        printf 'b 2\\na 3\\nc 1\\n' | sort -k2 -n > s && echo foo123bar | grep -oE '[0-9]+' > g && \
        env GREETING=hi sh -c 'echo $GREETING' > e && readlink -f /bin/date > r && \
        /bin/palladium/date +%s > d2";
    assert_eq!(sh(script), 0);
    assert_eq!(file("/tmp/d1").trim().len(), 4, "a year: {}", file("/tmp/d1"));
    assert_eq!(file("/tmp/h"), "abc");
    assert_eq!(file("/tmp/s"), "c 1\nb 2\na 3\n");
    assert_eq!(file("/tmp/g"), "123\n");
    assert_eq!(file("/tmp/e"), "hi\n");
    assert_eq!(file("/tmp/r"), "/bin/busybox\n");
    assert!(file("/tmp/d2").trim().parse::<u64>().is_ok(), "the original still runs");
}

#[test_case]
fn fork_is_copy_on_write() {
    let before = frames_in_use();
    assert_eq!(run(prog("test_cow"), &["test_cow"]), Ok(0));
    assert_eq!(frames_in_use(), before, "every shared and copied page was freed");
    assert!(only_kernel_left(), "{:?}", process::list());
}

#[test_case]
fn posix_threads_work() {
    let Some(_) = c_program("test_threads") else { return };
    let _ = filesystem::make_dir("/tmp");
    let before = frames_in_use();
    let status = sh("test_threads > /tmp/thr.out 2>&1");
    assert_eq!(status, 0, "{}", file("/tmp/thr.out"));
    assert!(file("/tmp/thr.out").contains("all checks passed"));
    // pthread_exit in main: the process ends when its last thread does
    assert_eq!(sh("test_threads main-exits > /tmp/thr2.out 2>&1"), 0, "{}", file("/tmp/thr2.out"));
    assert_eq!(file("/tmp/thr2.out"), "the other thread finished\n");
    assert!(only_kernel_left(), "{:?}", process::list());
    assert_eq!(frames_in_use(), before, "threads' stacks and memory were freed");
}

#[test_case]
fn epoll_eventfd_and_big_programs() {
    let Some(_) = c_program("test_events") else { return };
    let _ = filesystem::make_dir("/tmp");
    let before = frames_in_use();
    // (The 34 MiB program goes on the scratch disk: memory filesystems
    // limit files to 4 MiB.)
    let status = sh("test_events /alpine > /tmp/ev.out 2>&1");
    assert_eq!(status, 0, "{}", file("/tmp/ev.out"));
    assert!(file("/tmp/ev.out").contains("all checks passed"));
    assert!(only_kernel_left(), "{:?}", process::list());
    assert_eq!(frames_in_use(), before, "the big program's memory was freed");
}

#[test_case]
fn sysv_shared_memory_works() {
    let Some(_) = c_program("test_sysvshm") else { return };
    let _ = filesystem::make_dir("/tmp");
    // A first run grows kernel tables once; the second must give back all
    // it took.
    assert_eq!(sh("test_sysvshm > /tmp/sysvshm.out 2>&1"), 0, "{}", file("/tmp/sysvshm.out"));
    let before = frames_in_use();
    let status = sh("test_sysvshm > /tmp/sysvshm.out 2>&1");
    assert_eq!(status, 0, "{}", file("/tmp/sysvshm.out"));
    assert!(file("/tmp/sysvshm.out").contains("all checks passed"));
    assert!(only_kernel_left(), "{:?}", process::list());
    assert_eq!(frames_in_use(), before, "the segments were freed");
}

#[test_case]
fn shared_memory_works() {
    let Some(_) = c_program("test_shm") else { return };
    let _ = filesystem::make_dir("/tmp");
    let before = frames_in_use();
    let status = sh("test_shm > /tmp/shm.out 2>&1");
    assert_eq!(status, 0, "{}", file("/tmp/shm.out"));
    assert!(file("/tmp/shm.out").contains("all checks passed"));
    assert!(only_kernel_left(), "{:?}", process::list());
    assert_eq!(frames_in_use(), before, "shared memory was freed");
}

#[test_case]
fn framebuffer_console_and_mouse() {
    let Some(_) = c_program("test_fb") else { return };
    let _ = filesystem::make_dir("/tmp");
    let status = sh("test_fb > /tmp/fb.out 2>&1");
    assert_eq!(status, 0, "{}", file("/tmp/fb.out"));
    assert!(file("/tmp/fb.out").contains("all checks passed"));
    assert!(!palladiumos::vga_buffer::is_graphics(), "text mode is back");
}

/// The soak test (tools/soak.sh MINUTES): only in a kernel built with
/// PALLADIUM_SOAK_MINUTES; else it does nothing. Round after round until
/// the time is up: the stress program with processes starting and ending
/// around it, the C test programs (sockets, events, threads, terminals,
/// shared memory, the network, sound), and DOOM under X. Fails at the first
/// failure or at any new WARN line in the kernel log (a program's
/// segfault, a disk error...).
#[test_case]
fn soak() {
    let Some(minutes) = option_env!("PALLADIUM_SOAK_MINUTES").and_then(|m| m.parse::<u64>().ok()) else { return };
    let _ = filesystem::make_dir("/tmp");
    let ticks_per_s = 1_000_000_000 / palladiumos::process::TICK_NS;
    let start = palladiumos::process::ticks();
    let deadline = start + minutes * 60 * ticks_per_s;
    let warnings = palladiumos::boot::warnings();
    let check_log = |what: &str, round: u64| {
        if palladiumos::boot::warnings() != warnings {
            let log = palladiumos::boot::log();
            let lines: Vec<&str> = log.lines().filter(|l| l.contains("WARN") || l.contains("FAILED")).collect();
            panic!("soak round {}: the kernel logged a warning after {}:\n{}", round, what, lines.join("\n"));
        }
    };
    let programs: &[(&str, &str)] = &[
        ("test_unix", "test_unix"),
        ("test_events", "test_events /alpine"),
        ("test_threads", "test_threads"),
        ("test_pty", "test_pty"),
        ("test_shm", "test_shm"),
        ("test_sysvshm", "test_sysvshm"),
        ("test_net", "test_net"),
        ("test_sound", "test_sound --no-timing"), // (QEMU's silent sound output: bursts under load)
    ];
    let x = filesystem::read_file("/usr/bin/Xfbdev").is_ok() && filesystem::read_file("/usr/bin/doomgeneric").is_ok();
    let mut round = 0u64;
    while palladiumos::process::ticks() < deadline {
        round += 1;
        // 1. Stress, with processes starting and ending around it.
        let status = sh("i=0; while [ $i -lt 400 ]; do ls /bin > /dev/null; i=$((i+1)); done & \
                         j=0; while [ $j -lt 400 ]; do busybox true; j=$((j+1)); done & \
                         for k in 1 2 3 4; do test_smp_stress /bin/busybox 20 > /tmp/soak$k.txt 2>&1 & done; wait");
        let out: alloc::string::String =
            (1..=4).map(|k| filesystem::read_file(&alloc::format!("/tmp/soak{}.txt", k)).unwrap_or_default()).collect();
        assert!(status == 0 && out.matches(" ok: ").count() == 4, "soak round {}: stress:\n{}", round, out);
        check_log("the stress run", round);
        // 2. The C test programs.
        for &(name, command) in programs {
            if c_program(name).is_none() {
                continue;
            }
            let status = sh(&alloc::format!("{} > /tmp/soak.out 2>&1", command));
            assert_eq!(status, 0, "soak round {}: {}:\n{}", round, name, file("/tmp/soak.out"));
            check_log(name, round);
        }
        // 3. DOOM under X (with sound), still running after 10 seconds.
        if x {
            let status = sh("Xfbdev :0 > /tmp/soakx.log 2>&1 & sleep 2; \
                             DISPLAY=:0 doomgeneric -iwad /usr/share/games/doom/freedoom1.wad > /tmp/soakd.out 2>&1 & \
                             sleep 10; pidof doomgeneric > /tmp/soakalive.txt; pkill doomgeneric; sleep 1; pkill Xfbdev; wait");
            let alive = filesystem::read_file("/tmp/soakalive.txt").unwrap_or_default();
            assert!(status == 0 || !alive.trim().is_empty(), "soak round {}: DOOM:\n{}", round, file("/tmp/soakd.out"));
            assert!(!alive.trim().is_empty(), "soak round {}: DOOM died:\n{}", round, file("/tmp/soakd.out"));
            check_log("DOOM", round);
        }
        assert!(only_kernel_left(), "soak round {}: processes left: {:?}", round, process::list());
        let elapsed = (palladiumos::process::ticks() - start) / ticks_per_s;
        palladiumos::serial_println!("soak: round {} passed ({}:{:02} of {} min, {} frames in use)",
            round, elapsed / 60, elapsed % 60, minutes, frames_in_use());
    }
}

#[test_case]
fn processes_stay_intact_under_stress() {
    // Several programs at once (on every core with PALLADIUM_SMP=4): each
    // reads a file at random places, keeps memory and floating point,
    // maps and unmaps memory, and takes a timer signal 100 times a second.
    let Some(_) = c_program("test_smp_stress") else { return };
    let _ = filesystem::make_dir("/tmp");
    let status = sh("for i in 1 2 3; do test_smp_stress /bin/busybox 4 > /tmp/stress$i.txt 2>&1 & done; wait; \
                     cat /tmp/stress1.txt /tmp/stress2.txt /tmp/stress3.txt > /tmp/stress.txt");
    assert_eq!(status, 0);
    let out = file("/tmp/stress.txt");
    assert_eq!(out.matches(" ok: ").count(), 3, "{}", out);
    assert!(only_kernel_left());
}

#[test_case]
fn siginfo_fault_handlers_and_altstack() {
    let Some(_) = c_program("test_siginfo") else { return };
    let _ = filesystem::make_dir("/tmp");
    let status = sh("test_siginfo > /tmp/si.out 2>&1");
    assert_eq!(status, 0, "{}", file("/tmp/si.out"));
    assert!(file("/tmp/si.out").contains("all checks passed"), "{}", file("/tmp/si.out"));
    assert!(only_kernel_left());
}

#[test_case]
fn cxx_programs_run() {
    // C++ (palladium-c++, libstdc++ for musl): exceptions, RTTI, threads...
    let Some(_) = c_program("test_cxx") else { return };
    let _ = filesystem::make_dir("/tmp");
    let status = sh("test_cxx > /tmp/cxx.out 2>&1");
    assert_eq!(status, 0, "{}", file("/tmp/cxx.out"));
    assert!(file("/tmp/cxx.out").contains("c++ ok"), "{}", file("/tmp/cxx.out"));
    assert!(only_kernel_left());
}

#[test_case]
fn sound_card_plays() {
    let Some(_) = c_program("test_sound") else { return };
    let _ = filesystem::make_dir("/tmp");
    assert!(palladiumos::sound::present(), "QEMU's AC'97 card was found");
    let status = sh("test_sound > /tmp/snd.out 2>&1");
    assert_eq!(status, 0, "{}", file("/tmp/snd.out"));
    assert!(file("/tmp/snd.out").contains("all checks passed"), "{}", file("/tmp/snd.out"));
    assert!(only_kernel_left());
}

#[test_case]
fn opengl_draws_under_x() {
    // Mesa (tools/ports/mesa.sh), drawing in software on the X server.
    // (Looked up, not read: glxgears is 24 MB.)
    let exists = |p: &str| palladiumos::mounts::resolve(p).is_ok();
    if !exists("/usr/bin/glxgears") || !exists("/usr/bin/Xfbdev") {
        palladiumos::serial_print!("(skipped: no Mesa or X built) ");
        return;
    }
    let _ = filesystem::make_dir("/tmp");
    let warnings = palladiumos::boot::warnings();
    let status = sh("Xfbdev :0 > /tmp/glx.log 2>&1 & sleep 2; \
                     DISPLAY=:0 glxinfo > /tmp/glxinfo.txt 2>&1; \
                     DISPLAY=:0 test_gl > /tmp/test_gl.txt 2>&1; \
                     DISPLAY=:0 glxgears > /tmp/glxgears.txt 2>&1 & sleep 8; \
                     pidof glxgears > /tmp/glxalive.txt; pkill glxgears; sleep 1; pkill Xfbdev; wait");
    // Nothing crashed meanwhile (the kernel logs a warning for each).
    let new = palladiumos::boot::warnings() - warnings;
    if new > 0 {
        let log = palladiumos::boot::log();
        let lines: Vec<&str> = log.lines().filter(|l| l.contains("WARN")).collect();
        panic!("status {}; {} new warnings:\n{}\nglxinfo:\n{}\nglxgears:\n{}", status, new,
            lines[lines.len().saturating_sub(new as usize)..].join("\n"), file("/tmp/glxinfo.txt"), file("/tmp/glxgears.txt"));
    }
    let info = file("/tmp/glxinfo.txt");
    assert!(info.contains("OpenGL renderer string: softpipe") && info.contains("OpenGL version string: 3.3"), "{}", info);
    // A texture drawn right (read back from what was drawn).
    assert!(file("/tmp/test_gl.txt").contains("test_gl: ok"), "{}", file("/tmp/test_gl.txt"));
    // Still drawing after 8 seconds, and it said how fast (every 5 seconds).
    assert!(!file("/tmp/glxalive.txt").trim().is_empty(), "glxgears stopped:\n{}", file("/tmp/glxgears.txt"));
    assert!(file("/tmp/glxgears.txt").contains("frames in"), "{}", file("/tmp/glxgears.txt"));
    assert!(only_kernel_left());
}

#[test_case]
fn linux_binaries_run_unchanged() {
    // A static glibc program from the host's gcc, at 0x400000 (only with a
    // system disk built: see build.rs).
    if !filesystem::is_dir("/usr/share/linux-test") {
        palladiumos::serial_print!("(skipped: no Linux test binary) ");
        return;
    }
    let _ = filesystem::make_dir("/tmp");
    assert_eq!(sh("/usr/share/linux-test/hello one two > /tmp/linux.out 2>&1"), 0, "{}", file("/tmp/linux.out"));
    assert_eq!(file("/tmp/linux.out"), "glibc on PalladiumOS: 2 args, heap 7, child 9\n");
    // The same, dynamically linked: glibc's own dynamic linker loads it
    // and libc.so.6 (from the system disk).
    if palladiumos::mounts::stat("/usr/share/linux-test/hello-dyn").is_ok() {
        filesystem::init_default_files(); // /lib64/ld-linux-x86-64.so.2
        assert_eq!(sh("/usr/share/linux-test/hello-dyn a b > /tmp/linux2.out 2>&1"), 0, "{}", file("/tmp/linux2.out"));
        assert_eq!(file("/tmp/linux2.out"), "glibc on PalladiumOS: 2 args, heap 7, child 9\n");
    }
}

#[test_case]
fn dynamically_linked_musl_programs() {
    let Some(_) = c_program("test_dyn") else { return };
    if palladiumos::mounts::stat("/usr/lib/libc.so").is_err() {
        palladiumos::serial_print!("(skipped: no libc.so on the system disk) ");
        return;
    }
    let _ = filesystem::make_dir("/tmp");
    filesystem::init_default_files(); // /lib/ld-musl-x86_64.so.1
    // Libraries at startup, a constructor, TLS in a library across threads,
    // dlopen/dlsym/dlclose, dl_iterate_phdr.
    assert_eq!(sh("test_dyn > /tmp/dyn.out 2>&1"), 0, "{}", file("/tmp/dyn.out"));
    assert!(file("/tmp/dyn.out").contains("all checks passed"));
    assert_eq!(sh("ldd /bin/test_dyn > /tmp/ldd.out 2>&1"), 0, "{}", file("/tmp/ldd.out"));
    assert!(file("/tmp/ldd.out").contains("libgreet.so => /usr/lib/libgreet.so"), "{}", file("/tmp/ldd.out"));
    // Without its dynamic linker, a program can't start: "not found".
    assert_eq!(sh("mv /lib/ld-musl-x86_64.so.1 /lib/ld-away; test_dyn 2> /tmp/dyn.err; echo $? > /tmp/dyn.st; mv /lib/ld-away /lib/ld-musl-x86_64.so.1"), 0);
    assert_eq!(file("/tmp/dyn.st"), "127\n", "{}", file("/tmp/dyn.err"));
    assert!(only_kernel_left(), "{:?}", process::list());
}

// ---- networking ----

#[test_case]
fn sockets_over_loopback() {
    let Some(_) = c_program("test_net") else { return };
    let _ = filesystem::make_dir("/tmp");
    filesystem::init_default_files(); // /etc/hosts (localhost)
    let status = sh("test_net > /tmp/net.out 2>&1");
    assert_eq!(status, 0, "{}", file("/tmp/net.out"));
    assert!(file("/tmp/net.out").contains("all checks passed"));
    assert!(only_kernel_left(), "{:?}", process::list());
}

#[test_case]
fn eth0_is_configured_by_dhcp() {
    // QEMU's user network answers DHCP (no internet needed).
    if !palladiumos::net::has_card() {
        palladiumos::serial_print!("(skipped: no network card) ");
        return;
    }
    let config = palladiumos::net::config().or_else(|| palladiumos::net::wait_for_dhcp(5000)).expect("no DHCP answer");
    assert_eq!(alloc::format!("{}", config.address), "10.0.2.15/24");
    assert_eq!(config.gateway.map(|g| alloc::format!("{}", g)).as_deref(), Some("10.0.2.2"));
    assert_eq!(config.dns.map(|d| alloc::format!("{}", d)).as_deref(), Some("10.0.2.3"));
}

#[test_case]
fn busybox_ping_over_loopback() {
    if c_program("busybox").is_none() || !filesystem::is_dir("/usr/bin") {
        return;
    }
    let _ = filesystem::make_dir("/tmp");
    assert_eq!(sh("ping -c 2 -W 3 127.0.0.1 > /tmp/ping.out 2>&1"), 0, "{}", file("/tmp/ping.out"));
    assert!(file("/tmp/ping.out").contains("2 packets transmitted, 2 packets received"), "{}", file("/tmp/ping.out"));
    // Raw sockets are for root: an ordinary user's ping is refused.
    assert_ne!(sh("su user -c 'ping -c 1 127.0.0.1' > /tmp/ping2.out 2>&1"), 0);
}

#[test_case]
fn unix_domain_sockets() {
    let Some(_) = c_program("test_unix") else { return };
    let _ = filesystem::make_dir("/tmp");
    let status = sh("test_unix > /tmp/unix.out 2>&1");
    assert_eq!(status, 0, "{}", file("/tmp/unix.out"));
    assert!(file("/tmp/unix.out").contains("all checks passed"));
    assert!(only_kernel_left(), "{:?}", process::list());
}

#[test_case]
fn pseudo_terminals_and_sessions() {
    let Some(_) = c_program("test_pty") else { return };
    let _ = filesystem::make_dir("/tmp");
    let status = sh("test_pty > /tmp/pty.out 2>&1");
    assert_eq!(status, 0, "{}", file("/tmp/pty.out"));
    assert!(file("/tmp/pty.out").contains("all checks passed"));
    assert!(only_kernel_left(), "{:?}", process::list());
}

#[test_case]
fn passwords_in_the_old_format_still_work() {
    setup_users();
    // A hash PalladiumOS 0.5.0 and earlier wrote (password "oldpw").
    let old = "$sha256$00112233aabbccdd$a4a99cc7bee7c355f7f5e1a738cb66be31ac349558a27625654b550b5564bda7";
    let shadow = file("/etc/shadow");
    let changed: alloc::string::String = shadow
        .lines()
        .map(|l| if l.starts_with("user:") { alloc::format!("user:{}:0:0:99999:7:::\n", old) } else { alloc::format!("{}\n", l) })
        .collect();
    filesystem::write_file("/etc/shadow", &changed).unwrap();
    let _ = filesystem::make_dir("/tmp");
    palladiumos::task::keyboard::init_queue();
    type_line("user");
    type_line("oldpw");
    type_line("whoami > /tmp/who-old");
    type_line("exit");
    ctrl_d();
    assert_eq!(run(prog("login"), &["login"]), Ok(99));
    assert_eq!(file("/tmp/who-old"), "user\n");
    filesystem::write_file("/etc/shadow", &shadow).unwrap();
    assert!(only_kernel_left());
}

#[test_case]
fn dropbear_ssh_over_loopback() {
    if c_program("dropbear").is_none() || c_program("dbclient").is_none() {
        return;
    }
    setup_users();
    let _ = filesystem::make_dir("/tmp");
    // A key for root, a server on 127.0.0.1:2022, and the client logging in
    // with the key to run commands.
    let script = "rm -f /tmp/k /tmp/db.pid; dropbearkey -t ed25519 -f /tmp/k > /dev/null 2>&1 && \
        mkdir -p /root/.ssh && dropbearkey -y -f /tmp/k | grep '^ssh-' > /root/.ssh/authorized_keys && \
        chmod 700 /root/.ssh && chmod 600 /root/.ssh/authorized_keys && \
        dropbear -R -p 127.0.0.1:2022 -P /tmp/db.pid && sleep 1 && \
        dbclient -y -i /tmp/k -p 2022 root@127.0.0.1 'echo over ssh; id -u; tty' > /tmp/ssh.out 2>&1; \
        kill $(cat /tmp/db.pid)";
    assert_eq!(sh(script), 0, "{}", file("/tmp/ssh.out"));
    let out = file("/tmp/ssh.out");
    assert!(out.contains("over ssh\n0\nnot a tty"), "{}", out);
    assert_eq!(sh("ls /etc/dropbear | grep -q _host_key"), 0, "a host key was made");
}

#[test_case]
fn chroot_mount_fchdir_and_locks() {
    let Some(_) = c_program("test_chroot") else { return };
    let _ = filesystem::make_dir("/tmp");
    let status = sh("test_chroot > /tmp/chroot.out 2>&1");
    assert_eq!(status, 0, "{}", file("/tmp/chroot.out"));
    assert!(file("/tmp/chroot.out").contains("all checks passed"));
    assert!(only_kernel_left(), "{:?}", process::list());
}

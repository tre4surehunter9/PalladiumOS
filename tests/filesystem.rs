// Copyright (c) 2026 tre4surehunter9
//! Tests for the in-memory filesystem (Fdfs) and the shell commands built on it.
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(palladiumos::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec;
use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;
use palladiumos::fdfs::Fdfs;
use palladiumos::filesystem;
use palladiumos::shell::process_command;

entry_point!(main, config = &palladiumos::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut BootInfo) -> ! {
    let boot_info: &'static BootInfo = boot_info;
    use palladiumos::allocator;
    use palladiumos::memory::{self, BootInfoFrameAllocator};
    use x86_64::VirtAddr;

    palladiumos::init();
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset.into_option().unwrap());
    let mut mapper = unsafe { memory::init(phys_mem_offset) };
    let mut frame_allocator = unsafe { BootInfoFrameAllocator::init(&boot_info.memory_regions) };
    allocator::init_heap(&mut mapper, &mut frame_allocator).expect("heap initialization failed");

    test_main();
    palladiumos::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    palladiumos::test_panic_handler(info)
}

fn entry(name: &str, is_dir: bool) -> (String, bool) {
    (name.to_string(), is_dir)
}

// ---- Fdfs ----

#[test_case]
fn new_fs_has_empty_root() {
    let fs = Fdfs::new();
    assert!(fs.is_dir("/"));
    assert_eq!(fs.list_dir("/").unwrap(), vec![]);
}

#[test_case]
fn write_then_read() {
    let mut fs = Fdfs::new();
    fs.write_file("/a.txt", "hello").unwrap();
    assert_eq!(fs.read_file("/a.txt").unwrap(), "hello");
    assert!(!fs.is_dir("/a.txt"));
}

#[test_case]
fn write_overwrites() {
    let mut fs = Fdfs::new();
    fs.write_file("/a.txt", "one").unwrap();
    fs.write_file("/a.txt", "two").unwrap();
    assert_eq!(fs.read_file("/a.txt").unwrap(), "two");
    assert_eq!(fs.list_dir("/").unwrap().len(), 1, "overwrite must not duplicate the entry");
}

#[test_case]
fn empty_file() {
    let mut fs = Fdfs::new();
    fs.write_file("/empty", "").unwrap();
    assert_eq!(fs.read_file("/empty").unwrap(), "");
}

#[test_case]
fn nested_dirs() {
    let mut fs = Fdfs::new();
    fs.make_dir("/a").unwrap();
    fs.make_dir("/a/b").unwrap();
    fs.write_file("/a/b/c.txt", "deep").unwrap();
    assert_eq!(fs.read_file("/a/b/c.txt").unwrap(), "deep");
    assert_eq!(fs.list_dir("/a").unwrap(), vec![entry("b", true)]);
    assert_eq!(fs.list_dir("/a/b").unwrap(), vec![entry("c.txt", false)]);
}

#[test_case]
fn list_dir_marks_dirs_and_files() {
    let mut fs = Fdfs::new();
    fs.make_dir("/dir").unwrap();
    fs.write_file("/file", "x").unwrap();
    assert_eq!(fs.list_dir("/").unwrap(), vec![entry("dir", true), entry("file", false)]);
}

#[test_case]
fn trailing_slash_resolves() {
    let mut fs = Fdfs::new();
    fs.make_dir("/dir").unwrap();
    assert!(fs.is_dir("/dir/"));
}

#[test_case]
fn missing_paths_are_errors() {
    let mut fs = Fdfs::new();
    assert!(fs.read_file("/nope").is_err());
    assert!(fs.list_dir("/nope").is_err());
    assert!(fs.write_file("/no/such/dir.txt", "x").is_err());
    assert!(fs.make_dir("/no/such").is_err());
    assert!(fs.remove("/nope").is_err());
    assert!(!fs.is_dir("/nope"));
}

#[test_case]
fn file_vs_dir_errors() {
    let mut fs = Fdfs::new();
    fs.make_dir("/d").unwrap();
    fs.write_file("/f", "x").unwrap();
    assert_eq!(fs.read_file("/d"), Err("Is a directory"));
    assert_eq!(fs.list_dir("/f"), Err("Not a directory"));
    assert_eq!(fs.write_file("/d", "x"), Err("Is a directory"));
    assert!(fs.write_file("/f/child", "x").is_err(), "a file can't have children");
    assert!(fs.make_dir("/f/child").is_err());
}

#[test_case]
fn mkdir_existing_fails() {
    let mut fs = Fdfs::new();
    fs.make_dir("/d").unwrap();
    assert_eq!(fs.make_dir("/d"), Err("Already exists"));
}

#[test_case]
fn bad_path_shapes() {
    let mut fs = Fdfs::new();
    assert!(fs.write_file("noslash", "x").is_err());
    assert!(fs.write_file("/", "x").is_err());
    assert!(fs.make_dir("/d/").is_err());
}

#[test_case]
fn remove_file_and_empty_dir() {
    let mut fs = Fdfs::new();
    fs.write_file("/f", "x").unwrap();
    fs.make_dir("/d").unwrap();
    fs.remove("/f").unwrap();
    fs.remove("/d").unwrap();
    assert_eq!(fs.list_dir("/").unwrap(), vec![]);
    assert!(fs.read_file("/f").is_err());
}

#[test_case]
fn remove_non_empty_dir_fails() {
    let mut fs = Fdfs::new();
    fs.make_dir("/d").unwrap();
    fs.write_file("/d/f", "x").unwrap();
    assert_eq!(fs.remove("/d"), Err("Directory is not empty"));
    assert_eq!(fs.read_file("/d/f").unwrap(), "x");
}

#[test_case]
fn recreate_after_remove() {
    let mut fs = Fdfs::new();
    fs.write_file("/f", "old").unwrap();
    fs.remove("/f").unwrap();
    fs.write_file("/f", "new").unwrap();
    assert_eq!(fs.read_file("/f").unwrap(), "new");
    assert_eq!(fs.list_dir("/").unwrap().len(), 1);
}

#[test_case]
fn names_are_case_sensitive() {
    let mut fs = Fdfs::new();
    fs.write_file("/A", "upper").unwrap();
    fs.write_file("/a", "lower").unwrap();
    assert_eq!(fs.read_file("/A").unwrap(), "upper");
    assert_eq!(fs.read_file("/a").unwrap(), "lower");
}

// ---- global filesystem + shell ----
// These share the global FS and CWD, so each test uses its own directory
// and resets CWD to "/" when done.

fn cwd() -> String {
    filesystem::CWD.lock().clone()
}

#[test_case]
fn default_files_exist() {
    filesystem::init_default_files();
    assert!(filesystem::read_file("/etc/hosts").is_ok());
    assert!(filesystem::read_file("/etc/shells").is_ok());
    // The 0.4 demo files are gone.
    assert!(!filesystem::is_dir("/scripts"));
    assert!(filesystem::read_file("/welcome.txt").is_err());
}

#[test_case]
fn resolve_path_uses_cwd() {
    assert_eq!(filesystem::resolve_path("/abs"), "/abs");
    assert_eq!(filesystem::resolve_path("rel"), "/rel");
    filesystem::make_dir("/rp").unwrap();
    *filesystem::CWD.lock() = "/rp".to_string();
    assert_eq!(filesystem::resolve_path("rel"), "/rp/rel");
    assert_eq!(filesystem::resolve_path("/abs"), "/abs");
    assert_eq!(filesystem::resolve_path(".."), "/");
    assert_eq!(filesystem::resolve_path("../rp/x"), "/rp/x");
    *filesystem::CWD.lock() = "/".to_string();
}

#[test_case]
fn shell_mkdir_cd_pwd() {
    assert!(process_command("mkdir /sh1"));
    assert!(process_command("cd /sh1"));
    assert_eq!(cwd(), "/sh1");
    assert!(process_command("mkdir sub")); // relative to CWD
    assert!(process_command("cd sub"));
    assert_eq!(cwd(), "/sh1/sub");
    assert!(process_command("cd .."));
    assert_eq!(cwd(), "/sh1");
    assert!(process_command("cd .."));
    assert_eq!(cwd(), "/");
    assert!(process_command("cd .."), "cd .. at root stays at root");
    assert_eq!(cwd(), "/");
    assert!(process_command("pwd"));
}

#[test_case]
fn normalize_paths() {
    use filesystem::normalize;
    assert_eq!(normalize("/", "a"), "/a");
    assert_eq!(normalize("/a/b", "c"), "/a/b/c");
    assert_eq!(normalize("/a/b", "../c"), "/a/c");
    assert_eq!(normalize("/a/b", "../../.."), "/");
    assert_eq!(normalize("/a", "./x//y/"), "/a/x/y");
    assert_eq!(normalize("/a", "/abs/./p/.."), "/abs");
    assert_eq!(normalize("/", ".."), "/");
    assert_eq!(normalize("/", ""), "/");
}

#[test_case]
fn shell_cd_relative_paths() {
    assert!(process_command("mkdir /rel1"));
    assert!(process_command("mkdir /rel1/x"));
    assert!(process_command("mkdir /rel2"));
    assert!(process_command("cd /rel1/x"));
    assert!(process_command("cd ../../rel2"));
    assert_eq!(cwd(), "/rel2");
    assert!(process_command("cd ./../rel1/./x/"));
    assert_eq!(cwd(), "/rel1/x");
    assert!(process_command("cd /"));
}

#[test_case]
fn shell_cd_with_no_args_goes_home() {
    assert!(process_command("mkdir /sh2"));
    assert!(process_command("cd /sh2"));
    assert!(process_command("cd"));
    assert_eq!(cwd(), "/");
}

#[test_case]
fn shell_cd_into_file_fails() {
    filesystem::write_file("/sh3file", "x").unwrap();
    assert!(!process_command("cd /sh3file"));
    assert!(!process_command("cd /does-not-exist"));
    assert_eq!(cwd(), "/");
}

#[test_case]
fn shell_cat_and_rm() {
    filesystem::write_file("/sh4.txt", "contents").unwrap();
    assert!(process_command("cat /sh4.txt"));
    assert!(process_command("rm /sh4.txt"));
    assert!(!process_command("cat /sh4.txt"));
    assert!(!process_command("rm /sh4.txt"));
}

#[test_case]
fn shell_ls() {
    assert!(process_command("mkdir /sh5"));
    assert!(process_command("ls /sh5"));
    assert!(process_command("ls"));
}

#[test_case]
fn shell_missing_args_fail() {
    for cmd in ["cat", "mkdir", "rm", "edit", "script", "run"] {
        assert!(!process_command(cmd), "{} with no args should fail", cmd);
    }
}

#[test_case]
fn shell_unknown_command_fails() {
    assert!(!process_command("definitely-not-a-command"));
    assert!(!process_command(""));
    assert!(!process_command("   "));
}

#[test_case]
fn shell_simple_commands() {
    assert!(process_command("echo hello world"));
    assert!(process_command("help"));
    assert!(process_command("about"));
    assert!(process_command("  echo leading spaces are trimmed"));
}

#[test_case]
fn script_runs_all_lines() {
    filesystem::write_file("/sh6.psh", "# comment\n\nmkdir /sh6dir\nmkdir /sh6dir/inner\n").unwrap();
    assert!(process_command("script /sh6.psh"));
    assert!(filesystem::is_dir("/sh6dir/inner"));
}

#[test_case]
fn script_stops_at_first_error() {
    filesystem::write_file("/sh7.psh", "mkdir /sh7a\nbogus-command\nmkdir /sh7b\n").unwrap();
    assert!(!process_command("script /sh7.psh"));
    assert!(filesystem::is_dir("/sh7a"));
    assert!(!filesystem::is_dir("/sh7b"), "lines after the error must not run");
}

#[test_case]
fn script_missing_file_fails() {
    assert!(!process_command("script /no-such-script.psh"));
}

#[test_case]
fn run_unknown_program_fails() {
    assert!(!process_command("run no-such-program"));
}

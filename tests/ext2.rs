// Copyright (c) 2026 tre4surehunter9
//! Tests for the ext2 driver, the VFS (mounts), /dev, the ATA disk driver
//! and the real-time clock.
//!
//! The ext2 tests use a RAM disk holding a small image made by build.rs
//! (mke2fs -d), so they always start from the same state. The ATA test
//! uses the scratch disk QEMU attaches for tests (target/test-disk.img).
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(palladiumos::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;
use palladiumos::block::{AtaDisk, BlockDevice, RamDisk};
use palladiumos::ext2::Ext2;
use palladiumos::mounts::{self, FileSystem, FileType};
use palladiumos::syscall::{EEXIST, EISDIR, ENOENT, ENOTDIR, ENOTEMPTY};
use palladiumos::{filesystem, memory, programs};

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
    palladiumos::rtc::init();
    programs::install();

    test_main();
    palladiumos::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    palladiumos::test_panic_handler(info)
}

/// A fresh copy of the test image.
fn fresh() -> Ext2 {
    assert!(!programs::EXT2_TEST_IMAGE.is_empty(), "build.rs couldn't make the ext2 test image (is mke2fs installed?)");
    Ext2::mount(Box::new(RamDisk::new(programs::EXT2_TEST_IMAGE))).expect("mount failed")
}

fn read_all(fs: &mut dyn FileSystem, ino: u64) -> Vec<u8> {
    let size = fs.meta(ino).unwrap().size as usize;
    let mut buf = vec![0u8; size];
    let n = fs.read(ino, 0, &mut buf).unwrap();
    buf.truncate(n);
    buf
}

fn path(fs: &mut dyn FileSystem, p: &str) -> Result<u64, i64> {
    let mut ino = fs.root();
    for c in p.split('/').filter(|c| !c.is_empty()) {
        ino = fs.lookup(ino, c)?;
    }
    Ok(ino)
}

// ---- reading what mke2fs wrote ----

#[test_case]
fn reads_files_made_by_linux_tools() {
    let mut fs = fresh();
    let root = fs.root();
    let hello = fs.lookup(root, "hello.txt").unwrap();
    assert_eq!(read_all(&mut fs, hello), b"Hello from ext2\n");
    let deep = path(&mut fs, "dir/nested/deep.txt").unwrap();
    assert_eq!(read_all(&mut fs, deep), b"deep\n");
    let empty = fs.lookup(root, "empty").unwrap();
    assert_eq!(fs.meta(empty).unwrap().size, 0);
    let names: Vec<String> = fs.readdir(root).unwrap().into_iter().map(|e| e.0).collect();
    for n in ["hello.txt", "dir", "empty", "big.bin", "lost+found"] {
        assert!(names.iter().any(|x| x == n), "{} missing from {:?}", n, names);
    }
    let dir = fs.lookup(root, "dir").unwrap();
    assert_eq!(fs.meta(dir).unwrap().kind, FileType::Dir);
}

#[test_case]
fn reads_a_file_with_indirect_blocks() {
    // 300 KiB with 1 KiB blocks: direct, single and double indirect blocks.
    let mut fs = fresh();
    let big = path(&mut fs, "big.bin").unwrap();
    let data = read_all(&mut fs, big);
    assert_eq!(data.len(), 300 * 1024);
    assert!(data.iter().enumerate().all(|(i, &b)| b == (i % 251) as u8), "content mismatch");
    // a read crossing block boundaries in the double-indirect range
    let mut buf = [0u8; 3000];
    assert_eq!(fs.read(big, 290_000, &mut buf).unwrap(), 3000);
    assert!(buf.iter().enumerate().all(|(i, &b)| b == ((290_000 + i) % 251) as u8));
    assert_eq!(fs.read(big, 300 * 1024, &mut buf).unwrap(), 0, "end of file");
}

#[test_case]
fn lookup_errors() {
    let mut fs = fresh();
    let root = fs.root();
    assert_eq!(fs.lookup(root, "nope"), Err(ENOENT));
    let hello = fs.lookup(root, "hello.txt").unwrap();
    assert_eq!(fs.lookup(hello, "x"), Err(ENOTDIR));
    assert_eq!(fs.read(root, 0, &mut [0u8; 4]), Err(EISDIR));
}

// ---- writing ----

#[test_case]
fn create_write_read_remove() {
    let mut fs = fresh();
    let (free_blocks, free_inodes) = fs.free_counts();
    let root = fs.root();
    let f = fs.create(root, "new.txt", FileType::File, 0o644).unwrap();
    assert_eq!(fs.create(root, "new.txt", FileType::File, 0o644), Err(EEXIST));
    assert_eq!(fs.write(f, 0, b"hello").unwrap(), 5);
    assert_eq!(fs.write(f, 5, b" world").unwrap(), 6);
    assert_eq!(read_all(&mut fs, f), b"hello world");
    assert_eq!(fs.write(f, 0, b"J").unwrap(), 1, "overwrite in place");
    assert_eq!(read_all(&mut fs, f), b"Jello world");
    assert_eq!(fs.free_counts().1, free_inodes - 1);

    let d = fs.create(root, "sub", FileType::Dir, 0o755).unwrap();
    let inner = fs.create(d, "inner", FileType::File, 0o644).unwrap();
    fs.write(inner, 0, b"x").unwrap();
    assert_eq!(fs.meta(d).unwrap().nlink, 2);
    assert_eq!(fs.remove(root, "sub", true), Err(ENOTEMPTY));
    fs.remove(d, "inner", false).unwrap();
    fs.remove(root, "sub", true).unwrap();
    fs.remove(root, "new.txt", false).unwrap();
    assert_eq!(fs.lookup(root, "new.txt"), Err(ENOENT));
    assert_eq!(fs.free_counts(), (free_blocks, free_inodes), "everything freed");
}

#[test_case]
fn big_file_round_trip_and_free() {
    let mut fs = fresh();
    let before = fs.free_counts();
    let root = fs.root();
    let f = fs.create(root, "big2", FileType::File, 0o644).unwrap();
    let data: Vec<u8> = (0..400 * 1024u32).map(|i| (i * 7 % 256) as u8).collect();
    let mut done = 0;
    while done < data.len() {
        done += fs.write(f, done as u64, &data[done..(done + 5000).min(data.len())]).unwrap();
    }
    assert_eq!(read_all(&mut fs, f), data);
    assert!(fs.free_counts().0 < before.0 - 400, "data and indirect blocks were allocated");
    fs.remove(root, "big2", false).unwrap();
    assert_eq!(fs.free_counts(), before, "data and indirect blocks freed");
}

#[test_case]
fn holes_and_truncate() {
    let mut fs = fresh();
    let root = fs.root();
    let f = fs.create(root, "sparse", FileType::File, 0o644).unwrap();
    fs.write(f, 100_000, b"end").unwrap();
    assert_eq!(fs.meta(f).unwrap().size, 100_003);
    let mut buf = vec![0xffu8; 100];
    fs.read(f, 50_000, &mut buf).unwrap();
    assert!(buf.iter().all(|&b| b == 0), "a hole reads as zeros");
    fs.truncate(f, 0).unwrap();
    assert_eq!(fs.meta(f).unwrap().size, 0);
    fs.write(f, 0, b"0123456789").unwrap();
    fs.truncate(f, 4).unwrap();
    assert_eq!(read_all(&mut fs, f), b"0123");
    fs.truncate(f, 8).unwrap();
    assert_eq!(read_all(&mut fs, f), b"0123\0\0\0\0", "growing again shows zeros, not old data");
}

#[test_case]
fn directories_grow_past_one_block() {
    let mut fs = fresh();
    let root = fs.root();
    let before = fs.free_counts();
    let d = fs.create(root, "many", FileType::Dir, 0o755).unwrap();
    for i in 0..300 {
        let name = format!("file-with-a-longish-name-{}", i);
        let f = fs.create(d, &name, FileType::File, 0o644).unwrap();
        fs.write(f, 0, name.as_bytes()).unwrap();
    }
    assert!(fs.meta(d).unwrap().size > 1024, "the directory needed more blocks");
    assert_eq!(fs.readdir(d).unwrap().len(), 300);
    for i in (0..300).step_by(37) {
        let name = format!("file-with-a-longish-name-{}", i);
        let f = fs.lookup(d, &name).unwrap();
        assert_eq!(read_all(&mut fs, f), name.as_bytes());
    }
    for i in 0..300 {
        fs.remove(d, &format!("file-with-a-longish-name-{}", i), false).unwrap();
    }
    assert_eq!(fs.readdir(d).unwrap().len(), 0);
    // entries can be reused after removal
    fs.create(d, "again", FileType::File, 0o644).unwrap();
    fs.remove(d, "again", false).unwrap();
    fs.remove(root, "many", true).unwrap();
    assert_eq!(fs.free_counts(), before);
}

#[test_case]
fn deleted_while_open_is_freed_on_close() {
    let mut fs = fresh();
    let root = fs.root();
    let before = fs.free_counts();
    let f = fs.create(root, "open-file", FileType::File, 0o644).unwrap();
    fs.write(f, 0, &[7u8; 5000]).unwrap();
    fs.open_inode(f);
    fs.remove(root, "open-file", false).unwrap();
    assert_eq!(fs.lookup(root, "open-file"), Err(ENOENT));
    assert_eq!(read_all(&mut fs, f).len(), 5000, "still readable while open");
    assert_ne!(fs.free_counts(), before);
    fs.close_inode(f);
    assert_eq!(fs.free_counts(), before, "freed at last close");
}

#[test_case]
fn filling_the_disk() {
    let mut fs = fresh();
    let root = fs.root();
    let before = fs.free_counts();
    let f = fs.create(root, "filler", FileType::File, 0o644).unwrap();
    let chunk = [0x5au8; 8192];
    let mut offset = 0u64;
    let err = loop {
        match fs.write(f, offset, &chunk) {
            Ok(n) if n == chunk.len() => offset += n as u64,
            Ok(n) => offset += n as u64, // short write: the disk is nearly full
            Err(e) => break e,
        }
    };
    assert_eq!(err, 28, "ENOSPC");
    assert_eq!(fs.free_counts().0, 0);
    assert!(fs.create(root, "x", FileType::Dir, 0o755).is_err(), "no room for a directory block");
    fs.remove(root, "filler", false).unwrap();
    assert_eq!(fs.free_counts(), before);
}

#[test_case]
fn refuses_non_ext2() {
    assert!(Ext2::mount(Box::new(RamDisk::new(&[0u8; 8192]))).is_err());
}

// ---- the VFS with ext2 as the root filesystem ----

/// Mount a fresh test image at "/" with /bin (programs) and /dev, run `f`,
/// then put the old root back.
fn with_ext2_root(f: impl FnOnce()) {
    let old = mounts::swap("/", Box::new(fresh())).unwrap();
    filesystem::make_dir("/bin").unwrap();
    filesystem::make_dir("/dev").unwrap();
    filesystem::make_dir("/tmp").unwrap();
    mounts::mount("/bin", Box::new(palladiumos::fdfs::Fdfs::new()));
    mounts::mount("/tmp", Box::new(palladiumos::fdfs::Fdfs::new()));
    mounts::mount("/dev", Box::new(palladiumos::devfs::DevFs));
    programs::install();
    f();
    mounts::unmount("/bin").unwrap();
    mounts::unmount("/dev").unwrap();
    mounts::unmount("/tmp").unwrap();
    mounts::swap("/", old);
}

fn prog(name: &str) -> &'static [u8] {
    programs::find(name).unwrap()
}

fn run(name: &str, args: &[&str]) -> i32 {
    palladiumos::usermode::run(prog(name), args).unwrap()
}

#[test_case]
fn userland_test_programs_on_ext2() {
    with_ext2_root(|| {
        assert_eq!(filesystem::read_file("/hello.txt").unwrap(), "Hello from ext2\n");
        assert_eq!(run("test_fs", &["test_fs", "all"]), 0, "file syscalls");
        assert_eq!(run("test_pipe", &["test_pipe", "all"]), 0, "shared offsets, cloexec");
        assert_eq!(run("test_misc", &["test_misc", "dup"]), 0);
        assert_eq!(run("test_proc", &["test_proc", "all"]), 0, "exec from /bin, files across fork");
        assert_eq!(run("test_links", &["test_links"]), 0, "rename, links, symlinks, chmod on ext2");
    });
}

#[test_case]
fn shell_scripts_on_ext2() {
    with_ext2_root(|| {
        let script = "mkdir -p /a/b && cd /a/b && for i in 1 2 3; do echo line $i >> f.txt; done; \
                      cat f.txt | wc -l > /count.txt; ls /a/b > /ls.txt; rm -r /a";
        assert_eq!(run("sh", &["sh", "-c", script]), 0);
        assert_eq!(filesystem::read_file("/count.txt").unwrap().trim(), "3");
        assert_eq!(filesystem::read_file("/ls.txt").unwrap(), "f.txt\n");
        assert!(!filesystem::is_dir("/a"));
    });
}

#[test_case]
fn mount_points_and_cross_mount_paths() {
    with_ext2_root(|| {
        assert!(filesystem::is_dir("/bin") && filesystem::is_dir("/dev"));
        assert!(filesystem::read_bytes("/bin/sh").is_ok(), "/bin is the memfs mount");
        assert_eq!(filesystem::remove("/bin"), Err("Device or resource busy"), "can't remove a mount point");
        assert_eq!(run("sh", &["sh", "-c", "cd /bin/../dir/nested && cat deep.txt > /x.txt"]), 0, ".. across mounts");
        assert_eq!(filesystem::read_file("/x.txt").unwrap(), "deep\n");
        let names: Vec<(String, &str)> = mounts::list();
        assert!(names.iter().any(|(p, n)| p == "/" && *n == "ext2"));
        assert!(names.iter().any(|(p, n)| p == "/bin" && *n == "memfs"));
    });
}

#[test_case]
fn dev_null_zero_tty_random() {
    with_ext2_root(|| {
        let script = "echo gone > /dev/null && cat /dev/null > /n.txt && test -c /dev/null && \
                      ! test -c /n.txt && ls /dev > /d.txt";
        assert_eq!(run("sh", &["sh", "-c", script]), 0);
        assert_eq!(filesystem::read_file("/n.txt").unwrap(), "");
        assert_eq!(filesystem::read_file("/d.txt").unwrap(), "console\ndsp\nfb0\ninput/\nmixer\nnull\npsaux\nptmx\npts/\nrandom\nshm/\ntty\ntty0\ntty1\nurandom\nzero\n");
        // /dev/urandom: random bytes (not all the same, different each read)
        assert_eq!(run("sh", &["sh", "-c", "cat /dev/urandom | head -n 4 > /r1; cat /dev/urandom | head -n 4 > /r2"]), 0);
        let (r1, r2) = (filesystem::read_bytes("/r1").unwrap(), filesystem::read_bytes("/r2").unwrap());
        assert!(r1.len() > 4);
        assert!(r1 != r2 && r1.iter().any(|&b| b != r1[0]));
        assert_eq!(run("sh", &["sh", "-c", "echo to-tty > /dev/tty"]), 0);
        assert_eq!(run("sh", &["sh", "-c", "touch /dev/new"]), 1, "can't create files in /dev");
    });
}

// ---- hardware ----

#[test_case]
fn ata_disk() {
    // QEMU attaches target/test-disk.img (an empty ext2 image) for tests.
    let mut disk = AtaDisk::primary_slave().expect("no ATA disk found (QEMU -drive index=1)");
    assert_eq!(disk.sectors(), 4 * 1024 * 1024 / 512);
    let mut sb = [0u8; 1024];
    disk.read(2, &mut sb).unwrap();
    assert_eq!(u16::from_le_bytes([sb[56], sb[57]]), 0xEF53, "ext2 magic");
    // write and read back the last sector (unused by the filesystem)
    let last = disk.sectors() - 1;
    let pattern: Vec<u8> = (0..512).map(|i| (i * 3) as u8).collect();
    disk.write(last, &pattern).unwrap();
    disk.flush().unwrap();
    let mut back = [0u8; 512];
    disk.read(last, &mut back).unwrap();
    assert_eq!(&back[..], &pattern[..]);
    assert!(disk.read(disk.sectors(), &mut back).is_err(), "past the end");

    // and the filesystem on it
    let mut fs = Ext2::mount(Box::new(disk)).unwrap();
    let root = fs.root();
    let f = match fs.lookup(root, "ata-test.txt") {
        Ok(f) => f,
        Err(_) => fs.create(root, "ata-test.txt", FileType::File, 0o644).unwrap(),
    };
    fs.truncate(f, 0).unwrap();
    fs.write(f, 0, b"written through ATA").unwrap();
    fs.sync().unwrap();
    assert_eq!(read_all(&mut fs, f), b"written through ATA");
}

#[test_case]
fn real_time_clock() {
    let now = palladiumos::rtc::now();
    assert!(now > 1_577_836_800, "after 2020: {}", now); // 2020-01-01
    assert!(now < 4_102_444_800, "before 2100: {}", now);
}

// ---- links, symlinks, rename, chmod at the driver level ----

#[test_case]
fn ext2_links_and_rename() {
    let mut fs = fresh();
    let root = fs.root();
    let before = fs.free_counts();
    let f = fs.create(root, "f", FileType::File, 0o640).unwrap();
    fs.write(f, 0, &[1u8; 3000]).unwrap();
    assert_eq!(fs.meta(f).unwrap().perm, 0o640);
    fs.link(root, "f2", f).unwrap();
    assert_eq!(fs.meta(f).unwrap().nlink, 2);
    fs.remove(root, "f", false).unwrap();
    assert_eq!(fs.meta(f).unwrap().nlink, 1, "still linked as f2");
    assert_eq!(read_all(&mut fs, f).len(), 3000);

    let d = fs.create(root, "d", FileType::Dir, 0o755).unwrap();
    let e = fs.create(root, "e", FileType::Dir, 0o755).unwrap();
    let root_links = fs.meta(root).unwrap().nlink;
    fs.rename(root, "d", e, "moved").unwrap();
    assert_eq!(fs.lookup(e, "moved"), Ok(d));
    assert_eq!(fs.meta(root).unwrap().nlink, root_links - 1, "d's .. no longer points at /");
    assert_eq!(fs.meta(e).unwrap().nlink, 3, "e has its own .. plus moved's");
    fs.rename(root, "f2", e, "file").unwrap();
    assert_eq!(fs.lookup(root, "f2"), Err(ENOENT));

    let s = fs.symlink(root, "short", "e/file").unwrap();
    assert_eq!(fs.meta(s).unwrap().kind, FileType::Symlink);
    assert_eq!(fs.readlink(s).unwrap(), "e/file");
    let target: String = core::iter::repeat("x/").take(60).collect();
    let l = fs.symlink(root, "long", &target).unwrap();
    assert_eq!(fs.readlink(l).unwrap(), target, "a long target lives in a block");
    fs.chmod(l, 0o777).unwrap();

    // remove everything: the counts must come back exactly
    fs.remove(root, "short", false).unwrap();
    fs.remove(root, "long", false).unwrap();
    fs.remove(e, "file", false).unwrap();
    fs.remove(e, "moved", true).unwrap();
    fs.remove(root, "e", true).unwrap();
    // root_links was counted with d and e both in /: both are gone now
    assert_eq!(fs.meta(root).unwrap().nlink, root_links - 2, "back to the original count");
    assert_eq!(fs.free_counts(), before);
}

#[test_case]
fn ext2_rename_replaces_and_frees() {
    let mut fs = fresh();
    let root = fs.root();
    let before = fs.free_counts();
    let a = fs.create(root, "a", FileType::File, 0o644).unwrap();
    fs.write(a, 0, b"new").unwrap();
    let b = fs.create(root, "b", FileType::File, 0o644).unwrap();
    fs.write(b, 0, &[9u8; 20_000]).unwrap();
    fs.rename(root, "a", root, "b").unwrap();
    assert_eq!(fs.lookup(root, "b"), Ok(a));
    assert_eq!(read_all(&mut fs, a), b"new");
    fs.remove(root, "b", false).unwrap();
    assert_eq!(fs.free_counts(), before, "the replaced file's blocks were freed");
}

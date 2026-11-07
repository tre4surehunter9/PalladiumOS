// Copyright (c) 2026 tre4surehunter9
//! palladium-runner: cargo's runner for PalladiumOS (see .cargo/config.toml).
//! Turns a kernel ELF into a bootable disk image with the bootloader crate
//! (0.11) and starts QEMU, like `bootimage runner` did for bootloader 0.9.
//!
//!   palladium-runner KERNEL [QEMU ARGS...]     cargo run / cargo test
//!   palladium-runner image KERNEL OUT.img      just make the disk image
//!
//! Test kernels (cargo names them NAME-HASH) run without a
//! window, with serial output on the terminal and a time limit; they report
//! success by writing 0x10 to the isa-debug-exit port, which QEMU turns into
//! exit status 33.

use std::path::{Path, PathBuf};
use std::process::{exit, Command};
use std::time::{Duration, Instant};

/// QEMU's arguments for `cargo run`: the data disk, the system disk (/usr),
/// the Alpine disk, and a network card (port 2222 on this machine, localhost
/// only, reaches PalladiumOS's SSH server).
const RUN_ARGS: &[&str] = &[
    "-drive", "format=raw,file=target/disk.img,index=1,media=disk",
    "-drive", "format=raw,file=target/usr.img,index=2,media=disk",
    "-drive", "format=raw,file=target/alpine.img,index=3,media=disk",
    "-netdev", "user,id=net0,hostfwd=tcp:127.0.0.1:2222-:22", "-device", "rtl8139,netdev=net0",
];

/// QEMU's arguments for test kernels.
const TEST_ARGS: &[&str] = &[
    "-device", "isa-debug-exit,iobase=0xf4,iosize=0x04", "-serial", "stdio", "-display", "none",
    "-drive", "format=raw,file=target/test-disk.img,index=1,media=disk",
    "-drive", "format=raw,file=target/usr.img,index=2,media=disk",
    "-netdev", "user,id=net0", "-device", "rtl8139,netdev=net0",
];
const TEST_SUCCESS: i32 = 33; // (0x10 << 1) | 1
const TEST_TIMEOUT: Duration = Duration::from_secs(300);

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("image") if args.len() == 3 => {
            make_image(Path::new(&args[1]), Path::new(&args[2]));
        }
        Some(kernel) => run(Path::new(kernel), &args[1..]),
        None => {
            eprintln!("usage: palladium-runner KERNEL [QEMU ARGS...] | palladium-runner image KERNEL OUT.img");
            exit(2);
        }
    }
}

fn make_image(kernel: &Path, out: &Path) {
    // Without its debug info (kept in the kernel file itself, for
    // addr2line): the bootloader loads the whole file through the BIOS,
    // and debug info would make that slow and use up memory.
    let stripped = PathBuf::from(format!("{}.stripped", kernel.display()));
    let source = if strip(kernel, &stripped) { stripped.as_path() } else { kernel };
    if let Err(e) = bootloader::BiosBoot::new(source).create_disk_image(out) {
        eprintln!("palladium-runner: can't make a disk image from {}: {e}", kernel.display());
        exit(1);
    }
}

/// Copy `kernel` to `out` without debug info, with llvm-objcopy from Rust's
/// llvm-tools (which this project's toolchain includes). False if that
/// didn't work: the kernel is used as it is.
fn strip(kernel: &Path, out: &Path) -> bool {
    let Ok(sysroot) = Command::new("rustc").arg("--print").arg("sysroot").output() else { return false };
    let sysroot = String::from_utf8_lossy(&sysroot.stdout).trim().to_string();
    let objcopy = Path::new(&sysroot).join("lib/rustlib/x86_64-unknown-linux-gnu/bin/llvm-objcopy");
    Command::new(objcopy)
        .arg("--strip-debug")
        .arg(kernel)
        .arg(out)
        .status()
        .is_ok_and(|s| s.success())
}

/// The repository: the directory holding the `target` directory the kernel
/// was built in (QEMU's disk paths are relative to it).
fn repo_root(kernel: &Path) -> PathBuf {
    let kernel = kernel.canonicalize().unwrap_or_else(|_| kernel.to_path_buf());
    for dir in kernel.ancestors() {
        if dir.file_name().is_some_and(|n| n == "target") {
            return dir.parent().unwrap_or(dir).to_path_buf();
        }
    }
    std::env::current_dir().unwrap()
}

/// Test kernels are the ones cargo names NAME-HASH (16 hex digits), in a
/// `deps` directory or (newer cargo) a build directory.
fn is_test_kernel(kernel: &Path) -> bool {
    let name = kernel.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let hashed = name.rsplit_once('-').is_some_and(|(_, h)| h.len() == 16 && h.bytes().all(|b| b.is_ascii_hexdigit()));
    hashed || kernel.components().any(|c| c.as_os_str() == "deps")
}

fn run(kernel: &Path, extra: &[String]) {
    let test = is_test_kernel(kernel);
    let image = PathBuf::from(format!("{}.img", kernel.display()));
    make_image(kernel, &image);

    let mut qemu = Command::new("qemu-system-x86_64");
    qemu.current_dir(repo_root(kernel))
        .arg("-drive")
        .arg(format!("format=raw,file={}", image.canonicalize().unwrap_or(image.clone()).display()))
        .arg("-no-reboot")
        .args(if test { TEST_ARGS } else { RUN_ARGS })
        .args(extra);

    if !test {
        let status = qemu.status().unwrap_or_else(|e| {
            eprintln!("palladium-runner: can't start qemu-system-x86_64: {e}");
            exit(1)
        });
        exit(status.code().unwrap_or(1));
    }

    // A test kernel: wait (with a time limit), then turn QEMU's status into
    // success or failure.
    let mut child = qemu.spawn().unwrap_or_else(|e| {
        eprintln!("palladium-runner: can't start qemu-system-x86_64: {e}");
        exit(1)
    });
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let code = status.code().unwrap_or(1);
                exit(if code == TEST_SUCCESS { 0 } else { code.max(1) });
            }
            Ok(None) if start.elapsed() > TEST_TIMEOUT => {
                let _ = child.kill();
                eprintln!("palladium-runner: test timed out after {} s", TEST_TIMEOUT.as_secs());
                exit(1);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => {
                eprintln!("palladium-runner: waiting for QEMU: {e}");
                exit(1);
            }
        }
    }
}

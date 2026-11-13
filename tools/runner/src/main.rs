// Copyright (c) 2026 tre4surehunter9
//! palladium-runner: cargo's runner for PalladiumOS (see .cargo/config.toml).
//! Turns a kernel ELF into a bootable disk image with the bootloader crate
//! (0.11) and starts QEMU, like `bootimage runner` did for bootloader 0.9.
//!
//!   palladium-runner KERNEL [--uefi] [QEMU ARGS...]   cargo run / cargo test
//!   palladium-runner image KERNEL OUT.img [--uefi]    just make the disk image
//!   palladium-runner usb KERNEL OUT.img               one UEFI disk with everything
//!
//! `usb` makes a GPT disk image for a USB stick (or any disk): the UEFI
//! boot partition, then ext2 partitions the kernel finds by name:
//! palladium-data (/, fresh, 256 MiB), palladium-usr (/usr, from
//! target/usr.img) and palladium-alpine (/alpine, empty, 1 GiB). The file
//! is sparse; `dd ... conv=sparse` or any flashing tool writes it.
//!
//! `cargo run` boots with UEFI when the OVMF firmware is installed (Arch:
//! edk2-ovmf; or point OVMF at the file): a UEFI disk image (GPT, an EFI
//! system partition), at 1280x720. --bios (or PALLADIUM_BIOS=1) boots with
//! BIOS instead (the bootloader's BIOS stage goes up to 1280x720). Test
//! kernels boot with BIOS unless --uefi / PALLADIUM_UEFI=1.
//!
//! Test kernels (cargo names them NAME-HASH) run without a
//! window, with serial output on the terminal and a time limit; they report
//! success by writing 0x10 to the isa-debug-exit port, which QEMU turns into
//! exit status 33.

use std::path::{Path, PathBuf};
use std::process::{exit, Command};
use std::time::{Duration, Instant};

/// QEMU's arguments for `cargo run`: the data disk, the system disk (/usr),
/// the Alpine disk, and a network card. Ports on this machine (localhost
/// only) reach PalladiumOS: 2222 its SSH server (22), 8080 a web server on
/// port 80 (Apache, BusyBox httpd...).
const RUN_ARGS: &[&str] = &[
    // 1 GiB of memory: Node.js and other big programs don't fit in
    // QEMU's default 128 MiB.
    "-m", "1G",
    // (The screen: display_args.)
    // The data, system and Alpine disks as virtio disks (in this order:
    // PalladiumOS numbers them so; much faster than IDE or AHCI).
    "-drive", "format=raw,file=target/disk.img,if=virtio",
    "-drive", "format=raw,file=target/usr.img,if=virtio",
    "-drive", "format=raw,file=target/alpine.img,if=virtio",
    // A virtio network card (much faster than an emulated RTL8139).
    "-netdev", "user,id=net0,hostfwd=tcp:127.0.0.1:2222-:22,hostfwd=tcp:127.0.0.1:8080-:80", "-device", "virtio-net-pci,netdev=net0",
    // A virtio keyboard (QEMU sends the window's keys to it, not to PS/2)
    // and a virtio tablet: the pointer follows yours in QEMU's window,
    // without it grabbing the mouse. (And a USB controller, for USB
    // devices: PalladiumOS drives USB keyboards and tablets too.)
    "-device", "virtio-keyboard-pci", "-device", "virtio-tablet-pci", "-device", "qemu-xhci",
    // (The sound card: display_args.)
    // The host's randomness, for /dev/random (virtio-rng).
    "-device", "virtio-rng-pci",
    // A second terminal, with a login, on a socket (virtio-console):
    // `socat -,raw,echo=0 UNIX-CONNECT:target/console.sock`.
    "-device", "virtio-serial-pci", "-device", "virtconsole,chardev=hvc0",
    "-chardev", "socket,id=hvc0,path=target/console.sock,server=on,wait=off",
];

/// QEMU's arguments for test kernels.
const TEST_ARGS: &[&str] = &[
    "-device", "isa-debug-exit,iobase=0xf4,iosize=0x04", "-serial", "stdio", "-display", "none",
    // (The disks and the network card: test_extra_args.)
    "-netdev", "user,id=net0",
    // A sound card whose sound goes nowhere (it still plays in real time).
    "-audiodev", "none,id=snd0", "-device", "AC97,audiodev=snd0",
];
const TEST_SUCCESS: i32 = 33; // (0x10 << 1) | 1
/// How long a test kernel may run (PALLADIUM_TEST_TIMEOUT seconds instead,
/// for the soak test: tools/soak.sh).
fn test_timeout() -> Duration {
    let secs = std::env::var("PALLADIUM_TEST_TIMEOUT").ok().and_then(|s| s.parse().ok()).unwrap_or(300);
    Duration::from_secs(secs)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |f: &str| args.iter().any(|a| a == f);
    let env = |v: &str| std::env::var(v).is_ok_and(|x| x == "1");
    let want_uefi = flag("--uefi") || env("PALLADIUM_UEFI");
    let want_bios = flag("--bios") || env("PALLADIUM_BIOS");
    let args: Vec<String> = args.into_iter().filter(|a| a != "--uefi" && a != "--bios").collect();
    match args.first().map(String::as_str) {
        Some("image") if args.len() == 3 => {
            make_image(Path::new(&args[1]), Path::new(&args[2]), want_uefi);
        }
        Some("usb") if args.len() == 3 => {
            if let Err(e) = make_usb_image(Path::new(&args[1]), Path::new(&args[2])) {
                eprintln!("palladium-runner: can't make the USB image: {e}");
                exit(1);
            }
        }
        Some(kernel) => {
            let kernel = Path::new(kernel);
            // cargo run: UEFI if there's firmware for it; tests: BIOS.
            let uefi = !want_bios && (want_uefi || (!is_test_kernel(kernel) && find_ovmf().is_some()));
            run(kernel, &args[1..], uefi)
        }
        None => {
            eprintln!("usage: palladium-runner KERNEL [QEMU ARGS...] | palladium-runner image KERNEL OUT.img");
            exit(2);
        }
    }
}

fn make_image(kernel: &Path, out: &Path, uefi: bool) {
    // Without its debug info (kept in the kernel file itself, for
    // addr2line): the bootloader loads the whole file through the BIOS,
    // and debug info would make that slow and use up memory.
    let stripped = PathBuf::from(format!("{}.stripped", kernel.display()));
    let source = if strip(kernel, &stripped) { stripped.as_path() } else { kernel };
    let made = if uefi {
        bootloader::UefiBoot::new(source).create_disk_image(out)
    } else {
        bootloader::BiosBoot::new(source).create_disk_image(out)
    };
    if let Err(e) = made {
        eprintln!("palladium-runner: can't make a disk image from {}: {e}", kernel.display());
        exit(1);
    }
}

const MIB: u64 = 1024 * 1024;

/// See `usb` at the top.
fn make_usb_image(kernel: &Path, out: &Path) -> Result<(), String> {
    use std::fs;
    let work = PathBuf::from(format!("{}.parts", out.display()));
    fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let stripped = work.join("kernel");
    let source = if strip(kernel, &stripped) { stripped.clone() } else { kernel.to_path_buf() };

    let esp = work.join("esp.img");
    bootloader::DiskImageBuilder::new(source)
        .create_uefi_fat_partition(&esp)
        .map_err(|e| format!("boot partition: {e}"))?;
    let data = work.join("data.img");
    mke2fs(&data, "256M")?;
    let alpine = work.join("alpine.img");
    mke2fs(&alpine, "1G")?;
    let usr = repo_root(kernel).join("target/usr.img");
    if !usr.exists() {
        return Err(format!("{} is missing (cargo build makes it)", usr.display()));
    }

    let parts: [(&str, &Path, gpt::partition_types::Type); 4] = [
        ("boot", &esp, gpt::partition_types::EFI),
        ("palladium-data", &data, gpt::partition_types::LINUX_FS),
        ("palladium-usr", &usr, gpt::partition_types::LINUX_FS),
        ("palladium-alpine", &alpine, gpt::partition_types::LINUX_FS),
    ];
    let size_of = |p: &Path| fs::metadata(p).map(|m| m.len().next_multiple_of(MIB)).map_err(|e| e.to_string());
    let mut total = 2 * MIB; // GPT at both ends, 1 MiB alignment
    for (_, path, _) in &parts {
        total += size_of(path)?;
    }

    let _ = fs::remove_file(out);
    let mut disk = fs::OpenOptions::new().create(true).read(true).write(true).open(out).map_err(|e| e.to_string())?;
    disk.set_len(total).map_err(|e| e.to_string())?;
    let mbr = gpt::mbr::ProtectiveMBR::with_lb_size(u32::try_from(total / 512 - 1).unwrap_or(u32::MAX));
    mbr.overwrite_lba0(&mut disk).map_err(|e| e.to_string())?;
    let lb = gpt::disk::LogicalBlockSize::Lb512;
    let mut table = gpt::GptConfig::new()
        .writable(true)
        .initialized(false)
        .logical_block_size(lb)
        .create_from_device(Box::new(&mut disk), None)
        .map_err(|e| e.to_string())?;
    table.update_partitions(Default::default()).map_err(|e| e.to_string())?;
    let mut offsets = Vec::new();
    for (name, path, kind) in &parts {
        let id = table.add_partition(name, size_of(path)?, kind.clone(), 0, Some(MIB / 512)).map_err(|e| e.to_string())?;
        let start = table.partitions()[&id].bytes_start(lb).map_err(|e| e.to_string())?;
        offsets.push(start);
    }
    table.write().map_err(|e| e.to_string())?;
    for ((name, path, _), start) in parts.iter().zip(offsets) {
        copy_sparse(path, out, start).map_err(|e| format!("{name}: {e}"))?;
    }
    let _ = fs::remove_dir_all(&work);
    Ok(())
}

/// Copy `src` into `dst` at `offset`, skipping all-zero blocks (they are
/// zero already, and the image stays sparse).
fn copy_sparse(src: &Path, dst: &Path, offset: u64) -> std::io::Result<()> {
    use std::io::{Read, Seek, SeekFrom, Write};
    let mut from = std::fs::File::open(src)?;
    let mut to = std::fs::OpenOptions::new().write(true).open(dst)?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut pos = offset;
    loop {
        let n = from.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        if buf[..n].iter().any(|&b| b != 0) {
            to.seek(SeekFrom::Start(pos))?;
            to.write_all(&buf[..n])?;
        }
        pos += n as u64;
    }
}

/// An empty ext2 filesystem, with the options build.rs uses.
fn mke2fs(image: &Path, size: &str) -> Result<(), String> {
    let _ = std::fs::remove_file(image);
    let ok = Command::new("mke2fs")
        .args(["-q", "-t", "ext2", "-b", "1024", "-O", "^dir_index,^resize_inode,^ext_attr", "-L", "palladium", "-F"])
        .arg(image)
        .arg(size)
        .status()
        .is_ok_and(|s| s.success());
    if ok { Ok(()) } else { Err("mke2fs failed (is e2fsprogs installed?)".into()) }
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
/// `cargo run`'s screen: a virtio GPU (virtio-vga: VGA-compatible for the
/// firmware and the boot, then PalladiumOS puts the screen on virtio-gpu),
/// PALLADIUM_RESOLUTION (WIDTHxHEIGHT, default 1280x720) in size. The GPU
/// tells the firmware that size as its preferred one (EDID), and the
/// kernel gets it directly too (a fw_cfg file): with QEMU's window, the
/// GPU reports the window's size instead, smaller when the window manager
/// shrinks the window to fit the desktop. PALLADIUM_FOLLOW_WINDOW=1 leaves
/// the fw_cfg file out: the screen takes the window's size (at boot).
///
/// And QEMU's GTK window with Zoom To Fit off (unless the arguments choose
/// a display): for a GPU that can be told the window's size, QEMU turns it
/// on by default, and then never makes the window as big as the screen
/// (it scales the screen into whatever size the window has: small, at
/// first). Off, the window takes the screen's size, as with VGA.
fn display_args(extra: &[String]) -> Vec<String> {
    let wanted = std::env::var("PALLADIUM_RESOLUTION").ok();
    let (w, h) = wanted
        .as_deref()
        .and_then(|r| r.split_once('x'))
        .and_then(|(w, h)| Some((w.trim().parse::<u32>().ok()?, h.trim().parse::<u32>().ok()?)))
        .filter(|&(w, h)| (640..=7680).contains(&w) && (480..=4320).contains(&h))
        .unwrap_or_else(|| {
            if let Some(r) = &wanted {
                eprintln!("palladium-runner: PALLADIUM_RESOLUTION={:?} isn't WIDTHxHEIGHT (640x480 to 7680x4320): using 1280x720", r);
            }
            (1280, 720)
        });
    let mut args = vec![
        "-vga".to_string(),
        "none".to_string(),
        "-device".to_string(),
        format!("virtio-vga,edid=on,xres={},yres={}", w, h),
    ];
    if std::env::var_os("PALLADIUM_FOLLOW_WINDOW").is_none_or(|v| v.is_empty() || v == "0") {
        args.push("-fw_cfg".to_string());
        args.push(format!("name=opt/palladium/resolution,string={}x{}", w, h));
    }
    // The sound card, played through this machine's sound system: virtio
    // sound, or AC'97 with PALLADIUM_SOUND=ac97.
    let ac97 = std::env::var("PALLADIUM_SOUND").is_ok_and(|v| v.eq_ignore_ascii_case("ac97"));
    args.push("-device".to_string());
    args.push(if ac97 { "AC97" } else { "virtio-sound-pci" }.to_string());
    let chose_display = extra.iter().any(|a| matches!(a.as_str(), "-display" | "-nographic" | "-vnc" | "-spice" | "-sdl" | "-curses"));
    if !chose_display && gtk_display() {
        args.push("-display".to_string());
        args.push("gtk,zoom-to-fit=off".to_string());
    }
    args
}

/// Whether this QEMU has the GTK display (its default when it does).
fn gtk_display() -> bool {
    Command::new("qemu-system-x86_64")
        .args(["-display", "help"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).lines().any(|l| l.trim() == "gtk" || l.split_whitespace().any(|w| w == "gtk")))
}

fn is_test_kernel(kernel: &Path) -> bool {
    let name = kernel.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let hashed = name.rsplit_once('-').is_some_and(|(_, h)| h.len() == 16 && h.bytes().all(|b| b.is_ascii_hexdigit()));
    hashed || kernel.components().any(|c| c.as_os_str() == "deps")
}

/// The OVMF firmware for UEFI booting: $OVMF, or where distributions put it.
fn ovmf() -> PathBuf {
    find_ovmf().unwrap_or_else(|| {
        eprintln!("palladium-runner: no OVMF firmware found for UEFI (install edk2-ovmf / ovmf, set OVMF=path, or use --bios)");
        exit(1)
    })
}

fn find_ovmf() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("OVMF") {
        return Some(PathBuf::from(path));
    }
    let candidates = [
        "/usr/share/edk2/x64/OVMF.4m.fd",
        "/usr/share/ovmf/x64/OVMF.4m.fd",
        "/usr/share/edk2-ovmf/x64/OVMF.4m.fd",
        "/usr/share/OVMF/OVMF.fd",
        "/usr/share/ovmf/OVMF.fd",
        "/usr/share/qemu/OVMF.fd",
    ];
    candidates.iter().map(PathBuf::from).find(|p| p.exists())
}

/// Extra QEMU arguments for some test kernels, by name, and each one's
/// network card: virtio-net for the usermode tests (where the network is
/// tested), the RTL8139 for the others.
/// A folder of this machine's shared with PalladiumOS (virtio-9p: it's
/// /host there). `cargo run`: PALLADIUM_SHARE, else shared/ here (made if
/// missing; git ignores it). The usermode tests: a fresh target/test-share
/// with a file in it. QEMU does the reading and writing, as you.
fn share_args(kernel: &Path, test: bool) -> Vec<String> {
    let name = kernel.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let dir = if test {
        if !name.starts_with("usermode-") {
            return Vec::new();
        }
        let dir = PathBuf::from("target/test-share");
        let _ = std::fs::remove_dir_all(&dir);
        if std::fs::create_dir_all(dir.join("sub")).is_err() || std::fs::write(dir.join("hello.txt"), "shared by the host\n").is_err() {
            return Vec::new();
        }
        dir
    } else {
        let dir = std::env::var_os("PALLADIUM_SHARE").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("shared"));
        if std::fs::create_dir_all(&dir).is_err() {
            eprintln!("palladium-runner: can't make the shared folder {}", dir.display());
            return Vec::new();
        }
        dir
    };
    let Ok(abs) = dir.canonicalize() else { return Vec::new() };
    let path = abs.to_string_lossy();
    if path.contains(',') {
        eprintln!("palladium-runner: the shared folder's path has a comma, which QEMU can't take: {}", path);
        return Vec::new();
    }
    vec!["-virtfs".to_string(), format!("local,path={},mount_tag=host,security_model=none,id=host", path)]
}

fn test_extra_args(kernel: &Path, test: bool) -> Vec<&'static str> {
    let name = kernel.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if !test {
        return Vec::new();
    }
    let mut args: Vec<&'static str> = if name.starts_with("ahci-") {
        vec!["-machine", "q35"] // q35 has its disks on AHCI
    } else if name.starts_with("ext2-") {
        vec!["-machine", "pc"] // its disks on IDE (it tests the IDE driver; KVM would pick q35)
    } else if name.starts_with("smp-") {
        vec!["-smp", "4"] // four CPU cores
    } else if name.starts_with("usermode-") {
        vec!["-vga", "none", "-device", "virtio-vga"] // the screen on a virtio GPU (X and /dev/fb0 tested there)
    } else if name.starts_with("usb-") {
        vec!["-device", "qemu-xhci", "-device", "usb-kbd", "-device", "usb-tablet", "-device", "usb-mouse"]
    } else {
        Vec::new()
    };
    // The usermode tests on virtio disks and a virtio network card; the
    // others on IDE/AHCI disks (they test those drivers) and the RTL8139.
    let virtio = name.starts_with("usermode-");
    let disks: [&'static str; 3] = if virtio {
        [
            "format=raw,file=target/test-disk.img,if=virtio",
            "format=raw,file=target/usr.img,if=virtio",
            "format=raw,file=target/test-scratch.img,if=virtio",
        ]
    } else {
        [
            "format=raw,file=target/test-disk.img,index=1,media=disk",
            "format=raw,file=target/usr.img,index=2,media=disk",
            "format=raw,file=target/test-scratch.img,index=3,media=disk",
        ]
    };
    for d in disks {
        args.extend(["-drive", d]);
    }
    let nic = if virtio { "virtio-net-pci,netdev=net0" } else { "rtl8139,netdev=net0" };
    args.extend(["-device", nic]);
    if virtio {
        args.extend(["-device", "virtio-rng-pci", "-device", "virtio-keyboard-pci", "-device", "virtio-tablet-pci"]);
    }
    args
}

/// KVM, when this machine has it (/dev/kvm usable) and PALLADIUM_NO_KVM
/// isn't set: QEMU runs PalladiumOS on the processor itself, many times
/// faster, as the host CPU (`-cpu host`: its features; programs still see
/// no AVX, since the kernel doesn't enable AVX state, OSXSAVE). Not when the
/// arguments choose an accelerator themselves.
///
/// And on the q35 machine (disks on AHCI) unless the arguments pick one:
/// under KVM, QEMU's IDE disks finish each PIO sector slowly (~20 ms here,
/// whatever the guest does), so a 34 MB write took minutes; AHCI is fast.
fn kvm_args(args: &[&str]) -> Vec<&'static str> {
    if std::env::var_os("PALLADIUM_NO_KVM").is_some()
        || args.iter().any(|a| *a == "-enable-kvm" || *a == "-accel" || a.contains("accel="))
        || std::fs::OpenOptions::new().read(true).write(true).open("/dev/kvm").is_err()
    {
        return Vec::new();
    }
    let mut out = vec!["-enable-kvm", "-cpu", "host"];
    if !args.iter().any(|a| *a == "-machine" || *a == "-M") {
        out.extend(["-machine", "q35"]);
    }
    out
}

fn run(kernel: &Path, extra: &[String], uefi: bool) {
    let test = is_test_kernel(kernel);
    let image = PathBuf::from(format!("{}{}.img", kernel.display(), if uefi { ".uefi" } else { "" }));
    make_image(kernel, &image, uefi);

    let mut qemu = Command::new("qemu-system-x86_64");
    if uefi {
        qemu.arg("-bios").arg(ovmf());
    }
    qemu.current_dir(repo_root(kernel))
        .arg("-drive")
        .arg(format!("format=raw,file={}", image.canonicalize().unwrap_or(image.clone()).display()))
        .arg("-no-reboot")
        .args(if test { TEST_ARGS } else { RUN_ARGS })
        .args(if test { Vec::new() } else { display_args(&extra) })
        .args(share_args(kernel, test))
        .args(test_extra_args(kernel, test))
        .args(kvm_args(&test_extra_args(kernel, test).iter().copied().chain(extra.iter().map(String::as_str)).collect::<Vec<_>>()))
        .args(extra);
    // `cargo run`: CPU cores, as many as this machine has, up to 4 (unless
    // the arguments say otherwise).
    if !test && !extra.iter().any(|a| a == "-smp") {
        let n = std::thread::available_parallelism().map_or(1, |n| n.get()).min(4);
        if n > 1 {
            qemu.arg("-smp").arg(n.to_string());
        }
    }
    // PALLADIUM_SMP=N: test kernels get N CPU cores (the usermode tests
    // then run on all of them).
    if test {
        if let Some(n) = std::env::var("PALLADIUM_SMP").ok().filter(|n| n.parse::<u32>().is_ok_and(|n| n > 1)) {
            if !test_extra_args(kernel, test).contains(&"-smp") {
                qemu.arg("-smp").arg(n);
            }
        }
    }

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
            Ok(None) if start.elapsed() > test_timeout() => {
                let _ = child.kill();
                eprintln!("palladium-runner: test timed out after {} s", test_timeout().as_secs());
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

// Copyright (c) 2026 tre4surehunter9
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(crate::test_runner)]
#![reexport_test_harness_main = "test_main"]

use core::panic::PanicInfo;

#[cfg(test)]
pub fn test_runner(tests: &[&dyn Testable]) {
    serial_println!("Running {} tests", tests.len());
    for test in tests {
        test.run();
    }
    exit_qemu(QemuExitCode::Success);
}

#[test_case]
fn trivial_assertion() {
    assert_eq!(1, 1);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum QemuExitCode {
    Success = 0x10,
    Failed = 0x11,
}

pub fn exit_qemu(exit_code: QemuExitCode) {
    use x86_64::instructions::port::Port;
    unsafe {
        let mut port = Port::new(0xf4);
        port.write(exit_code as u32)
    }
}

pub trait Testable {
    fn run(&self) -> ();
}

impl<T> Testable for T
where
T: Fn(),
{
    fn run(&self) {
        serial_print!("{}...\t", core::any::type_name::<T>());
        self();
        serial_println!("[ok]")
    }
}

// Use the library's modules: declaring them again here (`mod vga_buffer;`)
// would create a second screen writer with its own cursor position.
use palladiumos::{println, serial_print, serial_println};

use bootloader_api::{BootInfo, entry_point};
use palladiumos::hlt_loop;
use palladiumos::task::{Task, simple_executor::SimpleExecutor};

entry_point!(kernel_main, config = &palladiumos::BOOTLOADER_CONFIG);
extern crate alloc;
use alloc::{boxed::Box, vec, vec::Vec, rc::Rc};

    // start function
    fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
        use palladiumos::allocator;
        use palladiumos::memory;
        use palladiumos::memory::BootInfoFrameAllocator;
        use palladiumos::task::executor::Executor;
        use palladiumos::task::keyboard::run_shell;
        use x86_64::VirtAddr;

        use palladiumos::{boot_failed, boot_info, boot_ok};

        // The console draws into the bootloader's framebuffer (the kernel
        // keeps it for good); then the boot information is only read.
        if let Some(fb) = core::mem::replace(&mut boot_info.framebuffer, bootloader_api::info::Optional::None).into_option() {
            palladiumos::vga_buffer::init_framebuffer(fb);
        }
        let boot_info: &'static BootInfo = boot_info;
        {
            let (addr, stride, cols, rows) = palladiumos::vga_buffer::grid_info();
            serial_println!("console: {}x{} cells, grid at {:#x} ({} cells per row)", cols, rows, addr, stride);
        }
        // A Linux-style boot log (see boot.rs; `dmesg` shows it again).
        palladiumos::vga_buffer::clear_screen();
        palladiumos::boot::set_verbose(true);
        boot_info!("PalladiumOS {} (x86_64) is booting", palladiumos::VERSION);
        palladiumos::init();

        let mut frame_allocator = unsafe { BootInfoFrameAllocator::init(&boot_info.memory_regions) };
        let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset.into_option().expect("no physical memory mapping"));
        assert_eq!(phys_mem_offset.as_u64(), memory::PHYS_OFFSET, "BOOTLOADER_CONFIG's physical memory mapping changed?");
        let mut mapper = unsafe { memory::init(phys_mem_offset) };
        memory::PHYS_MEM_OFFSET.store(phys_mem_offset.as_u64(), core::sync::atomic::Ordering::Relaxed);
        boot_ok!("Set up paging (physical memory mapped at {:#x})", phys_mem_offset.as_u64());

        match allocator::init_heap(&mut mapper, &mut frame_allocator) {
            Ok(()) => boot_ok!("Mapped the kernel heap ({} MiB)", allocator::HEAP_SIZE / 1024 / 1024),
            Err(_) => {
                boot_failed!("Mapping the kernel heap: out of memory");
                panic!("heap initialization failed");
            }
        }
        boot_info!("CPU: {}", palladiumos::boot::cpu_name());
        boot_info!("CPU features: {}", palladiumos::boot::cpu_features());
        boot_info!("Memory: {} MiB usable", frame_allocator.total_frames() * 4096 / 1024 / 1024);



        // Hand the mapper/frame allocator off to memory::MAPPER / memory::FRAME_ALLOCATOR
        // so shell.rs's cmd_run can reach them later. This is why the ring-3
        // launch no longer happens here — kernel_main boots straight to the shell now.
        *memory::MAPPER.lock() = Some(mapper);
        *memory::FRAME_ALLOCATOR.lock() = Some(frame_allocator);
        palladiumos::process::init();
        boot_ok!("Started the scheduler (preemptive, up to {} processes)", palladiumos::process::MAX_PROCESSES);
        let cores = palladiumos::smp::init(boot_info.rsdp_addr.into_option());
        if cores > 1 {
            boot_ok!("Started {} CPU cores (the others wait: processes run on the first)", cores);
        }
        boot_ok!(
            "Kernel at {:#x} (higher half); lower half free for programs",
            kernel_main as *const () as u64
        );
        palladiumos::rtc::init();
        boot_ok!("Read the real-time clock: {}", palladiumos::boot::format_date(palladiumos::rtc::now()));
        // Keys pressed before anything reads the keyboard must not be lost.
        palladiumos::task::keyboard::init_queue();
        boot_ok!("Started the keyboard");
        if palladiumos::mouse::init() {
            boot_ok!("Started the PS/2 mouse (/dev/input/mice)");
        }
        match palladiumos::xhci::init() {
            Some(0) => boot_ok!("Found a USB controller (xHCI): no keyboard"),
            Some(n) => boot_ok!("Found a USB controller (xHCI): {} USB keyboard{}", n, if n == 1 { "" } else { "s" }),
            None => {}
        }
        mount_filesystems();
        start_network();
        let (count, bytes) = palladiumos::programs::install();
        boot_ok!("Installed {} programs in /bin ({} KiB)", count, bytes / 1024);
        if palladiumos::programs::find("busybox").is_some() {
            boot_ok!(
                "Made {} BusyBox tools the defaults (originals: {})",
                palladiumos::programs::BUSYBOX_PREFERRED.len(),
                palladiumos::programs::REPLACED_DIR
            );
        }
        palladiumos::filesystem::init_default_files();
        boot_ok!("Checked the system files (/etc: accounts, os-release)");
        if let Some(name) = palladiumos::syscall::load_hostname() {
            boot_ok!("Set the hostname to {} (/etc/hostname)", name);
        }

        #[cfg(test)]
        test_main();

        // Start the userland shell. If it exits (or can't start), fall back
        // to the built-in kernel shell.
        #[cfg(not(test))]
        start_userland_shell();

        let mut executor = Executor::new();
        executor.spawn(Task::new(run_shell()));
        executor.run();
    }

/// The network card and TCP/IP: DHCP on eth0 (QEMU's user network),
/// loopback on lo.
fn start_network() {
    use palladiumos::{boot_info, boot_ok, boot_warn, net};
    match net::init() {
        Some((mac, irq)) => {
            boot_info!(
                "eth0: RTL8139, MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}, IRQ {}",
                mac[0], mac[1], mac[2], mac[3], mac[4], mac[5], irq
            );
            // DHCP usually answers at once in QEMU; don't hold up the boot.
            match net::wait_for_dhcp(40) {
                Some(c) => boot_ok!(
                    "Configured eth0 by DHCP: {} (gateway {}, DNS {})",
                    c.address,
                    c.gateway.map_or("none".into(), |g| alloc::format!("{}", g)),
                    c.dns.map_or("none".into(), |d| alloc::format!("{}", d))
                ),
                None => {
                    net::use_static_config();
                    boot_warn!("No DHCP answer: eth0 is 10.0.2.15/24 (QEMU's default)");
                }
            }
            net::write_resolv_conf();
        }
        None => boot_warn!("No network card (QEMU: -device rtl8139): only loopback"),
    }
    boot_ok!("Started networking (TCP/IP: lo 127.0.0.1{})", if net::has_card() { ", eth0" } else { "" });
}

/// `/` is the ext2 disk if there is one (so files survive a reboot),
/// otherwise memory. /bin (the bundled programs, fresh every boot) and /tmp
/// are in memory, and /dev has the device files.
fn mount_filesystems() {
    use alloc::boxed::Box;
    use palladiumos::{block, boot_failed, boot_info, boot_ok, boot_warn, devfs::DevFs, ext2::Ext2, fdfs::Fdfs, mounts};

    let disk = block::data_disk();
    if let Some(d) = &disk {
        boot_info!("ata0-slave: {} MiB disk", d.sectors() * 512 / 1024 / 1024);
    }
    let root: Box<dyn mounts::FileSystem> = match disk.map(Ext2::mount) {
        Some(Ok(mut fs)) => {
            let stats = mounts::FileSystem::statfs(&mut fs);
            let used = (stats.blocks - stats.free_blocks) * stats.block_size / 1024 / 1024;
            let size = stats.blocks * stats.block_size / 1024 / 1024;
            let ro = if fs.is_read_only() { ", read-only" } else { "" };
            boot_ok!("Mounted the ext2 disk at / ({} of {} MiB used{})", used, size, ro);
            Box::new(fs)
        }
        Some(Err(e)) => {
            boot_failed!("Mounting the disk at /: {} (files will not be saved)", e);
            Box::new(Fdfs::new())
        }
        None => {
            boot_warn!("No disk attached: / is in memory, files will not be saved");
            Box::new(Fdfs::new())
        }
    };
    mounts::mount("/", root);
    for dir in ["/bin", "/tmp", "/dev", "/proc"] {
        let _ = palladiumos::filesystem::make_dir(dir); // mount points
    }
    mounts::mount("/bin", Box::new(Fdfs::new()));
    let mut tmp = Fdfs::new();
    let _ = mounts::FileSystem::chmod(&mut tmp, 0, 0o1777); // sticky, writable by everyone
    mounts::mount("/tmp", Box::new(tmp));
    boot_ok!("Mounted /bin and /tmp (in memory)");
    mounts::mount("/dev", Box::new(DevFs));
    boot_ok!("Mounted /dev (console, tty, null, zero, random, urandom)");
    palladiumos::block::mount_shm();
    boot_ok!("Mounted /dev/shm (shared memory, in memory)");
    mounts::mount("/proc", Box::new(palladiumos::procfs::ProcFs));
    boot_ok!("Mounted /proc (processes and system information)");
    if block::mount_system_disk() {
        boot_ok!("Mounted the system disk at /usr (read-only: C headers and libraries)");
    } else {
        boot_warn!("No system disk: /usr is empty (tcc has no headers or libraries)");
    }
    if let Some(mib) = block::mount_alpine_disk() {
        boot_ok!("Mounted the Alpine disk at /alpine ({} MiB)", mib);
    }
}

#[cfg(not(test))]
fn start_userland_shell() {
    // login asks who you are and starts your shell; when you log out, it
    // starts again. Ctrl-D at the login prompt (status 99) opens the kernel
    // shell. Without /bin/login, start sh directly.
    use palladiumos::{boot_failed, boot_info, boot_ok};
    const TO_KERNEL_SHELL: i32 = 99;
    let env = palladiumos::process::DEFAULT_ENV;
    let mut first = true;
    loop {
        let (bytes, argv) = match palladiumos::filesystem::read_bytes("/bin/login") {
            Ok(b) => (b, ["login"]),
            Err(_) => match palladiumos::filesystem::read_bytes("/bin/sh") {
                Ok(b) => (b, ["sh"]),
                Err(e) => {
                    boot_failed!("Starting a shell: can't read /bin/login or /bin/sh ({}); using the kernel shell", e);
                    return;
                }
            },
        };
        if first {
            boot_ok!("Reached the {} prompt", if argv[0] == "login" { "login" } else { "shell" });
            boot_info!("Boot finished. `dmesg` shows this log again.");
            first = false;
        }
        match palladiumos::process::run_program_env(&bytes, &argv, env) {
            Ok(TO_KERNEL_SHELL) => {
                println!("This is the kernel shell; type 'run login' to log in again.");
                return;
            }
            Ok(_) if argv[0] == "login" => {} // logged out: ask again
            Ok(status) => {
                println!("\nsh exited with status {}. This is the kernel shell.", status);
                return;
            }
            Err(e) => {
                boot_failed!("Starting {}: {}; using the kernel shell", argv[0], e);
                return;
            }
        }
    }
}

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("{}", info);
    palladiumos::hlt_loop();
}

#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    palladiumos::test_panic_handler(info)
}

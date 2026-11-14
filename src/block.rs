// Copyright (c) 2026 tre4surehunter9
//! Block devices: things made of 512-byte sectors that filesystems live on.
//!
//! - `AtaDisk`: an IDE/ATA hard disk, using PIO (the CPU copies every word
//!   through I/O ports). Simple, and what QEMU's default machine has.
//! - `ahci::AhciDisk` (src/ahci.rs): a SATA disk on an AHCI controller.
//! - `RamDisk`: a disk image held in memory (used by the tests).

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;
use x86_64::instructions::port::Port;

pub const SECTOR_SIZE: usize = 512;
/// The most sectors one ATA command moves (a sector count of 0 means 256).
const MAX_SECTORS_PER_COMMAND: usize = 256;

pub trait BlockDevice: Send {
    /// Number of 512-byte sectors.
    fn sectors(&self) -> u64;
    /// Read `buf.len() / 512` sectors starting at `sector`.
    fn read(&mut self, sector: u64, buf: &mut [u8]) -> Result<(), &'static str>;
    /// Write `data.len() / 512` sectors starting at `sector`.
    fn write(&mut self, sector: u64, data: &[u8]) -> Result<(), &'static str>;
    /// Make sure written data has reached the disk.
    fn flush(&mut self) -> Result<(), &'static str> {
        Ok(())
    }
}

// ---------------------------------------------------------------- RAM disk

pub struct RamDisk {
    data: Vec<u8>,
}

impl RamDisk {
    pub fn new(image: &[u8]) -> Self {
        let mut data = image.to_vec();
        data.resize(data.len().next_multiple_of(SECTOR_SIZE), 0);
        RamDisk { data }
    }
}

impl BlockDevice for RamDisk {
    fn sectors(&self) -> u64 {
        (self.data.len() / SECTOR_SIZE) as u64
    }

    fn read(&mut self, sector: u64, buf: &mut [u8]) -> Result<(), &'static str> {
        let start = sector as usize * SECTOR_SIZE;
        let src = self.data.get(start..start + buf.len()).ok_or("read past the end of the disk")?;
        buf.copy_from_slice(src);
        Ok(())
    }

    fn write(&mut self, sector: u64, data: &[u8]) -> Result<(), &'static str> {
        let start = sector as usize * SECTOR_SIZE;
        let dst = self.data.get_mut(start..start + data.len()).ok_or("write past the end of the disk")?;
        dst.copy_from_slice(data);
        Ok(())
    }
}

// ---------------------------------------------------------------- ATA PIO

// Status register bits
const BSY: u8 = 0x80;
const DF: u8 = 0x20;
const DRQ: u8 = 0x08;
const ERR: u8 = 0x01;

/// How many times to poll the status register before giving up. QEMU
/// finishes disk I/O on another thread, so on a busy host a request can
/// take a while; a dead disk still times out (in seconds).
const POLL_LIMIT: u32 = 100_000_000;

const CMD_READ_SECTORS: u8 = 0x20;
const CMD_WRITE_SECTORS: u8 = 0x30;
const CMD_CACHE_FLUSH: u8 = 0xE7;
const CMD_IDENTIFY: u8 = 0xEC;

pub struct AtaDisk {
    io: u16,
    control: u16,
    slave: bool,
    sectors: u64,
}

impl AtaDisk {
    /// The disk on the primary IDE bus in the master position (QEMU's
    /// `index=0`, usually the boot disk).
    pub fn primary_master() -> Option<AtaDisk> {
        AtaDisk::probe(0x1F0, 0x3F6, false)
    }

    /// The disk on the primary IDE bus in the slave position (QEMU puts a
    /// `-drive ...,index=1` there; the boot disk is the master). Returns
    /// None if there's no disk.
    pub fn primary_slave() -> Option<AtaDisk> {
        AtaDisk::probe(0x1F0, 0x3F6, true)
    }

    /// The master disk on the secondary IDE bus (QEMU's `index=2`).
    pub fn secondary_master() -> Option<AtaDisk> {
        AtaDisk::probe(0x170, 0x376, false)
    }

    /// The slave disk on the secondary IDE bus (QEMU's `index=3`).
    pub fn secondary_slave() -> Option<AtaDisk> {
        AtaDisk::probe(0x170, 0x376, true)
    }

    fn probe(io: u16, control: u16, slave: bool) -> Option<AtaDisk> {
        let mut disk = AtaDisk { io, control, slave, sectors: 0 };
        unsafe {
            // nIEN: no interrupts from the device (we poll; IRQ 14 has no handler).
            Port::<u8>::new(control).write(0x02);
            disk.select(0);
            for reg in 2..=5 {
                Port::<u8>::new(io + reg).write(0);
            }
            Port::<u8>::new(io + 7).write(CMD_IDENTIFY);
            if disk.status() == 0 || disk.status() == 0xFF {
                return None; // nothing there (floating bus)
            }
            // Not an ATA disk if the signature bytes are set (ATAPI, SATA).
            let mut spins = 0;
            while disk.status() & BSY != 0 {
                spins += 1;
                if spins > 1_000_000 {
                    return None;
                }
            }
            if Port::<u8>::new(io + 4).read() != 0 || Port::<u8>::new(io + 5).read() != 0 {
                return None;
            }
            if disk.wait_drq().is_err() {
                return None;
            }
            let mut ident = [0u16; 256];
            let mut data: Port<u16> = Port::new(io);
            for w in ident.iter_mut() {
                *w = data.read();
            }
            disk.sectors = ident[60] as u64 | (ident[61] as u64) << 16; // LBA28 sector count
        }
        if disk.sectors == 0 {
            return None;
        }
        Some(disk)
    }

    fn status(&self) -> u8 {
        unsafe { Port::<u8>::new(self.io + 7).read() }
    }

    /// Select the drive and set the top 4 bits of the LBA.
    fn select(&self, lba: u64) {
        let drive = 0xE0 | if self.slave { 0x10 } else { 0 } | ((lba >> 24) & 0x0F) as u8;
        unsafe {
            Port::<u8>::new(self.io + 6).write(drive);
            // Reading the alternate status 4 times waits the required 400 ns.
            for _ in 0..4 {
                Port::<u8>::new(self.control).read();
            }
        }
    }

    fn wait_ready(&self) -> Result<(), &'static str> {
        for _ in 0..POLL_LIMIT {
            let s = self.status();
            if s & BSY == 0 {
                if s & (ERR | DF) != 0 {
                    return Err("disk error");
                }
                return Ok(());
            }
        }
        Err("disk timeout")
    }

    fn wait_drq(&self) -> Result<(), &'static str> {
        for _ in 0..POLL_LIMIT {
            let s = self.status();
            if s & (ERR | DF) != 0 {
                return Err("disk error");
            }
            if s & BSY == 0 && s & DRQ != 0 {
                return Ok(());
            }
        }
        Err("disk timeout")
    }

    /// Start a transfer of `count` sectors (1..=256; 256 is sent as 0).
    fn command(&self, lba: u64, count: usize, cmd: u8) -> Result<(), &'static str> {
        if lba + count as u64 > self.sectors || lba + count as u64 > 1 << 28 {
            return Err("sector out of range");
        }
        self.wait_ready()?;
        self.select(lba);
        unsafe {
            Port::<u8>::new(self.io + 2).write(count as u8); // (256 -> 0)
            Port::<u8>::new(self.io + 3).write(lba as u8);
            Port::<u8>::new(self.io + 4).write((lba >> 8) as u8);
            Port::<u8>::new(self.io + 5).write((lba >> 16) as u8);
            Port::<u8>::new(self.io + 7).write(cmd);
        }
        Ok(())
    }
}

impl BlockDevice for AtaDisk {
    fn sectors(&self) -> u64 {
        self.sectors
    }

    /// Up to 256 sectors per command; each sector moves with one string
    /// instruction (rep insw), not 256 separate port reads.
    fn read(&mut self, sector: u64, buf: &mut [u8]) -> Result<(), &'static str> {
        assert!(buf.len() % SECTOR_SIZE == 0, "partial sector read");
        let mut done = 0;
        for batch in buf.chunks_mut(SECTOR_SIZE * MAX_SECTORS_PER_COMMAND) {
            let count = batch.len() / SECTOR_SIZE;
            self.command(sector + done, count, CMD_READ_SECTORS)?;
            for chunk in batch.chunks_mut(SECTOR_SIZE) {
                crate::interrupts::let_in(); // (keys typed meanwhile aren't lost)
                self.wait_drq()?;
                unsafe {
                    core::arch::asm!("rep insw", in("dx") self.io, inout("rdi") chunk.as_mut_ptr() => _,
                        inout("rcx") SECTOR_SIZE / 2 => _, options(nostack, preserves_flags));
                }
            }
            done += count as u64;
        }
        Ok(())
    }

    fn write(&mut self, sector: u64, data: &[u8]) -> Result<(), &'static str> {
        assert!(data.len() % SECTOR_SIZE == 0, "partial sector write");
        let mut done = 0;
        for batch in data.chunks(SECTOR_SIZE * MAX_SECTORS_PER_COMMAND) {
            let count = batch.len() / SECTOR_SIZE;
            self.command(sector + done, count, CMD_WRITE_SECTORS)?;
            for chunk in batch.chunks(SECTOR_SIZE) {
                crate::interrupts::let_in();
                self.wait_drq()?;
                unsafe {
                    core::arch::asm!("rep outsw", in("dx") self.io, inout("rsi") chunk.as_ptr() => _,
                        inout("rcx") SECTOR_SIZE / 2 => _, options(nostack, preserves_flags, readonly));
                }
            }
            self.wait_ready()?;
            done += count as u64;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), &'static str> {
        self.wait_ready()?;
        self.select(0);
        unsafe { Port::<u8>::new(self.io + 7).write(CMD_CACHE_FLUSH) };
        self.wait_ready()
    }
}

/// Disk number `index`, as QEMU numbers `-drive ...,index=N`: the IDE
/// position N (0 primary master, 1 primary slave, 2 secondary master,
/// 3 secondary slave) on PCs with IDE, or SATA port N on an AHCI controller
/// (QEMU's q35 machine, and most real PCs). Each disk is handed out once.
pub fn disk(index: u8) -> Option<Box<dyn BlockDevice>> {
    let ide = match index {
        0 => AtaDisk::primary_master(),
        1 => AtaDisk::primary_slave(),
        2 => AtaDisk::secondary_master(),
        3 => AtaDisk::secondary_slave(),
        _ => None,
    };
    match ide {
        Some(d) => Some(Box::new(d)),
        // Then AHCI port N; then virtio disks, after the boot disk (N >= 1:
        // the (N-1)th).
        None => crate::ahci::boxed(index).or_else(|| index.checked_sub(1).and_then(|n| crate::virtio_blk::disk(n as usize))),
    }
}

// ---------------------------------------------------------------- partitions

/// A part of a disk, as a block device of its own. Several partitions can
/// share one disk.
pub struct Partition {
    disk: Arc<Mutex<Box<dyn BlockDevice>>>,
    start: u64,
    sectors: u64,
}

impl BlockDevice for Partition {
    fn sectors(&self) -> u64 {
        self.sectors
    }

    fn read(&mut self, sector: u64, buf: &mut [u8]) -> Result<(), &'static str> {
        if sector + (buf.len() / SECTOR_SIZE) as u64 > self.sectors {
            return Err("read past the end of the partition");
        }
        self.disk.lock().read(self.start + sector, buf)
    }

    fn write(&mut self, sector: u64, data: &[u8]) -> Result<(), &'static str> {
        if sector + (data.len() / SECTOR_SIZE) as u64 > self.sectors {
            return Err("write past the end of the partition");
        }
        self.disk.lock().write(self.start + sector, data)
    }

    fn flush(&mut self) -> Result<(), &'static str> {
        self.disk.lock().flush()
    }
}

/// The partitions of a GPT-partitioned disk: (name, first sector, sectors).
pub fn gpt_partitions(disk: &mut dyn BlockDevice) -> Vec<(String, u64, u64)> {
    let mut found = Vec::new();
    let mut header = [0u8; SECTOR_SIZE];
    if disk.read(1, &mut header).is_err() || &header[0..8] != b"EFI PART" {
        return found;
    }
    let u32_at = |b: &[u8], o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    let u64_at = |b: &[u8], o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap());
    let entries_lba = u64_at(&header, 72);
    let count = u32_at(&header, 80).min(256) as usize;
    let size = u32_at(&header, 84) as usize;
    if !(128..=SECTOR_SIZE).contains(&size) || SECTOR_SIZE % size != 0 {
        return found;
    }
    let mut table = alloc::vec![0u8; (count * size).next_multiple_of(SECTOR_SIZE)];
    if disk.read(entries_lba, &mut table).is_err() {
        return found;
    }
    for entry in table.chunks(size).take(count) {
        if entry[0..16].iter().all(|&b| b == 0) {
            continue; // unused entry
        }
        let (first, last) = (u64_at(entry, 32), u64_at(entry, 40));
        if last < first || last >= disk.sectors() {
            continue;
        }
        let name: String = char::decode_utf16(
            entry[56..128].chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&c| c != 0),
        )
        .map(|c| c.unwrap_or('?'))
        .collect();
        found.push((name, first, last - first + 1));
    }
    found
}

/// PalladiumOS's partitions (named palladium-...) on any disk, found once.
/// Disks without any are left alone (and free for disk(N)).
static NAMED: Mutex<Option<Vec<(String, Partition, String)>>> = Mutex::new(None);

/// Linux's name for a disk: hda..hdd for IDE positions 0..3, sda, sdb...
/// for AHCI ports.
fn disk_name(index: u8, ide: bool) -> String {
    alloc::format!("/dev/{}{}", if ide { "hd" } else { "sd" }, (b'a' + index) as char)
}

/// Disk `index` (as disk()), with its Linux name.
fn disk_named(index: u8) -> Option<(Box<dyn BlockDevice>, String)> {
    disk_named_uncounted(index).map(|(d, name)| (counted(d, &name), name))
}

fn disk_named_uncounted(index: u8) -> Option<(Box<dyn BlockDevice>, String)> {
    let ide = match index {
        0 => AtaDisk::primary_master(),
        1 => AtaDisk::primary_slave(),
        2 => AtaDisk::secondary_master(),
        3 => AtaDisk::secondary_slave(),
        _ => None,
    };
    match ide {
        Some(d) => Some((Box::new(d), disk_name(index, true))),
        None => crate::ahci::boxed(index).map(|d| (d, disk_name(index, false))).or_else(|| {
            // A virtio disk: /dev/vda, vdb...
            let n = index.checked_sub(1)?;
            crate::virtio_blk::disk(n as usize).map(|d| (d, alloc::format!("/dev/vd{}", (b'a' + n) as char)))
        }),
    }
}

fn take_named(name: &str) -> Option<(Box<dyn BlockDevice>, String)> {
    let mut named = NAMED.lock();
    let list = named.get_or_insert_with(|| {
        let mut list = Vec::new();
        let ahci_ports = crate::ahci::ports_with_disks();
        let candidates = (0..4u8)
            .filter_map(|i| {
                match i {
                    0 => AtaDisk::primary_master(),
                    1 => AtaDisk::primary_slave(),
                    2 => AtaDisk::secondary_master(),
                    _ => AtaDisk::secondary_slave(),
                }
                .map(|d| (Box::new(d) as Box<dyn BlockDevice>, disk_name(i, true)))
            })
            .chain(ahci_ports.into_iter().filter_map(|p| crate::ahci::boxed(p).map(|d| (d, disk_name(p, false)))));
        for (mut disk, dev) in candidates {
            // (Numbered as on Linux: their place in the partition table.)
            let parts: Vec<_> = gpt_partitions(disk.as_mut())
                .into_iter()
                .enumerate()
                .filter(|(_, p)| p.0.starts_with("palladium-"))
                .collect();
            if parts.is_empty() {
                continue; // (dropped: an AHCI port is handed back)
            }
            let shared = Arc::new(Mutex::new(counted(disk, &dev)));
            for (i, (pname, start, sectors)) in parts {
                crate::serial_println!("partition {}: sectors {}..+{}", pname, start, sectors);
                let devname = alloc::format!("{}{}", dev, i + 1);
                list.push((pname, Partition { disk: shared.clone(), start, sectors }, devname));
            }
        }
        list
    });
    let i = list.iter().position(|(n, _, _)| n == name)?;
    let (_, part, devname) = list.remove(i);
    Some((Box::new(part), devname))
}

/// The data disk, mounted at /: a partition named palladium-data on any
/// disk (see `palladium-runner usb`), or else disk 1.
/// (With its device name, for /proc/mounts.)
pub fn data_disk() -> Option<(Box<dyn BlockDevice>, String)> {
    take_named("palladium-data").or_else(|| disk_named(1))
}

/// The system disk: the files under /usr (C headers and libraries for
/// tcc), made by build.rs as target/usr.img. palladium-usr, or disk 2.
pub fn system_disk() -> Option<(Box<dyn BlockDevice>, String)> {
    take_named("palladium-usr").or_else(|| disk_named(2))
}

/// Mount the Alpine disk (palladium-alpine, or disk 3: target/alpine.img
/// from build.rs) at /alpine, if there is one: room for an Alpine Linux
/// root filesystem (see /bin/alpine). Returns its size in MiB.
pub fn mount_alpine_disk() -> Option<u64> {
    let (dev, name) = take_named("palladium-alpine").or_else(|| disk_named(3))?;
    let mib = dev.sectors() * 512 / 1024 / 1024;
    let fs = crate::ext2::Ext2::mount(dev).ok()?;
    let _ = crate::filesystem::make_dir("/alpine");
    crate::mounts::mount("/alpine", Box::new(fs));
    crate::mounts::set_device("/alpine", &name);
    Some(mib)
}

/// Mount the system disk read-only at /usr, if there is one with files.
pub fn mount_system_disk() -> bool {
    let Some((dev, name)) = system_disk() else { return false };
    let Ok(mut fs) = crate::ext2::Ext2::mount(dev) else { return false };
    fs.make_read_only();
    let _ = crate::filesystem::make_dir("/usr");
    crate::mounts::mount("/usr", Box::new(fs));
    crate::mounts::set_device("/usr", &name);
    // Programs on it that scripts name by a /bin path (#!/bin/bash).
    for prog in ["bash"] {
        let (target, link) = (alloc::format!("/usr/bin/{}", prog), alloc::format!("/bin/{}", prog));
        if crate::mounts::resolve(&target).is_ok() && crate::mounts::lstat(&link).is_err() {
            let _ = crate::mounts::symlink(&target, &link);
        }
    }
    true
}

/// The host's shared folder (virtio-9p), mounted at /host. Returns its
/// mount tag.
pub fn mount_host() -> Option<alloc::string::String> {
    let (fs, tag) = crate::virtio_9p::init()?;
    let _ = crate::filesystem::make_dir("/host");
    crate::mounts::mount("/host", Box::new(fs));
    crate::mounts::set_device("/host", &tag);
    Some(tag)
}

/// /sys: what programs read there about the machine, in memory. So far
/// the CPU cores (/sys/devices/system/cpu: cpu0, cpu1... and online,
/// possible, present: "0-N"), as htop and libc's get_nprocs_conf count
/// them.
pub fn mount_sys() {
    use crate::filesystem::{make_dir, write_file};
    let _ = make_dir("/sys");
    crate::mounts::mount("/sys", Box::new(crate::fdfs::Fdfs::named("sysfs")));
    let n = crate::smp::cores().max(1);
    let cpu = "/sys/devices/system/cpu";
    for dir in ["/sys/devices", "/sys/devices/system", cpu] {
        let _ = make_dir(dir);
    }
    let range = if n == 1 { alloc::string::String::from("0\n") } else { alloc::format!("0-{}\n", n - 1) };
    for file in ["online", "possible", "present"] {
        let path = alloc::format!("{}/{}", cpu, file);
        let _ = write_file(&path, &range);
        let _ = crate::filesystem::chmod(&path, 0o444);
    }
    for i in 0..n {
        let _ = make_dir(&alloc::format!("{}/cpu{}", cpu, i));
        if i > 0 {
            // (cpu0 can't be taken offline: Linux has no "online" for it.)
            let path = alloc::format!("{}/cpu{}/online", cpu, i);
            let _ = write_file(&path, "1\n");
            let _ = crate::filesystem::chmod(&path, 0o444);
        }
    }
}

/// Mount an in-memory filesystem at /dev/shm, open to everyone (sticky,
/// like /tmp): where shm_open and named semaphores keep their files.
pub fn mount_shm() {
    crate::mounts::mount("/dev/shm", Box::new(crate::fdfs::Fdfs::new()));
    let _ = crate::filesystem::chmod("/dev/shm", 0o1777);
}

// ---------------------------------------------------------------- statistics

/// A disk's reads and writes (whole requests and 512-byte sectors), for
/// /proc/diskstats (htop's disk meter, iostat, vmstat -d).
#[derive(Default)]
pub struct DiskStats {
    pub reads: core::sync::atomic::AtomicU64,
    pub sectors_read: core::sync::atomic::AtomicU64,
    pub writes: core::sync::atomic::AtomicU64,
    pub sectors_written: core::sync::atomic::AtomicU64,
}

/// The disks counted: (name without /dev/, its counts).
static DISK_STATS: Mutex<Vec<(String, Arc<DiskStats>)>> = Mutex::new(Vec::new());

/// The disks' counts, for /proc/diskstats.
pub fn disk_stats() -> Vec<(String, Arc<DiskStats>)> {
    DISK_STATS.lock().clone()
}

/// A disk whose reads and writes are counted.
struct Counted {
    inner: Box<dyn BlockDevice>,
    stats: Arc<DiskStats>,
}

fn counted(inner: Box<dyn BlockDevice>, devname: &str) -> Box<dyn BlockDevice> {
    let name = String::from(devname.trim_start_matches("/dev/"));
    let stats = {
        let mut all = DISK_STATS.lock();
        match all.iter().find(|(n, _)| *n == name) {
            Some((_, s)) => s.clone(),
            None => {
                let s = Arc::new(DiskStats::default());
                all.push((name, s.clone()));
                s
            }
        }
    };
    Box::new(Counted { inner, stats })
}

impl BlockDevice for Counted {
    fn sectors(&self) -> u64 {
        self.inner.sectors()
    }

    fn read(&mut self, sector: u64, buf: &mut [u8]) -> Result<(), &'static str> {
        use core::sync::atomic::Ordering::Relaxed;
        self.stats.reads.fetch_add(1, Relaxed);
        self.stats.sectors_read.fetch_add((buf.len() / SECTOR_SIZE) as u64, Relaxed);
        self.inner.read(sector, buf)
    }

    fn write(&mut self, sector: u64, data: &[u8]) -> Result<(), &'static str> {
        use core::sync::atomic::Ordering::Relaxed;
        self.stats.writes.fetch_add(1, Relaxed);
        self.stats.sectors_written.fetch_add((data.len() / SECTOR_SIZE) as u64, Relaxed);
        self.inner.write(sector, data)
    }

    fn flush(&mut self) -> Result<(), &'static str> {
        self.inner.flush()
    }
}

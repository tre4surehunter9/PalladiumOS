// Copyright (c) 2026 tre4surehunter9
//! Block devices: things made of 512-byte sectors that filesystems live on.
//!
//! - `AtaDisk`: an IDE/ATA hard disk, using PIO (the CPU copies every word
//!   through I/O ports). Simple and works everywhere, including QEMU.
//! - `RamDisk`: a disk image held in memory (used by the tests).

use alloc::boxed::Box;
use alloc::vec::Vec;
use x86_64::instructions::port::Port;

pub const SECTOR_SIZE: usize = 512;

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

    fn command(&self, lba: u64, cmd: u8) -> Result<(), &'static str> {
        if lba >= self.sectors || lba >= 1 << 28 {
            return Err("sector out of range");
        }
        self.wait_ready()?;
        self.select(lba);
        unsafe {
            Port::<u8>::new(self.io + 2).write(1); // one sector
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

    fn read(&mut self, sector: u64, buf: &mut [u8]) -> Result<(), &'static str> {
        for (i, chunk) in buf.chunks_mut(SECTOR_SIZE).enumerate() {
            self.command(sector + i as u64, CMD_READ_SECTORS)?;
            self.wait_drq()?;
            let mut data: Port<u16> = Port::new(self.io);
            for pair in chunk.chunks_mut(2) {
                let w = unsafe { data.read() };
                pair[0] = w as u8;
                pair[1] = (w >> 8) as u8;
            }
        }
        Ok(())
    }

    fn write(&mut self, sector: u64, data: &[u8]) -> Result<(), &'static str> {
        for (i, chunk) in data.chunks(SECTOR_SIZE).enumerate() {
            self.command(sector + i as u64, CMD_WRITE_SECTORS)?;
            self.wait_drq()?;
            let mut port: Port<u16> = Port::new(self.io);
            for pair in chunk.chunks(2) {
                unsafe { port.write(pair[0] as u16 | (pair[1] as u16) << 8) };
            }
            self.wait_ready()?;
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

/// Probe for the data disk (see AtaDisk::primary_slave).
pub fn data_disk() -> Option<Box<dyn BlockDevice>> {
    AtaDisk::primary_slave().map(|d| Box::new(d) as Box<dyn BlockDevice>)
}

/// Probe for the system disk: the files under /usr (C headers and
/// libraries for tcc), made by build.rs as target/usr.img.
pub fn system_disk() -> Option<Box<dyn BlockDevice>> {
    AtaDisk::secondary_master().map(|d| Box::new(d) as Box<dyn BlockDevice>)
}

/// Mount the system disk read-only at /usr, if there is one with files.
pub fn mount_system_disk() -> bool {
    let Some(dev) = system_disk() else { return false };
    let Ok(mut fs) = crate::ext2::Ext2::mount(dev) else { return false };
    fs.make_read_only();
    let _ = crate::filesystem::make_dir("/usr");
    crate::mounts::mount("/usr", Box::new(fs));
    true
}

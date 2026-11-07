// Copyright (c) 2026 tre4surehunter9
//! AHCI: the standard SATA controller interface, what most PCs from the
//! last 15 years use for their disks (and QEMU's q35 machine, ich9-ahci).
//!
//! The controller is a PCI device (class 1, subclass 6) whose registers are
//! memory-mapped at BAR 5. Each of up to 32 ports may have a disk. A command
//! is a small table in memory (the ATA command as a "FIS", plus where the
//! data goes); the kernel points the port at it, sets a bit, and the
//! controller moves the data by DMA. This driver polls (no interrupts), one
//! command at a time per port, and copies through a bounce buffer, like the
//! ATA driver it sits next to.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ptr::{read_volatile, write_volatile};
use spin::Mutex;

use crate::block::{BlockDevice, SECTOR_SIZE};
use crate::memory;

/// Sectors per command: the bounce buffer's size.
const MAX_SECTORS: usize = 256;
const BOUNCE_PAGES: usize = MAX_SECTORS * SECTOR_SIZE / 4096;
const POLL_LIMIT: u32 = 50_000_000;

// Generic host control registers.
const CAP: usize = 0x00;
const GHC: usize = 0x04;
const PI: usize = 0x0C;
const CAP2: usize = 0x24;
const BOHC: usize = 0x28;
const GHC_AE: u32 = 1 << 31; // AHCI enable
const CAP_SSS: u32 = 1 << 27; // staggered spin-up

// Port registers (at 0x100 + 0x80 * port).
const P_CLB: usize = 0x00;
const P_CLBU: usize = 0x04;
const P_FB: usize = 0x08;
const P_FBU: usize = 0x0C;
const P_IS: usize = 0x10;
const P_IE: usize = 0x14;
const P_CMD: usize = 0x18;
const P_TFD: usize = 0x20;
const P_SIG: usize = 0x24;
const P_SSTS: usize = 0x28;
const P_SERR: usize = 0x30;
const P_CI: usize = 0x38;
const CMD_ST: u32 = 1 << 0; // start processing commands
const CMD_SUD: u32 = 1 << 1; // spin up
const CMD_POD: u32 = 1 << 2; // power on
const CMD_FRE: u32 = 1 << 4; // receive FISes
const CMD_FR: u32 = 1 << 14;
const CMD_CR: u32 = 1 << 15;
const IS_TFES: u32 = 1 << 30; // task file error
const TFD_BSY: u32 = 0x80;
const TFD_DRQ: u32 = 0x08;
const TFD_ERR: u32 = 0x01;
const SIG_ATA: u32 = 0x0000_0101;

// ATA commands.
const ATA_READ_DMA_EXT: u8 = 0x25;
const ATA_WRITE_DMA_EXT: u8 = 0x35;
const ATA_FLUSH_CACHE_EXT: u8 = 0xEA;
const ATA_IDENTIFY: u8 = 0xEC;

// Layout of a port's page: command list (32 headers of 32 bytes, 1 KiB
// aligned), received FISes (256 bytes, 256-aligned), one command table
// (128-aligned: the FIS area, then the PRD table).
const CL_OFF: u64 = 0x000;
const FB_OFF: u64 = 0x400;
const CT_OFF: u64 = 0x500;

/// The controller: its registers, and which ports have been handed out.
struct Controller {
    abar: usize,
    implemented: u32,
    claimed: u32,
}

static CONTROLLER: Mutex<Option<Controller>> = Mutex::new(None);
static PROBED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

fn rd(addr: usize) -> u32 {
    unsafe { read_volatile(addr as *const u32) }
}

fn wr(addr: usize, v: u32) {
    unsafe { write_volatile(addr as *mut u32, v) }
}

/// Find and set up the first AHCI controller (once).
fn probe() {
    if PROBED.swap(true, core::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let Some(pci) = crate::net::pci::scan().into_iter().find(|d| d.class == 0x01 && d.subclass == 0x06) else {
        return;
    };
    let bar = pci.bar(5);
    if bar & 1 != 0 || bar & !0xF == 0 {
        return; // not a memory BAR
    }
    pci.enable();
    let abar = memory::map_mmio(u64::from(bar & !0xF), 0x1100) as usize;
    // Take the controller from the firmware, if it says it owns it.
    if rd(abar + CAP2) & 1 != 0 {
        wr(abar + BOHC, rd(abar + BOHC) | 1 << 1);
        let mut spins = 0;
        while rd(abar + BOHC) & 1 != 0 && spins < POLL_LIMIT {
            spins += 1;
            core::hint::spin_loop();
        }
    }
    wr(abar + GHC, rd(abar + GHC) | GHC_AE);
    let implemented = rd(abar + PI);
    *CONTROLLER.lock() = Some(Controller { abar, implemented, claimed: 0 });
    crate::boot_ok!("Found an AHCI (SATA) controller: {} ports", implemented.count_ones());
}

/// The disk on AHCI port `port`, if there is a SATA disk there (each port
/// is handed out once).
pub fn disk(port: u8) -> Option<AhciDisk> {
    probe();
    let mut guard = CONTROLLER.lock();
    let ctl = guard.as_mut()?;
    let bit = 1u32.checked_shl(u32::from(port))?;
    if ctl.implemented & bit == 0 || ctl.claimed & bit != 0 {
        return None;
    }
    let disk = AhciDisk::init(port, ctl.abar + 0x100 + 0x80 * port as usize, rd(ctl.abar + CAP))?;
    ctl.claimed |= bit;
    Some(disk)
}

/// Every SATA disk on the controller, by port number.
pub fn ports_with_disks() -> Vec<u8> {
    probe();
    let guard = CONTROLLER.lock();
    let Some(ctl) = guard.as_ref() else { return Vec::new() };
    (0..32u8)
        .filter(|&p| ctl.implemented & (1 << p) != 0)
        .filter(|&p| {
            let port = ctl.abar + 0x100 + 0x80 * p as usize;
            rd(port + P_SSTS) & 0xF == 3 && rd(port + P_SIG) == SIG_ATA
        })
        .collect()
}

pub struct AhciDisk {
    /// The port's number, and its registers' address.
    number: u8,
    port: usize,
    /// Physical address of the port's page (command list, FISes, table).
    page: u64,
    /// Physical address of the bounce buffer.
    bounce: u64,
    sectors: u64,
}

impl AhciDisk {
    fn init(number: u8, port: usize, cap: u32) -> Option<AhciDisk> {
        // Power on and spin up the device, if the controller can control that.
        if cap & CAP_SSS != 0 {
            wr(port + P_CMD, rd(port + P_CMD) | CMD_POD | CMD_SUD);
        }
        let mut spins = 0;
        while rd(port + P_SSTS) & 0xF != 3 {
            spins += 1;
            if spins > 1_000_000 {
                return None; // no device (or no link)
            }
            core::hint::spin_loop();
        }
        if rd(port + P_SIG) != SIG_ATA {
            return None; // ATAPI (CD drive), port multiplier, ...
        }
        // Stop the port before pointing it at new memory.
        if !stop(port) {
            return None;
        }
        let (page, bounce) = {
            let mut alloc = memory::FRAME_ALLOCATOR.lock();
            let frames = alloc.as_mut()?;
            let page = frames.allocate_contiguous(1)?.start_address().as_u64();
            let bounce = frames.allocate_contiguous(BOUNCE_PAGES)?.start_address().as_u64();
            (page, bounce)
        };
        unsafe { core::ptr::write_bytes(memory::phys_to_virt(page) as *mut u8, 0, 4096) };
        let cl = page + CL_OFF;
        let fb = page + FB_OFF;
        wr(port + P_CLB, cl as u32);
        wr(port + P_CLBU, (cl >> 32) as u32);
        wr(port + P_FB, fb as u32);
        wr(port + P_FBU, (fb >> 32) as u32);
        wr(port + P_SERR, u32::MAX); // clear old errors
        wr(port + P_IS, u32::MAX);
        wr(port + P_IE, 0); // polled
        wr(port + P_CMD, rd(port + P_CMD) | CMD_FRE);
        wr(port + P_CMD, rd(port + P_CMD) | CMD_ST);

        let mut disk = AhciDisk { number, port, page, bounce, sectors: 0 };
        disk.command(ATA_IDENTIFY, 0, 1, false).ok()?; // (on failure, drop frees it all)
        let id = unsafe { core::slice::from_raw_parts(memory::phys_to_virt(bounce) as *const u16, 256) };
        let lba48 = id[83] & (1 << 10) != 0;
        disk.sectors = if lba48 {
            (0..4).fold(0u64, |acc, i| acc | u64::from(id[100 + i]) << (16 * i))
        } else {
            u64::from(id[60]) | u64::from(id[61]) << 16
        };
        Some(disk)
    }

    fn reg(&self, off: usize) -> u32 {
        rd(self.port + off)
    }

    /// Run one ATA command on `count` sectors at `lba`, with the data in the
    /// bounce buffer (`write`: from it; else into it). Waits until it's done.
    fn command(&mut self, cmd: u8, lba: u64, count: usize, write: bool) -> Result<(), &'static str> {
        let mut spins = 0;
        while self.reg(P_TFD) & (TFD_BSY | TFD_DRQ) != 0 {
            spins += 1;
            if spins > POLL_LIMIT {
                return Err("AHCI: the disk stays busy");
            }
            core::hint::spin_loop();
        }
        let bytes = count * SECTOR_SIZE;
        let base = memory::phys_to_virt(self.page) as *mut u8;
        let ct = self.page + CT_OFF;
        unsafe {
            // Command header 0: a 5-dword FIS, write or read, one PRD entry.
            let header = base.add(CL_OFF as usize) as *mut u32;
            let prds = u32::from(bytes > 0);
            let flags = 5 | if write { 1 << 6 } else { 0 } | (prds << 16);
            write_volatile(header, flags);
            write_volatile(header.add(1), 0); // bytes transferred (filled in)
            write_volatile(header.add(2), ct as u32);
            write_volatile(header.add(3), (ct >> 32) as u32);
            // The command FIS (register, host to device).
            let fis = base.add(CT_OFF as usize);
            core::ptr::write_bytes(fis, 0, 0x80 + 16);
            let f = core::slice::from_raw_parts_mut(fis, 20);
            f[0] = 0x27; // FIS type: register H2D
            f[1] = 0x80; // this is a command
            f[2] = cmd;
            f[4] = lba as u8;
            f[5] = (lba >> 8) as u8;
            f[6] = (lba >> 16) as u8;
            f[7] = if cmd == ATA_IDENTIFY { 0 } else { 0x40 }; // LBA mode
            f[8] = (lba >> 24) as u8;
            f[9] = (lba >> 32) as u8;
            f[10] = (lba >> 40) as u8;
            f[12] = count as u8;
            f[13] = (count >> 8) as u8;
            // PRD entry 0: the bounce buffer.
            let prd = fis.add(0x80) as *mut u32;
            write_volatile(prd, self.bounce as u32);
            write_volatile(prd.add(1), (self.bounce >> 32) as u32);
            write_volatile(prd.add(3), (bytes.max(2) - 1) as u32); // (unused without data)
        }
        wr(self.port + P_IS, u32::MAX);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        wr(self.port + P_CI, 1);
        let mut spins = 0u32;
        loop {
            if self.reg(P_IS) & IS_TFES != 0 || self.reg(P_TFD) & TFD_ERR != 0 {
                self.recover();
                return Err("AHCI: the disk reported an error");
            }
            if self.reg(P_CI) & 1 == 0 {
                break;
            }
            spins += 1;
            if spins % 4096 == 0 {
                crate::interrupts::let_in(); // (keys typed meanwhile aren't lost)
            }
            if spins > POLL_LIMIT {
                self.recover();
                return Err("AHCI: command timed out");
            }
            core::hint::spin_loop();
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    /// After an error: restart the port so the next command can run.
    fn recover(&mut self) {
        wr(self.port + P_CMD, rd(self.port + P_CMD) & !CMD_ST);
        let mut spins = 0;
        while rd(self.port + P_CMD) & CMD_CR != 0 && spins < POLL_LIMIT {
            spins += 1;
            core::hint::spin_loop();
        }
        wr(self.port + P_SERR, u32::MAX);
        wr(self.port + P_IS, u32::MAX);
        wr(self.port + P_CMD, rd(self.port + P_CMD) | CMD_ST);
    }

    fn bounce_buf(&self) -> *mut u8 {
        memory::phys_to_virt(self.bounce) as *mut u8
    }
}

/// Stop a port's command processing and FIS receiving. False if it won't.
fn stop(port: usize) -> bool {
    wr(port + P_CMD, rd(port + P_CMD) & !(CMD_ST | CMD_FRE));
    let mut spins = 0;
    while rd(port + P_CMD) & (CMD_CR | CMD_FR) != 0 {
        spins += 1;
        if spins > POLL_LIMIT {
            return false;
        }
        core::hint::spin_loop();
    }
    true
}

/// Dropping a disk stops its port, frees its memory and hands the port
/// back (a disk that didn't mount can be tried again).
impl Drop for AhciDisk {
    fn drop(&mut self) {
        let stopped = stop(self.port);
        if stopped {
            use x86_64::structures::paging::PhysFrame;
            use x86_64::PhysAddr;
            if let Some(frames) = memory::FRAME_ALLOCATOR.lock().as_mut() {
                for i in 0..=BOUNCE_PAGES as u64 {
                    let phys = if i == 0 { self.page } else { self.bounce + (i - 1) * 4096 };
                    frames.release(PhysFrame::containing_address(PhysAddr::new(phys)));
                }
            }
        } // (else the controller may still write there: leak the memory)
        if let Some(ctl) = CONTROLLER.lock().as_mut() {
            ctl.claimed &= !(1 << self.number);
        }
    }
}

impl BlockDevice for AhciDisk {
    fn sectors(&self) -> u64 {
        self.sectors
    }

    fn read(&mut self, sector: u64, buf: &mut [u8]) -> Result<(), &'static str> {
        assert!(buf.len() % SECTOR_SIZE == 0, "partial sector read");
        let mut lba = sector;
        for chunk in buf.chunks_mut(MAX_SECTORS * SECTOR_SIZE) {
            let count = chunk.len() / SECTOR_SIZE;
            if lba + count as u64 > self.sectors {
                return Err("read past the end of the disk");
            }
            self.command(ATA_READ_DMA_EXT, lba, count, false)?;
            unsafe { core::ptr::copy_nonoverlapping(self.bounce_buf(), chunk.as_mut_ptr(), chunk.len()) };
            lba += count as u64;
        }
        Ok(())
    }

    fn write(&mut self, sector: u64, data: &[u8]) -> Result<(), &'static str> {
        assert!(data.len() % SECTOR_SIZE == 0, "partial sector write");
        let mut lba = sector;
        for chunk in data.chunks(MAX_SECTORS * SECTOR_SIZE) {
            let count = chunk.len() / SECTOR_SIZE;
            if lba + count as u64 > self.sectors {
                return Err("write past the end of the disk");
            }
            unsafe { core::ptr::copy_nonoverlapping(chunk.as_ptr(), self.bounce_buf(), chunk.len()) };
            self.command(ATA_WRITE_DMA_EXT, lba, count, true)?;
            lba += count as u64;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), &'static str> {
        self.command(ATA_FLUSH_CACHE_EXT, 0, 0, false)
    }
}

/// A boxed AHCI disk for port `port`.
pub fn boxed(port: u8) -> Option<Box<dyn BlockDevice>> {
    disk(port).map(|d| Box::new(d) as Box<dyn BlockDevice>)
}

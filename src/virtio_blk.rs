// Copyright (c) 2026 tre4surehunter9
//! virtio-blk (QEMU's -drive if=virtio, or -device virtio-blk-pci): a disk
//! made for virtual machines, much less work for QEMU than emulating IDE or
//! AHCI (SATA).
//!
//! One queue; each request is a chain of three buffers: a header (read,
//! write or flush, and the first 512-byte sector), the data, and a status
//! byte the device writes (0: done). Data goes through a 64 KiB buffer the
//! device can reach (contiguous memory). The disk's size is in its
//! configuration (capacity, in sectors).
//!
//! Disks are numbered after IDE and AHCI ones: block::disk(N) takes the
//! (N-1)th virtio disk when there's no IDE or AHCI disk N (disk 0 is the
//! one the machine boots from). Linux calls them /dev/vda, vdb...

use alloc::boxed::Box;
use alloc::vec::Vec;
use spin::Mutex;

use crate::block::{BlockDevice, SECTOR_SIZE};
use crate::net::pci::PciDevice;
use crate::virtio::{self, Device, Queue};

const T_IN: u32 = 0; // read
const T_OUT: u32 = 1; // write
const T_FLUSH: u32 = 4;
/// VIRTIO_BLK_F_FLUSH: the device has a write cache to flush.
const F_FLUSH: u64 = 1 << 9;
const BOUNCE: usize = 64 * 1024;

pub struct VirtioBlk {
    _dev: Device,
    queue: Queue,
    sectors: u64,
    flush: bool,
    bounce: (u64, *mut u8),
}

unsafe impl Send for VirtioBlk {}

/// virtio disks already handed out (by PCI position: each once).
static TAKEN: Mutex<Vec<(u8, u8, u8)>> = Mutex::new(Vec::new());

impl VirtioBlk {
    fn new(pci: PciDevice) -> Option<VirtioBlk> {
        let (dev, features) = Device::new(pci, F_FLUSH)?;
        dev.no_interrupts(); // (polled: each request is waited for)
        let queue = dev.queue(0, 16)?;
        let sectors = dev.config32(0) as u64 | (dev.config32(4) as u64) << 32;
        let first = crate::memory::FRAME_ALLOCATOR.lock().as_mut()?.allocate_contiguous(BOUNCE / 4096)?;
        let phys = first.start_address().as_u64();
        let bounce = (phys, crate::memory::phys_to_virt(phys) as *mut u8);
        dev.start();
        Some(VirtioBlk { _dev: dev, queue, sectors, flush: features & F_FLUSH != 0, bounce })
    }

    /// One request: `kind` at `sector`, with `len` bytes of the bounce
    /// buffer as its data (none for a flush).
    fn request(&mut self, kind: u32, sector: u64, len: usize) -> Result<(), &'static str> {
        let (sphys, scratch) = self.queue.scratch();
        unsafe {
            // struct virtio_blk_req's header: type, reserved, sector.
            core::ptr::write_volatile(scratch as *mut u32, kind);
            core::ptr::write_volatile(scratch.add(4) as *mut u32, 0);
            core::ptr::write_volatile(scratch.add(8) as *mut u64, sector);
            core::ptr::write_volatile(scratch.add(512), 0xff); // the status, until the device writes it
        }
        let header = (sphys, 16, false);
        let status = (sphys + 512, 1, true);
        let done = if len == 0 {
            self.queue.request_chain(&[header, status])
        } else {
            self.queue.request_chain(&[header, (self.bounce.0, len as u32, kind == T_IN), status])
        };
        let st = unsafe { core::ptr::read_volatile(scratch.add(512)) };
        if !done {
            return Err("virtio disk: no answer");
        }
        if st != 0 { Err("virtio disk: I/O error") } else { Ok(()) }
    }
}

impl BlockDevice for VirtioBlk {
    fn sectors(&self) -> u64 {
        self.sectors
    }

    fn read(&mut self, sector: u64, buf: &mut [u8]) -> Result<(), &'static str> {
        assert!(buf.len() % SECTOR_SIZE == 0, "partial sector read");
        let mut done = 0usize;
        for chunk in buf.chunks_mut(BOUNCE) {
            let at = sector + (done / SECTOR_SIZE) as u64;
            if at + (chunk.len() / SECTOR_SIZE) as u64 > self.sectors {
                return Err("sector out of range");
            }
            self.request(T_IN, at, chunk.len())?;
            unsafe { core::ptr::copy_nonoverlapping(self.bounce.1, chunk.as_mut_ptr(), chunk.len()) };
            done += chunk.len();
        }
        Ok(())
    }

    fn write(&mut self, sector: u64, data: &[u8]) -> Result<(), &'static str> {
        assert!(data.len() % SECTOR_SIZE == 0, "partial sector write");
        let mut done = 0usize;
        for chunk in data.chunks(BOUNCE) {
            let at = sector + (done / SECTOR_SIZE) as u64;
            if at + (chunk.len() / SECTOR_SIZE) as u64 > self.sectors {
                return Err("sector out of range");
            }
            unsafe { core::ptr::copy_nonoverlapping(chunk.as_ptr(), self.bounce.1, chunk.len()) };
            self.request(T_OUT, at, chunk.len())?;
            done += chunk.len();
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), &'static str> {
        if self.flush { self.request(T_FLUSH, 0, 0) } else { Ok(()) }
    }
}

/// The `n`th virtio disk (in PCI order), set up, if there is one and it
/// wasn't handed out yet.
pub fn disk(n: usize) -> Option<Box<dyn BlockDevice>> {
    let pci = *virtio::find(2).get(n)?;
    let key = (pci.bus, pci.slot, pci.func);
    let mut taken = TAKEN.lock();
    if taken.contains(&key) {
        return None;
    }
    let d = VirtioBlk::new(pci)?;
    taken.push(key);
    Some(Box::new(d))
}

/// How many virtio disks there are.
pub fn count() -> usize {
    virtio::find(2).len()
}

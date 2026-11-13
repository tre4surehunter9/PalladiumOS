// Copyright (c) 2026 tre4surehunter9
//! virtio 1.x devices on PCI ("modern" virtio: QEMU's virtio-gpu, -net,
//! -blk...): the transport that every driver of one shares.
//!
//! A virtio device lists its register regions in vendor-specific PCI
//! capabilities (common configuration, notification, interrupt status,
//! device configuration), each a range in one of its memory BARs. The
//! driver resets it, says it knows it (ACKNOWLEDGE, DRIVER), agrees on
//! features (VIRTIO_F_VERSION_1 at least, FEATURES_OK), sets up its queues
//! and starts it (DRIVER_OK).
//!
//! Requests go through split virtqueues in memory: a table of descriptors
//! (buffer address, length, flags, next), a ring where the driver lists
//! the chains it made available, and one where the device lists those it
//! used. Two ways to use one:
//! - `Queue::request`: a chain of two descriptors, the command (read by the
//!   device) and room for its answer (written by it), then waiting for the
//!   answer: for small commands the device (QEMU) answers at once (virtio-
//!   gpu's control).
//! - `submit`, `kick`, `take_used`: buffers handed over one descriptor each
//!   and taken back as the device uses them, for streams (virtio-net's
//!   received and sent frames).

use alloc::vec::Vec;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{fence, Ordering};

use crate::memory;
use crate::net::pci::{self, PciDevice};
use x86_64::structures::paging::FrameAllocator;

pub const VENDOR: u16 = 0x1af4;

// Device status bits.
const ACKNOWLEDGE: u8 = 1;
const DRIVER: u8 = 2;
const DRIVER_OK: u8 = 4;
const FEATURES_OK: u8 = 8;
const FAILED: u8 = 0x80;
/// VIRTIO_F_VERSION_1: a modern (virtio 1.x) device.
pub const F_VERSION_1: u64 = 1 << 32;

// The common configuration's registers (offsets).
const DEVICE_FEATURE_SELECT: usize = 0;
const DEVICE_FEATURE: usize = 4;
const DRIVER_FEATURE_SELECT: usize = 8;
const DRIVER_FEATURE: usize = 12;
const DEVICE_STATUS: usize = 20;
const QUEUE_SELECT: usize = 22;
const QUEUE_SIZE: usize = 24;
const QUEUE_ENABLE: usize = 28;
const QUEUE_NOTIFY_OFF: usize = 30;
const QUEUE_DESC: usize = 32;
const QUEUE_DRIVER: usize = 40;
const QUEUE_DEVICE: usize = 48;

// Descriptor flags.
const DESC_NEXT: u16 = 1;
const DESC_WRITE: u16 = 2;

/// A page of memory the device can use: (physical, kernel address), zeroed.
pub fn dma_page() -> Option<(u64, *mut u8)> {
    let frame = memory::FRAME_ALLOCATOR.lock().as_mut()?.allocate_frame()?;
    memory::zero_frame(frame);
    let phys = frame.start_address().as_u64();
    Some((phys, memory::phys_to_virt(phys) as *mut u8))
}

/// A virtio device's registers.
pub struct Device {
    pub pci: PciDevice,
    common: *mut u8,
    notify: *mut u8,
    notify_mul: u32,
    /// The interrupt status register (reading it acknowledges the
    /// interrupt: PCI's are level-triggered).
    pub isr: *mut u8,
    /// Its own configuration (virtio-gpu's events, a disk's size...).
    pub config: *mut u8,
}

// (Only used with the kernel lock held, by its driver.)
unsafe impl Send for Device {}

fn cfg8(d: &PciDevice, off: u8) -> u8 {
    (pci::read32(d.bus, d.slot, d.func, off & !3) >> ((off & 3) * 8)) as u8
}

fn cfg32(d: &PciDevice, off: u8) -> u32 {
    pci::read32(d.bus, d.slot, d.func, off)
}

/// A memory BAR's physical address (64-bit ones span two BARs).
fn bar_address(d: &PciDevice, n: u8) -> u64 {
    let low = d.bar(n);
    if low & 1 != 0 {
        return 0; // an I/O BAR: not where virtio 1.x keeps its registers
    }
    let high = if (low >> 1) & 3 == 2 { d.bar(n + 1) as u64 } else { 0 };
    (high << 32) | (low & !0xf) as u64
}

impl Device {
    /// Set up the virtio PCI device `pci` up to its queues: its regions
    /// found and mapped, reset, features agreed (`wanted`, of which
    /// VIRTIO_F_VERSION_1 is needed). Returns None if it's not a modern
    /// virtio device or refuses.
    pub fn new(pci: PciDevice, wanted: u64) -> Option<(Device, u64)> {
        pci.enable();
        // The capability list.
        if cfg32(&pci, 0x04) >> 16 & 0x10 == 0 {
            return None;
        }
        let (mut common, mut notify, mut notify_mul, mut config, mut isr) = (None, None, 0, None, None);
        let mut at = cfg8(&pci, 0x34) & !3;
        let mut guard = 0;
        while at != 0 && guard < 48 {
            guard += 1;
            if cfg8(&pci, at) == 0x09 {
                // struct virtio_pci_cap: cfg_type, bar, offset, length.
                let kind = cfg8(&pci, at + 3);
                let bar = cfg8(&pci, at + 4);
                let offset = cfg32(&pci, at + 8) as u64;
                let length = cfg32(&pci, at + 12) as usize;
                let base = bar_address(&pci, bar);
                if base != 0 && length != 0 {
                    let regs = memory::map_mmio(base + offset, length);
                    match kind {
                        1 => common = Some(regs),
                        2 => {
                            notify = Some(regs);
                            notify_mul = cfg32(&pci, at + 16);
                        }
                        3 => isr = Some(regs),
                        4 => config = Some(regs),
                        _ => {}
                    }
                }
            }
            at = cfg8(&pci, at + 1) & !3;
        }
        let dev = Device {
            pci,
            common: common?,
            notify: notify?,
            notify_mul,
            isr: isr.unwrap_or(core::ptr::null_mut()),
            config: config.unwrap_or(core::ptr::null_mut()),
        };
        // Reset, then: we see you, we can drive you.
        dev.w8(DEVICE_STATUS, 0);
        let mut tries = 0;
        while dev.r8(DEVICE_STATUS) != 0 && tries < 1_000_000 {
            tries += 1;
            core::hint::spin_loop();
        }
        dev.w8(DEVICE_STATUS, ACKNOWLEDGE);
        dev.w8(DEVICE_STATUS, ACKNOWLEDGE | DRIVER);
        // Features: what it offers, of what we want.
        let mut offered = 0u64;
        for half in 0..2u32 {
            dev.w32(DEVICE_FEATURE_SELECT, half);
            offered |= (dev.r32(DEVICE_FEATURE) as u64) << (32 * half);
        }
        let agreed = offered & (wanted | F_VERSION_1);
        if agreed & F_VERSION_1 == 0 {
            dev.w8(DEVICE_STATUS, FAILED);
            return None;
        }
        for half in 0..2u32 {
            dev.w32(DRIVER_FEATURE_SELECT, half);
            dev.w32(DRIVER_FEATURE, (agreed >> (32 * half)) as u32);
        }
        dev.w8(DEVICE_STATUS, ACKNOWLEDGE | DRIVER | FEATURES_OK);
        if dev.r8(DEVICE_STATUS) & FEATURES_OK == 0 {
            dev.w8(DEVICE_STATUS, FAILED);
            return None;
        }
        Some((dev, agreed))
    }

    /// No interrupts from this device (its driver polls): the PCI
    /// interrupt disabled. Else, a level-triggered line nobody acknowledges
    /// stays raised, and on a line shared with another device (virtio-net's)
    /// the CPU never gets out of the interrupt.
    pub fn no_interrupts(&self) {
        let cmd = pci::read32(self.pci.bus, self.pci.slot, self.pci.func, 0x04);
        pci::write32(self.pci.bus, self.pci.slot, self.pci.func, 0x04, cmd | 1 << 10);
    }

    /// Everything's set up: the device may start.
    pub fn start(&self) {
        let s = self.r8(DEVICE_STATUS);
        self.w8(DEVICE_STATUS, s | DRIVER_OK);
    }

    /// Set up queue `index` with up to `size` entries.
    pub fn queue(&self, index: u16, size: u16) -> Option<Queue> {
        self.w16(QUEUE_SELECT, index);
        let max = self.r16(QUEUE_SIZE);
        if max == 0 {
            return None;
        }
        let size = size.min(max).min(128);
        // Descriptors and the driver's ring in one page, the device's ring
        // in another (16 * 128 + 4 + 2 * 128 + 2 fits in 4096).
        let (desc_phys, desc) = dma_page()?;
        let (used_phys, used) = dma_page()?;
        let avail_phys = desc_phys + 16 * size as u64;
        self.w16(QUEUE_SIZE, size);
        self.w64(QUEUE_DESC, desc_phys);
        self.w64(QUEUE_DRIVER, avail_phys);
        self.w64(QUEUE_DEVICE, used_phys);
        let notify_off = self.r16(QUEUE_NOTIFY_OFF) as usize;
        self.w16(QUEUE_ENABLE, 1);
        let (req_phys, req) = dma_page()?;
        let (resp_phys, resp) = dma_page()?;
        Some(Queue {
            index,
            size,
            desc,
            avail: unsafe { desc.add(16 * size as usize) },
            used,
            doorbell: unsafe { self.notify.add(notify_off * self.notify_mul as usize) },
            next_avail: 0,
            last_used: 0,
            req: (req_phys, req),
            resp: (resp_phys, resp),
        })
    }

    fn r8(&self, off: usize) -> u8 {
        unsafe { read_volatile(self.common.add(off)) }
    }
    fn w8(&self, off: usize, v: u8) {
        unsafe { write_volatile(self.common.add(off), v) }
    }
    fn r16(&self, off: usize) -> u16 {
        unsafe { read_volatile(self.common.add(off) as *const u16) }
    }
    fn w16(&self, off: usize, v: u16) {
        unsafe { write_volatile(self.common.add(off) as *mut u16, v) }
    }
    fn r32(&self, off: usize) -> u32 {
        unsafe { read_volatile(self.common.add(off) as *const u32) }
    }
    fn w32(&self, off: usize, v: u32) {
        unsafe { write_volatile(self.common.add(off) as *mut u32, v) }
    }
    fn w64(&self, off: usize, v: u64) {
        self.w32(off, v as u32);
        self.w32(off + 4, (v >> 32) as u32);
    }

    /// A byte of the device's own configuration.
    pub fn config8(&self, off: usize) -> u8 {
        if self.config.is_null() {
            return 0;
        }
        unsafe { read_volatile(self.config.add(off)) }
    }

    /// A 32-bit field of the device's own configuration.
    pub fn config32(&self, off: usize) -> u32 {
        if self.config.is_null() {
            return 0;
        }
        unsafe { read_volatile(self.config.add(off) as *const u32) }
    }

    pub fn set_config32(&self, off: usize, v: u32) {
        if !self.config.is_null() {
            unsafe { write_volatile(self.config.add(off) as *mut u32, v) }
        }
    }
}

/// A split virtqueue, used one request at a time.
pub struct Queue {
    pub index: u16,
    size: u16,
    desc: *mut u8,
    avail: *mut u8,
    used: *mut u8,
    doorbell: *mut u8,
    next_avail: u16,
    last_used: u16,
    /// A page for requests and one for answers.
    req: (u64, *mut u8),
    resp: (u64, *mut u8),
}

unsafe impl Send for Queue {}

impl Queue {
    fn set_desc(&self, i: u16, addr: u64, len: u32, flags: u16, next: u16) {
        unsafe {
            let d = self.desc.add(16 * i as usize);
            write_volatile(d as *mut u64, addr);
            write_volatile(d.add(8) as *mut u32, len);
            write_volatile(d.add(12) as *mut u16, flags);
            write_volatile(d.add(14) as *mut u16, next);
        }
    }

    /// The number of descriptors (and buffers) the queue has.
    pub fn size(&self) -> u16 {
        self.size
    }

    /// Hand buffer `phys` (`len` bytes) to the device as descriptor `desc`
    /// (written by the device if `writable`: received data; else read by
    /// it). It comes back through take_used with that number.
    pub fn submit(&mut self, desc: u16, phys: u64, len: u32, writable: bool) {
        self.set_desc(desc, phys, len, if writable { DESC_WRITE } else { 0 }, 0);
        unsafe {
            let slot = self.next_avail % self.size;
            write_volatile(self.avail.add(4 + 2 * slot as usize) as *mut u16, desc);
            fence(Ordering::SeqCst);
            self.next_avail = self.next_avail.wrapping_add(1);
            write_volatile(self.avail.add(2) as *mut u16, self.next_avail);
        }
    }

    /// Tell the device there's something new in the queue.
    pub fn kick(&self) {
        fence(Ordering::SeqCst);
        unsafe { write_volatile(self.doorbell as *mut u16, self.index) };
    }

    /// A buffer the device has finished with: (descriptor, bytes it wrote).
    pub fn take_used(&mut self) -> Option<(u16, u32)> {
        let idx = unsafe { read_volatile(self.used.add(2) as *const u16) };
        if idx == self.last_used {
            return None;
        }
        fence(Ordering::SeqCst);
        let slot = (self.last_used % self.size) as usize;
        let (id, len) = unsafe {
            let e = self.used.add(4 + 8 * slot);
            (read_volatile(e as *const u32), read_volatile(e.add(4) as *const u32))
        };
        self.last_used = self.last_used.wrapping_add(1);
        Some((id as u16, len))
    }

    /// The queue's own request page: (physical, kernel address), for small
    /// structures a driver sends with request_chain.
    pub fn scratch(&self) -> (u64, *mut u8) {
        self.req
    }

    /// Hand the device a chain of buffers (physical address, length,
    /// written by the device?) and wait until it has used it. False if it
    /// didn't in time.
    pub fn request_chain(&mut self, parts: &[(u64, u32, bool)]) -> bool {
        let n = parts.len().min(self.size as usize);
        for (i, &(phys, len, writable)) in parts.iter().take(n).enumerate() {
            let last = i + 1 == n;
            let flags = if writable { DESC_WRITE } else { 0 } | if last { 0 } else { DESC_NEXT };
            self.set_desc(i as u16, phys, len, flags, if last { 0 } else { i as u16 + 1 });
        }
        unsafe {
            let slot = self.next_avail % self.size;
            write_volatile(self.avail.add(4 + 2 * slot as usize) as *mut u16, 0);
            fence(Ordering::SeqCst);
            self.next_avail = self.next_avail.wrapping_add(1);
            write_volatile(self.avail.add(2) as *mut u16, self.next_avail);
        }
        self.kick();
        let mut spins = 0u64;
        loop {
            let idx = unsafe { read_volatile(self.used.add(2) as *const u16) };
            if idx != self.last_used {
                self.last_used = idx;
                fence(Ordering::SeqCst);
                return true;
            }
            spins += 1;
            if spins > 2_000_000_000 {
                return false;
            }
            core::hint::spin_loop();
        }
    }

    /// Send `cmd` (at most a page) and wait for an answer of up to
    /// `resp_len` bytes; returns the answer (empty if none came back in
    /// time).
    pub fn request(&mut self, cmd: &[u8], resp_len: usize) -> Vec<u8> {
        let resp_len = resp_len.min(4096);
        unsafe {
            core::ptr::copy_nonoverlapping(cmd.as_ptr(), self.req.1, cmd.len().min(4096));
            core::ptr::write_bytes(self.resp.1, 0, resp_len);
        }
        self.set_desc(0, self.req.0, cmd.len().min(4096) as u32, DESC_NEXT, 1);
        self.set_desc(1, self.resp.0, resp_len as u32, DESC_WRITE, 0);
        unsafe {
            // The driver's ring: flags (0), idx, ring[size].
            let slot = self.next_avail % self.size;
            write_volatile(self.avail.add(4 + 2 * slot as usize) as *mut u16, 0);
            fence(Ordering::SeqCst);
            self.next_avail = self.next_avail.wrapping_add(1);
            write_volatile(self.avail.add(2) as *mut u16, self.next_avail);
            fence(Ordering::SeqCst);
            write_volatile(self.doorbell as *mut u16, self.index);
        }
        // The device's ring: flags, idx, ring[size] of (id, len).
        let mut spins = 0u64;
        loop {
            let idx = unsafe { read_volatile(self.used.add(2) as *const u16) };
            if idx != self.last_used {
                self.last_used = idx;
                break;
            }
            spins += 1;
            if spins > 200_000_000 {
                return Vec::new();
            }
            core::hint::spin_loop();
        }
        fence(Ordering::SeqCst);
        let mut out = alloc::vec![0u8; resp_len];
        unsafe { core::ptr::copy_nonoverlapping(self.resp.1, out.as_mut_ptr(), resp_len) };
        out
    }
}

/// The virtio PCI functions for device type `kind` (16 GPU, 1 network, 2
/// block): modern-only devices are numbered 0x1040 + type; "transitional"
/// ones, which also speak the old (legacy) interface and are what QEMU's
/// virtio-net-pci and virtio-blk-pci are by default, 0x1000..0x103f with
/// the type as their PCI subsystem ID. Both have the modern registers.
pub fn find(kind: u16) -> Vec<PciDevice> {
    pci::scan()
        .into_iter()
        .filter(|d| {
            d.vendor == VENDOR
                && (d.device == 0x1040 + kind
                    || ((0x1000..0x1040).contains(&d.device) && (cfg32(d, 0x2C) >> 16) as u16 == kind))
        })
        .collect()
}

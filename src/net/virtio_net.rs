// Copyright (c) 2026 tre4surehunter9
//! virtio-net (QEMU's -device virtio-net-pci): a network card made for
//! virtual machines, much less work for QEMU than an emulated RTL8139.
//!
//! Two queues: received frames (0) and frames to send (1). Every frame
//! starts with a 12-byte virtio-net header (checksum and segmentation
//! offloads: none used here, so zeros). For receiving, RX_BUFFERS buffers
//! are always with the device; each one comes back with a frame and goes
//! back once copied. For sending, TX_BUFFERS buffers: one is filled and
//! handed over per frame, and taken back when the device says it's sent.
//! The device interrupts (its PCI line, as the RTL8139 does) when frames
//! arrive; the handler acknowledges it by reading the ISR register.

use crate::virtio::{self, Device, Queue};

pub const MTU: usize = 1514;
/// struct virtio_net_hdr_v1 (flags, GSO type, header length, GSO size,
/// checksum start and offset, buffers).
const HEADER: usize = 12;
/// Each buffer: half a page (the header and a full frame fit).
const BUFFER: usize = 2048;
const RX_BUFFERS: u16 = 32;
const TX_BUFFERS: u16 = 16;
/// VIRTIO_NET_F_MAC: the device's MAC address is in its configuration.
const F_MAC: u64 = 1 << 5;

pub struct VirtioNet {
    _dev: Device,
    rx: Queue,
    tx: Queue,
    /// Each buffer: (physical, kernel address).
    rx_bufs: alloc::vec::Vec<(u64, *mut u8)>,
    tx_bufs: alloc::vec::Vec<(u64, *mut u8)>,
    /// Send buffers not with the device.
    tx_free: alloc::vec::Vec<u16>,
    pub mac: [u8; 6],
    pub irq: u8,
    /// The ISR register's address, to acknowledge interrupts.
    pub isr: u64,
}

unsafe impl Send for VirtioNet {}

/// Buffers, two to a page.
fn buffers(n: u16) -> Option<alloc::vec::Vec<(u64, *mut u8)>> {
    let mut v = alloc::vec::Vec::new();
    while v.len() < n as usize {
        let (phys, virt) = virtio::dma_page()?;
        v.push((phys, virt));
        v.push((phys + BUFFER as u64, unsafe { virt.add(BUFFER) }));
    }
    v.truncate(n as usize);
    Some(v)
}

impl VirtioNet {
    /// The `n`th virtio network card (in PCI order), set up.
    pub fn find(n: usize) -> Option<VirtioNet> {
        let pci = virtio::find(1).into_iter().nth(n)?;
        let irq = pci.irq;
        let (dev, features) = Device::new(pci, F_MAC)?;
        let mut rx = dev.queue(0, RX_BUFFERS)?;
        let mut tx = dev.queue(1, TX_BUFFERS)?;
        let mut mac = [0x52, 0x54, 0, 0x12, 0x34, 0x56];
        if features & F_MAC != 0 {
            for (i, b) in mac.iter_mut().enumerate() {
                *b = dev.config8(i);
            }
        }
        let rx_bufs = buffers(rx.size().min(RX_BUFFERS))?;
        let tx_bufs = buffers(tx.size().min(TX_BUFFERS))?;
        dev.start();
        // Every receive buffer to the device.
        for (i, &(phys, _)) in rx_bufs.iter().enumerate() {
            rx.submit(i as u16, phys, BUFFER as u32, true);
        }
        rx.kick();
        let _ = &mut tx;
        let tx_free = (0..tx_bufs.len() as u16).collect();
        let isr = dev.isr as u64;
        Some(VirtioNet { _dev: dev, rx, tx, rx_bufs, tx_bufs, tx_free, mac, irq, isr })
    }

    /// The next received frame, copied into `out` (its length).
    pub fn receive_frame(&mut self, out: &mut [u8]) -> Option<usize> {
        let (id, len) = self.rx.take_used()?;
        let (phys, virt) = *self.rx_bufs.get(id as usize)?;
        let n = (len as usize).saturating_sub(HEADER).min(out.len()).min(BUFFER - HEADER);
        unsafe { core::ptr::copy_nonoverlapping(virt.add(HEADER), out.as_mut_ptr(), n) };
        // The buffer goes back for the next frame.
        self.rx.submit(id, phys, BUFFER as u32, true);
        self.rx.kick();
        Some(n)
    }

    /// Send a frame (a buffer for it: one the device has finished with).
    pub fn send_frame(&mut self, frame: &[u8]) {
        while let Some((id, _)) = self.tx.take_used() {
            self.tx_free.push(id);
        }
        let mut spins = 0;
        let id = loop {
            if let Some(id) = self.tx_free.pop() {
                break id;
            }
            if let Some((id, _)) = self.tx.take_used() {
                break id;
            }
            spins += 1;
            if spins > 10_000_000 {
                return; // (the device isn't taking frames: dropped)
            }
            core::hint::spin_loop();
        };
        let (phys, virt) = self.tx_bufs[id as usize];
        let n = frame.len().min(BUFFER - HEADER);
        unsafe {
            core::ptr::write_bytes(virt, 0, HEADER);
            core::ptr::copy_nonoverlapping(frame.as_ptr(), virt.add(HEADER), n);
        }
        self.tx.submit(id, phys, (HEADER + n) as u32, false);
        self.tx.kick();
    }
}

/// Acknowledge the card's interrupt (reading the ISR register clears it).
pub fn ack_interrupt(isr: u64) {
    if isr != 0 {
        unsafe { core::ptr::read_volatile(isr as *const u8) };
    }
}

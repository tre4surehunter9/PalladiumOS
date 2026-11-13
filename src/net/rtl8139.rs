// Copyright (c) 2026 tre4surehunter9
//! A driver for the Realtek RTL8139 network card (QEMU's `-device rtl8139`).
//!
//! Receiving: the card writes frames into one ring buffer (8 KiB, plus
//! room for a frame that runs past the end), each preceded by a 4-byte
//! header (status, length). Sending: four transmit slots, each a buffer
//! address and a "start" register written with the length. Both buffers
//! are physical memory the card reaches by DMA.

use x86_64::instructions::port::Port;

use super::pci::PciDevice;
use crate::memory;

const RX_RING: usize = 8192;
const RX_BUF_PAGES: usize = 4; // 8 KiB + 16 + room for a wrapped 1.5 KiB frame
const TX_SLOTS: usize = 4;
const MTU: usize = 1514;

// Registers
const IDR0: u16 = 0x00; // MAC address
const TSD0: u16 = 0x10; // transmit status, per slot
const TSAD0: u16 = 0x20; // transmit buffer address, per slot
const RBSTART: u16 = 0x30;
const CMD: u16 = 0x37;
const CAPR: u16 = 0x38;
const IMR: u16 = 0x3C;
const ISR: u16 = 0x3E;
const RCR: u16 = 0x44;
const CONFIG1: u16 = 0x52;

const CMD_RESET: u8 = 0x10;
const CMD_RX_ENABLE: u8 = 0x08;
const CMD_TX_ENABLE: u8 = 0x04;
const CMD_BUFFER_EMPTY: u8 = 0x01;
const TSD_OWN: u32 = 1 << 13; // the card is done with the slot

pub struct Rtl8139 {
    io: u16,
    pub mac: [u8; 6],
    pub irq: u8,
    rx_phys: u64,
    rx_offset: usize,
    tx_phys: [u64; TX_SLOTS],
    tx_next: usize,
}

fn virt(phys: u64) -> *mut u8 {
    memory::phys_ptr(x86_64::PhysAddr::new(phys))
}

impl Rtl8139 {
    pub const VENDOR: u16 = 0x10EC;
    pub const DEVICE: u16 = 0x8139;

    pub fn new(pci: &PciDevice) -> Option<Rtl8139> {
        let bar = pci.bar(0);
        if bar & 1 == 0 {
            return None; // expected an I/O port BAR
        }
        pci.enable();
        let io = (bar & !3) as u16;
        let mut alloc = memory::FRAME_ALLOCATOR.lock();
        let frames = alloc.as_mut()?;
        let rx = frames.allocate_contiguous(RX_BUF_PAGES)?.start_address().as_u64();
        let mut tx_phys = [0u64; TX_SLOTS];
        for t in tx_phys.iter_mut() {
            *t = frames.allocate_contiguous(1)?.start_address().as_u64();
        }
        drop(alloc);
        unsafe { core::ptr::write_bytes(virt(rx), 0, RX_BUF_PAGES * 4096) };
        let mut nic = Rtl8139 { io, mac: [0; 6], irq: pci.irq, rx_phys: rx, rx_offset: 0, tx_phys, tx_next: 0 };
        unsafe {
            nic.out8(CONFIG1, 0); // power on
            nic.out8(CMD, CMD_RESET);
            let mut spins = 0;
            while nic.in8(CMD) & CMD_RESET != 0 {
                spins += 1;
                if spins > 1_000_000 {
                    return None;
                }
            }
            for i in 0..6 {
                nic.mac[i] = nic.in8(IDR0 + i as u16);
            }
            nic.out32(RBSTART, rx as u32);
            nic.out16(IMR, 0x0005); // interrupt on "received" and "sent"
            // Accept broadcast, multicast and our own address; WRAP: a frame
            // may run past the ring's end (into the extra room).
            nic.out32(RCR, 0x0e | 1 << 7);
            // Every multicast frame through (the hash filter all ones):
            // IPv6's neighbour discovery and router advertisements.
            nic.out32(0x08, 0xffff_ffff);
            nic.out32(0x0c, 0xffff_ffff);
            nic.out8(CMD, CMD_RX_ENABLE | CMD_TX_ENABLE);
        }
        Some(nic)
    }

    unsafe fn in8(&self, r: u16) -> u8 {
        unsafe { Port::<u8>::new(self.io + r).read() }
    }
    unsafe fn out8(&self, r: u16, v: u8) {
        unsafe { Port::<u8>::new(self.io + r).write(v) }
    }
    unsafe fn out16(&self, r: u16, v: u16) {
        unsafe { Port::<u16>::new(self.io + r).write(v) }
    }
    unsafe fn out32(&self, r: u16, v: u32) {
        unsafe { Port::<u32>::new(self.io + r).write(v) }
    }
    unsafe fn in32(&self, r: u16) -> u32 {
        unsafe { Port::<u32>::new(self.io + r).read() }
    }

    /// The card's I/O ports (for acknowledging its interrupt).
    pub fn io_base(&self) -> u16 {
        self.io
    }
}

/// Acknowledge the card's interrupt: clear its status register. Called from
/// the IRQ handler with just the port number (no locks): PCI interrupts are
/// level-triggered, so an unacknowledged one fires again at once, forever.
pub fn ack_interrupt(io: u16) {
    unsafe {
        let mut isr = Port::<u16>::new(io + ISR);
        let status = isr.read();
        isr.write(status);
    }
}

impl Rtl8139 {

    /// The next received frame, if any (copied out of the ring).
    pub(super) fn receive_frame(&mut self, out: &mut [u8; MTU + 4]) -> Option<usize> {
        unsafe {
            if self.in8(CMD) & CMD_BUFFER_EMPTY != 0 {
                return None;
            }
            let base = virt(self.rx_phys);
            let header = base.add(self.rx_offset) as *const u16;
            let status = header.read_volatile();
            let len = header.add(1).read_volatile() as usize; // includes the 4-byte CRC
            let good = status & 1 != 0 && (64..=MTU + 4).contains(&len);
            let copied = if good {
                core::ptr::copy_nonoverlapping(base.add(self.rx_offset + 4), out.as_mut_ptr(), len - 4);
                Some(len - 4)
            } else {
                None
            };
            // Next frame: after this one, 4-byte aligned, wrapping around the ring.
            self.rx_offset = (self.rx_offset + len + 4 + 3) & !3;
            self.rx_offset %= RX_RING;
            self.out16(CAPR, (self.rx_offset as u16).wrapping_sub(16));
            match copied {
                Some(n) => Some(n),
                None => None, // a bad frame was skipped; the next poll gets the rest
            }
        }
    }

    /// Send a frame (waits for a free slot).
    pub(super) fn send_frame(&mut self, frame: &[u8]) {
        let slot = self.tx_next;
        self.tx_next = (slot + 1) % TX_SLOTS;
        unsafe {
            let tsd = TSD0 + 4 * slot as u16;
            // Wait for the card to finish with this slot's previous frame
            // (a fresh slot reads as not owned until first used).
            let mut spins = 0;
            while self.in32(tsd) & TSD_OWN == 0 && self.in32(tsd) != 0 && spins < 1_000_000 {
                spins += 1;
            }
            let len = frame.len().clamp(60, MTU); // at least the Ethernet minimum
            let buf = virt(self.tx_phys[slot]);
            core::ptr::write_bytes(buf, 0, len);
            core::ptr::copy_nonoverlapping(frame.as_ptr(), buf, frame.len().min(MTU));
            self.out32(TSAD0 + 4 * slot as u16, self.tx_phys[slot] as u32);
            self.out32(tsd, len as u32); // OWN = 0: send it
        }
    }
}


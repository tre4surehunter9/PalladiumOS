// Copyright (c) 2026 tre4surehunter9
//! PCI configuration space (the legacy 0xCF8/0xCFC ports): finding devices
//! and turning on their I/O and bus mastering (DMA).

use alloc::vec::Vec;
use x86_64::instructions::port::Port;

#[derive(Clone, Copy, Debug)]
pub struct PciDevice {
    pub bus: u8,
    pub slot: u8,
    pub func: u8,
    pub vendor: u16,
    pub device: u16,
    pub class: u8,
    pub subclass: u8,
    /// Interrupt line (the PIC IRQ the firmware routed it to).
    pub irq: u8,
}

fn address(bus: u8, slot: u8, func: u8, offset: u8) -> u32 {
    0x8000_0000 | (bus as u32) << 16 | (slot as u32) << 11 | (func as u32) << 8 | (offset as u32 & 0xfc)
}

pub fn read32(bus: u8, slot: u8, func: u8, offset: u8) -> u32 {
    unsafe {
        Port::<u32>::new(0xCF8).write(address(bus, slot, func, offset));
        Port::<u32>::new(0xCFC).read()
    }
}

pub fn write32(bus: u8, slot: u8, func: u8, offset: u8, value: u32) {
    unsafe {
        Port::<u32>::new(0xCF8).write(address(bus, slot, func, offset));
        Port::<u32>::new(0xCFC).write(value);
    }
}

impl PciDevice {
    /// Base address register `n` (I/O BARs have bit 0 set).
    pub fn bar(&self, n: u8) -> u32 {
        read32(self.bus, self.slot, self.func, 0x10 + 4 * n)
    }

    /// Let the device answer I/O and memory accesses and do DMA.
    pub fn enable(&self) {
        let cmd = read32(self.bus, self.slot, self.func, 0x04);
        write32(self.bus, self.slot, self.func, 0x04, cmd | 0x1 | 0x2 | 0x4);
    }
}

/// Every PCI function on buses 0-7.
pub fn scan() -> Vec<PciDevice> {
    let mut found = Vec::new();
    for bus in 0..8u8 {
        for slot in 0..32u8 {
            for func in 0..8u8 {
                let id = read32(bus, slot, func, 0);
                if id & 0xffff == 0xffff {
                    if func == 0 {
                        break; // no device in this slot
                    }
                    continue;
                }
                let class = read32(bus, slot, func, 0x08);
                let irq = read32(bus, slot, func, 0x3C) as u8;
                found.push(PciDevice {
                    bus,
                    slot,
                    func,
                    vendor: id as u16,
                    device: (id >> 16) as u16,
                    class: (class >> 24) as u8,
                    subclass: (class >> 16) as u8,
                    irq,
                });
                // Single-function devices answer the same for every function.
                if func == 0 && read32(bus, slot, 0, 0x0C) & 0x0080_0000 == 0 {
                    break;
                }
            }
        }
    }
    found
}

// Copyright (c) 2026 tre4surehunter9
//! USB keyboards, through an xHCI (USB 3) host controller: the USB
//! controller of PCs from about 2012 on (QEMU: -device qemu-xhci).
//!
//! The controller is a PCI device (class 0x0C, subclass 3, interface 0x30)
//! with memory-mapped registers. The kernel talks to it through rings of
//! 16-byte "TRBs" in memory: a command ring (enable a device slot, give a
//! device its address, configure an endpoint), one transfer ring per
//! endpoint, and an event ring where the controller reports what finished.
//!
//! At boot, every port with a device is reset and the device enumerated:
//! read its descriptors, and if it's a keyboard (HID class, boot protocol),
//! configure its interrupt endpoint and keep transfers queued on it. Each
//! completed transfer is an 8-byte report (modifier keys and up to six keys
//! held down); comparing it with the previous one gives key presses and
//! releases, which become PS/2 scancodes in the same queue the PS/2
//! keyboard feeds, so everything above (the TTY, Ctrl-C) works the same.
//! USB keyboards don't repeat keys themselves, so this does it. Events are
//! polled from the timer interrupt (100 Hz), and transfers stay queued in
//! between, so no key is lost.
//!
//! USB pointers feed /dev/input/mice (as PS/2 packets, like the PS/2
//! mouse): a boot-protocol mouse moves the pointer by its own reports; an
//! absolute pointer (a tablet: QEMU's -device usb-tablet, which follows the
//! host's cursor without grabbing it), found from its HID report
//! descriptor, is turned into moves from where the pointer was. X
//! accelerates moves of more than 4 pixels, so those are sent in steps of
//! at most 2 pixels a side (applied exactly), and the pointer is first put
//! in the top left corner (X stops it there; moves of at most 45 pixels,
//! which X's protocol detection accepts), and again whenever the tablet
//! reaches the left or top edge, or a program sets the mouse up (X's
//! driver does, more than once, dropping moves meanwhile), so it stays
//! where the tablet says.

use alloc::vec::Vec;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{fence, Ordering};
use spin::Mutex;

use crate::memory;

const RING_TRBS: usize = 256;
const POLL_LIMIT: u32 = 20_000_000;
/// Reports queued on the keyboard's endpoint at once.
const REPORT_SLOTS: usize = 16;
const REPORT_SIZE: usize = 64;
/// Key repeat, as a PC keyboard: after 500 ms held, 20 times a second.
/// (In timer ticks, from their length: tick-counted values sped up when
/// the timer went from 18.2 to 100 Hz.)
const REPEAT_DELAY_MS: u64 = 500;
const REPEAT_EVERY_MS: u64 = 50;

fn ms_to_ticks(ms: u64) -> u32 {
    (ms * 1_000_000).div_ceil(crate::process::TICK_NS).max(1) as u32
}

// TRB types.
const TRB_NORMAL: u32 = 1;
const TRB_SETUP: u32 = 2;
const TRB_DATA: u32 = 3;
const TRB_STATUS: u32 = 4;
const TRB_LINK: u32 = 6;
const TRB_ENABLE_SLOT: u32 = 9;
const TRB_ADDRESS_DEVICE: u32 = 11;
const TRB_CONFIGURE_EP: u32 = 12;
const TRB_EVALUATE_CONTEXT: u32 = 13;
const TRB_EV_TRANSFER: u32 = 32;
const TRB_EV_COMMAND: u32 = 33;
const CC_SUCCESS: u32 = 1;
const CC_SHORT_PACKET: u32 = 13;

// Operational registers.
const USBCMD: usize = 0x00;
const USBSTS: usize = 0x04;
const CRCR: usize = 0x18;
const DCBAAP: usize = 0x30;
const CONFIG: usize = 0x38;
const CMD_RUN: u32 = 1 << 0;
const CMD_RESET: u32 = 1 << 1;
const STS_HALTED: u32 = 1 << 0;
const STS_NOT_READY: u32 = 1 << 11;

// Port status and control.
const PORT_CCS: u32 = 1 << 0; // a device is connected
const PORT_PED: u32 = 1 << 1; // enabled
const PORT_PR: u32 = 1 << 4; // reset
const PORT_PRC: u32 = 1 << 21; // reset finished
/// Bits written back unchanged (the rest are write-1-to-clear or actions).
const PORT_KEEP: u32 = (1 << 0) | (1 << 3) | (0xF << 5) | (1 << 9) | (0xF << 10) | (3 << 14) | (7 << 25) | (1 << 24) | (1 << 30);

fn rd(addr: usize) -> u32 {
    unsafe { read_volatile(addr as *const u32) }
}

fn wr(addr: usize, v: u32) {
    unsafe { write_volatile(addr as *mut u32, v) }
}

fn wr64(addr: usize, v: u64) {
    wr(addr, v as u32);
    wr(addr + 4, (v >> 32) as u32);
}

/// A zeroed page of memory for the controller: (physical address, pointer).
fn dma_page() -> Option<(u64, *mut u32)> {
    let phys = memory::FRAME_ALLOCATOR.lock().as_mut()?.allocate_contiguous(1)?.start_address().as_u64();
    let virt = memory::phys_to_virt(phys) as *mut u32;
    unsafe { core::ptr::write_bytes(virt as *mut u8, 0, 4096) };
    Some((phys, virt))
}

/// A ring the kernel fills (commands, or transfers for one endpoint). The
/// last TRB links back to the start, and the cycle bit flips each lap so
/// the controller can tell new TRBs from old ones.
struct Ring {
    phys: u64,
    virt: *mut u32,
    index: usize,
    cycle: u32,
}

impl Ring {
    fn new() -> Option<Ring> {
        let (phys, virt) = dma_page()?;
        Some(Ring { phys, virt, index: 0, cycle: 1 })
    }

    /// Add a TRB; returns its physical address.
    fn push(&mut self, param: u64, status: u32, control: u32) -> u64 {
        if self.index == RING_TRBS - 1 {
            self.write(self.index, self.phys, 0, (TRB_LINK << 10) | (1 << 1) | self.cycle);
            self.cycle ^= 1;
            self.index = 0;
        }
        let addr = self.phys + 16 * self.index as u64;
        self.write(self.index, param, status, control | self.cycle);
        self.index += 1;
        addr
    }

    fn write(&mut self, i: usize, param: u64, status: u32, control: u32) {
        unsafe {
            let p = self.virt.add(4 * i);
            write_volatile(p, param as u32);
            write_volatile(p.add(1), (param >> 32) as u32);
            write_volatile(p.add(2), status);
            fence(Ordering::SeqCst);
            write_volatile(p.add(3), control); // (the cycle bit hands it over)
        }
    }
}

/// The event ring the controller fills.
struct EventRing {
    phys: u64,
    virt: *mut u32,
    index: usize,
    cycle: u32,
}

impl EventRing {
    fn next(&mut self) -> Option<[u32; 4]> {
        let p = unsafe { self.virt.add(4 * self.index) };
        let control = unsafe { read_volatile(p.add(3)) };
        if control & 1 != self.cycle {
            return None;
        }
        fence(Ordering::SeqCst);
        let trb = unsafe { [read_volatile(p), read_volatile(p.add(1)), read_volatile(p.add(2)), control] };
        self.index += 1;
        if self.index == RING_TRBS {
            self.index = 0;
            self.cycle ^= 1;
        }
        Some(trb)
    }

    fn dequeue_pointer(&self) -> u64 {
        self.phys + 16 * self.index as u64
    }
}

fn trb_type(trb: &[u32; 4]) -> u32 {
    (trb[3] >> 10) & 0x3F
}

fn trb_param(trb: &[u32; 4]) -> u64 {
    u64::from(trb[0]) | u64::from(trb[1]) << 32
}

fn completion(trb: &[u32; 4]) -> u32 {
    trb[2] >> 24
}

/// A device being set up: its slot, input context and control endpoint.
struct Device {
    slot: u8,
    port: u8,
    input: (u64, *mut u32),
    ep0: Ring,
    /// A page for descriptors.
    buf: (u64, *mut u32),
}

struct Keyboard {
    slot: u8,
    dci: u8,
    ring: Ring,
    /// REPORT_SLOTS buffers of REPORT_SIZE bytes.
    reports: (u64, *mut u8),
    /// Which buffer each ring TRB points at.
    slot_of_trb: [u8; RING_TRBS],
    length: u32,
    prev: [u8; 8],
    /// The key held longest (a HID usage), and for how many ticks.
    held: Option<(u8, u32)>,
    /// Not a keyboard but a pointer.
    pointer: Option<Pointer>,
}

/// A USB pointer: a boot-protocol mouse (relative moves), or an absolute
/// pointer laid out by its HID report descriptor.
struct Pointer {
    layout: Layout,
    /// The screen's size (for absolute pointers).
    screen: (i32, i32),
    /// Where the pointer is now, as far as programs were told (absolute
    /// pointers; None until it's been put in the corner once).
    at: Option<(i32, i32)>,
    buttons: u8,
}

enum Layout {
    Boot,
    Absolute(Absolute),
}

/// A field of an input report: bits `offset..offset + size`, with values
/// from `min` to `max`.
#[derive(Clone, Copy)]
struct Field {
    offset: u32,
    size: u32,
    min: i32,
    max: i32,
}

/// Where an absolute pointer's report keeps its values (report_id 0: the
/// device numbers no reports).
struct Absolute {
    report_id: u8,
    x: Field,
    y: Field,
    buttons: [Option<Field>; 3],
    wheel: Option<Field>,
}

struct Xhci {
    op: usize,
    rt: usize,
    db: usize,
    ports: u8,
    csz: usize,
    dcbaa: *mut u64,
    cmd: Ring,
    events: EventRing,
    keyboards: Vec<Keyboard>,
}

unsafe impl Send for Xhci {}

static XHCI: Mutex<Option<Xhci>> = Mutex::new(None);

/// Find the USB controller, set it up and look for keyboards, mice and
/// tablets. Returns how many devices were set up (None: no xHCI controller).
pub fn init() -> Option<usize> {
    let pci = crate::net::pci::scan().into_iter().find(|d| {
        d.class == 0x0C && d.subclass == 0x03 && (crate::net::pci::read32(d.bus, d.slot, d.func, 0x08) >> 8) & 0xFF == 0x30
    })?;
    let bar0 = pci.bar(0);
    if bar0 & 1 != 0 {
        return None;
    }
    let mut base = u64::from(bar0 & !0xF);
    if (bar0 >> 1) & 3 == 2 {
        base |= u64::from(pci.bar(1)) << 32; // a 64-bit BAR
    }
    pci.enable();
    let cap = memory::map_mmio(base, 0x1000) as usize;
    let caplength = rd(cap) & 0xFF;
    let hcs1 = rd(cap + 0x04);
    let hcs2 = rd(cap + 0x08);
    let hcc1 = rd(cap + 0x10);
    let dboff = (rd(cap + 0x14) & !3) as usize;
    let rtsoff = (rd(cap + 0x18) & !0x1F) as usize;
    let max_slots = (hcs1 & 0xFF) as usize;
    let ports = (hcs1 >> 24) as u8;
    let op_off = caplength as usize;
    let end = (dboff + 4 * (max_slots + 1)).max(rtsoff + 0x40).max(op_off + 0x400 + 0x10 * ports as usize);
    memory::map_mmio(base, end);
    let op = cap + op_off;

    take_from_firmware(cap, hcc1);

    // Stop and reset the controller.
    wr(op + USBCMD, rd(op + USBCMD) & !CMD_RUN);
    wait(|| rd(op + USBSTS) & STS_HALTED != 0)?;
    wr(op + USBCMD, CMD_RESET);
    wait(|| rd(op + USBCMD) & CMD_RESET == 0 && rd(op + USBSTS) & STS_NOT_READY == 0)?;

    let slots = max_slots.min(32);
    wr(op + CONFIG, slots as u32);
    let (dcbaa_phys, dcbaa) = dma_page()?;
    let dcbaa = dcbaa as *mut u64;
    // Scratchpad pages: memory the controller keeps for itself.
    let scratch = ((hcs2 >> 21) & 0x1F) << 5 | ((hcs2 >> 27) & 0x1F);
    if scratch > 0 {
        let (array_phys, array) = dma_page()?;
        for i in 0..scratch.min(512) as usize {
            let (p, _) = dma_page()?;
            unsafe { write_volatile((array as *mut u64).add(i), p) };
        }
        unsafe { write_volatile(dcbaa, array_phys) };
    }
    wr64(op + DCBAAP, dcbaa_phys);

    let cmd = Ring::new()?;
    wr64(op + CRCR, cmd.phys | 1);

    // The event ring, and its one-entry segment table, for interrupter 0.
    let (ev_phys, ev_virt) = dma_page()?;
    let (erst_phys, erst) = dma_page()?;
    unsafe {
        write_volatile(erst, ev_phys as u32);
        write_volatile(erst.add(1), (ev_phys >> 32) as u32);
        write_volatile(erst.add(2), RING_TRBS as u32);
    }
    let rt = cap + rtsoff;
    let ir0 = rt + 0x20;
    wr(ir0 + 0x08, 1); // segment table size
    wr64(ir0 + 0x18, ev_phys); // dequeue pointer
    wr64(ir0 + 0x10, erst_phys); // segment table (last: this starts it)

    wr(op + USBCMD, CMD_RUN);
    wait(|| rd(op + USBSTS) & STS_HALTED == 0)?;

    let mut hc = Xhci {
        op,
        rt,
        db: cap + dboff,
        ports,
        csz: if hcc1 & (1 << 2) != 0 { 64 } else { 32 },
        dcbaa,
        cmd,
        events: EventRing { phys: ev_phys, virt: ev_virt, index: 0, cycle: 1 },
        keyboards: Vec::new(),
    };
    for port in 1..=hc.ports {
        if let Some(kbd) = hc.setup_port(port) {
            hc.keyboards.push(kbd);
        }
    }
    for k in 0..hc.keyboards.len() {
        hc.queue_reports(k);
    }
    let found = hc.keyboards.len();
    *XHCI.lock() = Some(hc);
    Some(found)
}

/// Wait (a bounded time) until `done`.
fn wait(mut done: impl FnMut() -> bool) -> Option<()> {
    for _ in 0..POLL_LIMIT {
        if done() {
            return Some(());
        }
        core::hint::spin_loop();
    }
    None
}

/// The firmware may drive the controller itself (USB keyboards in the
/// BIOS setup): ask for it through the "USB legacy support" capability.
fn take_from_firmware(cap: usize, hcc1: u32) {
    let mut off = ((hcc1 >> 16) << 2) as usize;
    while off != 0 {
        let reg = cap + off;
        let v = rd(reg);
        if v & 0xFF == 1 {
            wr(reg, v | 1 << 24); // OS owned
            let _ = wait(|| rd(reg) & (1 << 16) == 0); // until not BIOS owned
            wr(reg + 4, 0xE000_0000); // no more SMIs (and clear their status)
            return;
        }
        let next = ((v >> 8) & 0xFF) as usize;
        off = if next == 0 { 0 } else { off + (next << 2) };
    }
}

impl Xhci {
    fn port_reg(&self, port: u8) -> usize {
        self.op + 0x400 + 0x10 * (port as usize - 1)
    }

    fn ring_doorbell(&self, slot: u8, target: u32) {
        fence(Ordering::SeqCst);
        wr(self.db + 4 * slot as usize, target);
    }

    fn update_dequeue(&self) {
        wr64(self.rt + 0x20 + 0x18, self.events.dequeue_pointer() | (1 << 3));
    }

    /// Wait for an event that `want` accepts (others are dropped: nothing
    /// else is running while devices are set up).
    fn wait_event(&mut self, want: impl Fn(&[u32; 4]) -> bool) -> Option<[u32; 4]> {
        for _ in 0..POLL_LIMIT {
            if let Some(ev) = self.events.next() {
                self.update_dequeue();
                if want(&ev) {
                    return Some(ev);
                }
            } else {
                core::hint::spin_loop();
            }
        }
        None
    }

    fn command(&mut self, param: u64, control: u32) -> Option<[u32; 4]> {
        let addr = self.cmd.push(param, 0, control);
        self.ring_doorbell(0, 0);
        let ev = self.wait_event(|e| trb_type(e) == TRB_EV_COMMAND && trb_param(e) == addr)?;
        (completion(&ev) == CC_SUCCESS).then_some(ev)
    }

    /// A control transfer on endpoint 0, with `len` bytes of data in (or
    /// none) through the device's buffer. Returns the bytes received.
    fn control(&mut self, dev: &mut Device, req_type: u8, req: u8, value: u16, index: u16, len: u16) -> Option<usize> {
        let setup = u64::from(req_type) | u64::from(req) << 8 | u64::from(value) << 16 | u64::from(index) << 32 | u64::from(len) << 48;
        let input = len > 0;
        let trt = if input { 3 } else { 0 };
        dev.ep0.push(setup, 8, (TRB_SETUP << 10) | (1 << 6) | (trt << 16));
        if input {
            dev.ep0.push(dev.buf.0, u32::from(len), (TRB_DATA << 10) | (1 << 16));
        }
        let dir = if input { 0 } else { 1 << 16 };
        let status = dev.ep0.push(0, 0, (TRB_STATUS << 10) | (1 << 5) | dir);
        self.ring_doorbell(dev.slot, 1);
        let mut remaining = 0;
        let slot = dev.slot;
        loop {
            let ev = self.wait_event(|e| trb_type(e) == TRB_EV_TRANSFER && (e[3] >> 24) as u8 == slot)?;
            let cc = completion(&ev);
            if cc != CC_SUCCESS && cc != CC_SHORT_PACKET {
                return None;
            }
            if cc == CC_SHORT_PACKET {
                remaining = ev[2] & 0xFF_FFFF;
            }
            if trb_param(&ev) == status {
                return Some(usize::from(len) - remaining as usize);
            }
        }
    }

    /// Context `i` of the device's input context (0: input control, 1:
    /// slot, 2: endpoint 0, ...).
    fn input_ctx(&self, dev: &Device, i: usize) -> *mut u32 {
        unsafe { dev.input.1.add(i * self.csz / 4) }
    }

    /// Reset the port if needed, address the device on it, and set it up if
    /// it's a keyboard.
    fn setup_port(&mut self, port: u8) -> Option<Keyboard> {
        let reg = self.port_reg(port);
        let sc = rd(reg);
        if sc & PORT_CCS == 0 {
            return None;
        }
        if sc & PORT_PED == 0 {
            // USB 2 ports need a reset to enable them (USB 3 ones do it themselves).
            wr(reg, (sc & PORT_KEEP) | PORT_PR);
            wait(|| rd(reg) & PORT_PRC != 0)?;
            wr(reg, (rd(reg) & PORT_KEEP) | PORT_PRC);
            wait(|| rd(reg) & PORT_PED != 0)?;
        }
        let speed = (rd(reg) >> 10) & 0xF;

        let ev = self.command(0, TRB_ENABLE_SLOT << 10)?;
        let slot = (ev[3] >> 24) as u8;
        let (out_phys, _) = dma_page()?;
        unsafe { write_volatile(self.dcbaa.add(slot as usize), out_phys) };
        let mut dev = Device { slot, port, input: dma_page()?, ep0: Ring::new()?, buf: dma_page()? };

        // Address it: slot context and endpoint 0.
        let mps: u32 = match speed {
            2 | 1 => 8, // low / full speed (full speed may be bigger: read below)
            3 => 64,    // high speed
            _ => 512,   // super speed
        };
        unsafe {
            write_volatile(self.input_ctx(&dev, 0).add(1), 0b11); // add slot, ep 0
            let s = self.input_ctx(&dev, 1);
            write_volatile(s, speed << 20 | 1 << 27);
            write_volatile(s.add(1), u32::from(port) << 16);
            let e = self.input_ctx(&dev, 2);
            write_volatile(e.add(1), 3 << 1 | 4 << 3 | mps << 16); // 3 retries, control
            write_volatile(e.add(2), dev.ep0.phys as u32 | 1);
            write_volatile(e.add(3), (dev.ep0.phys >> 32) as u32);
            write_volatile(e.add(4), 8);
        }
        self.command(dev.input.0, TRB_ADDRESS_DEVICE << 10 | u32::from(slot) << 24)?;

        // The first 8 bytes of the device descriptor: endpoint 0's real size.
        self.control(&mut dev, 0x80, 6, 0x0100, 0, 8)?;
        let real_mps = u32::from(unsafe { *(dev.buf.1 as *const u8).add(7) });
        if speed < 3 && real_mps != mps && real_mps != 0 {
            unsafe {
                write_volatile(self.input_ctx(&dev, 0).add(1), 0b10);
                let e = self.input_ctx(&dev, 2);
                write_volatile(e.add(1), 3 << 1 | 4 << 3 | real_mps << 16);
            }
            self.command(dev.input.0, TRB_EVALUATE_CONTEXT << 10 | u32::from(slot) << 24)?;
        }

        // The configuration descriptor, with its interfaces and endpoints.
        self.control(&mut dev, 0x80, 6, 0x0200, 0, 9)?;
        let total = u16::from_le_bytes(unsafe { [*(dev.buf.1 as *const u8).add(2), *(dev.buf.1 as *const u8).add(3)] }).min(4096);
        let got = self.control(&mut dev, 0x80, 6, 0x0200, 0, total)?;
        let desc = unsafe { core::slice::from_raw_parts(dev.buf.1 as *const u8, got) }.to_vec();
        let config_value = desc[5];
        let hid = find_hid(&desc)?;
        let (iface, ep_addr, maxp, interval) = (hid.iface, hid.endpoint, hid.max_packet, hid.interval);

        self.control(&mut dev, 0x00, 9, u16::from(config_value), 0, 0)?; // SET_CONFIGURATION
        let pointer = match hid.protocol {
            1 => None, // a keyboard
            2 => Some(Layout::Boot),
            _ => {
                // Its report descriptor: an absolute pointer, or nothing used here.
                let len = hid.report_len.clamp(1, 4096);
                let got = self.control(&mut dev, 0x81, 6, 0x2200, u16::from(iface), len)?;
                let rd = unsafe { core::slice::from_raw_parts(dev.buf.1 as *const u8, got) };
                Some(Layout::Absolute(parse_absolute(rd)?))
            }
        };
        if hid.protocol != 0 {
            self.control(&mut dev, 0x21, 0x0B, 0, u16::from(iface), 0)?; // SET_PROTOCOL: boot
        }
        let _ = self.control(&mut dev, 0x21, 0x0A, 0, u16::from(iface), 0); // SET_IDLE: only on changes

        // Configure the interrupt IN endpoint.
        let dci = (ep_addr & 0xF) * 2 + 1;
        let ring = Ring::new()?;
        let interval_exp = if speed >= 3 {
            u32::from(interval.clamp(1, 16)) - 1
        } else {
            // Frames of 1 ms, as a power of two of 125 us.
            (31 - (u32::from(interval.max(1)) * 8).leading_zeros()).clamp(3, 10)
        };
        unsafe {
            core::ptr::write_bytes(dev.input.1 as *mut u8, 0, 4096);
            write_volatile(self.input_ctx(&dev, 0).add(1), 1 | 1 << dci);
            let s = self.input_ctx(&dev, 1);
            write_volatile(s, speed << 20 | u32::from(dci) << 27);
            write_volatile(s.add(1), u32::from(port) << 16);
            let e = self.input_ctx(&dev, dci as usize + 1);
            write_volatile(e, interval_exp << 16);
            write_volatile(e.add(1), 3 << 1 | 7 << 3 | u32::from(maxp) << 16); // interrupt IN
            write_volatile(e.add(2), ring.phys as u32 | 1);
            write_volatile(e.add(3), (ring.phys >> 32) as u32);
            write_volatile(e.add(4), 8 | u32::from(maxp) << 16);
        }
        self.command(dev.input.0, TRB_CONFIGURE_EP << 10 | u32::from(slot) << 24)?;
        let _ = dev.port;
        let (rphys, rvirt) = dma_page()?;
        Some(Keyboard {
            slot,
            dci,
            ring,
            reports: (rphys, rvirt as *mut u8),
            slot_of_trb: [0; RING_TRBS],
            length: u32::from(maxp.clamp(8, REPORT_SIZE as u16)),
            prev: [0; 8],
            held: None,
            pointer: pointer.map(|layout| Pointer {
                layout,
                screen: crate::vga_buffer::framebuffer().map_or((1280, 720), |f| (f.2 as i32, f.3 as i32)),
                at: None,
                buttons: 0,
            }),
        })
    }

    /// Queue a transfer for every report buffer of keyboard `k`.
    fn queue_reports(&mut self, k: usize) {
        for b in 0..REPORT_SLOTS {
            self.queue_report(k, b);
        }
    }

    fn queue_report(&mut self, k: usize, buffer: usize) {
        let kbd = &mut self.keyboards[k];
        let index = if kbd.ring.index == RING_TRBS - 1 { 0 } else { kbd.ring.index };
        kbd.slot_of_trb[index] = buffer as u8;
        let addr = kbd.reports.0 + (buffer * REPORT_SIZE) as u64;
        // Interrupt on completion, and on a short packet.
        kbd.ring.push(addr, kbd.length, (TRB_NORMAL << 10) | (1 << 5) | (1 << 2));
        let (slot, dci) = (kbd.slot, kbd.dci);
        self.ring_doorbell(slot, u32::from(dci));
    }

    /// Handle what the controller finished since last time.
    fn poll(&mut self) {
        let mut any = false;
        while let Some(ev) = self.events.next() {
            any = true;
            if trb_type(&ev) != TRB_EV_TRANSFER {
                continue;
            }
            let slot = (ev[3] >> 24) as u8;
            let dci = ((ev[3] >> 16) & 0x1F) as u8;
            let Some(k) = self.keyboards.iter().position(|kb| kb.slot == slot && kb.dci == dci) else { continue };
            let cc = completion(&ev);
            let kbd = &mut self.keyboards[k];
            let index = ((trb_param(&ev).wrapping_sub(kbd.ring.phys)) / 16) as usize;
            let buffer = kbd.slot_of_trb[index.min(RING_TRBS - 1)] as usize;
            if cc == CC_SUCCESS || cc == CC_SHORT_PACKET {
                let mut report = [0u8; REPORT_SIZE];
                unsafe { core::ptr::copy_nonoverlapping(kbd.reports.1.add(buffer * REPORT_SIZE), report.as_mut_ptr(), REPORT_SIZE) };
                let len = (kbd.length as usize).saturating_sub((ev[2] & 0xFF_FFFF) as usize).min(REPORT_SIZE);
                match kbd.pointer.as_mut() {
                    Some(p) => p.report(&report[..len]),
                    None => kbd.report(report[..8].try_into().unwrap()),
                }
            }
            self.queue_report(k, buffer);
        }
        if any {
            self.update_dequeue();
        }
        for kbd in &mut self.keyboards {
            kbd.tick();
        }
    }
}

/// The largest move of an absolute pointer's pins (see Pointer::move_to).
const PIN_STEP: i32 = 45;

/// A HID interface of a configuration descriptor.
struct HidInterface {
    iface: u8,
    /// 1 a keyboard, 2 a mouse (boot protocol), 0 neither (a tablet...).
    protocol: u8,
    endpoint: u8,
    max_packet: u16,
    interval: u8,
    /// The length of its report descriptor.
    report_len: u16,
}

/// In a configuration descriptor, the HID interface to use (a keyboard if
/// there's one, else the first pointer-ish one) with its interrupt IN
/// endpoint.
fn find_hid(desc: &[u8]) -> Option<HidInterface> {
    let mut i = 0;
    let mut cur: Option<HidInterface> = None;
    let mut best: Option<HidInterface> = None;
    while i + 2 <= desc.len() {
        let (len, kind) = (desc[i] as usize, desc[i + 1]);
        if len < 2 || i + len > desc.len() {
            break;
        }
        let d = &desc[i..i + len];
        match kind {
            4 if len >= 9 => {
                cur = (d[5] == 3 && d[7] <= 2).then_some(HidInterface {
                    iface: d[2],
                    protocol: if d[6] == 1 { d[7] } else { 0 },
                    endpoint: 0,
                    max_packet: 0,
                    interval: 0,
                    report_len: 0,
                });
            }
            0x21 if len >= 9 => {
                if let Some(c) = cur.as_mut() {
                    if d[6] == 0x22 {
                        c.report_len = u16::from_le_bytes([d[7], d[8]]);
                    }
                }
            }
            5 if len >= 7 => {
                if let Some(mut c) = cur.take() {
                    if d[2] & 0x80 != 0 && d[3] & 3 == 3 {
                        c.endpoint = d[2];
                        c.max_packet = u16::from_le_bytes([d[4], d[5]]) & 0x7FF;
                        c.interval = d[6];
                        if c.protocol == 1 {
                            return Some(c);
                        }
                        if best.is_none() {
                            best = Some(c);
                        }
                    } else {
                        cur = Some(c);
                    }
                }
            }
            _ => {}
        }
        i += len;
    }
    best
}

/// From a HID report descriptor, where an absolute pointer's input report
/// keeps X, Y (absolute), the first three buttons and the wheel; None if
/// it isn't one.
fn parse_absolute(d: &[u8]) -> Option<Absolute> {
    let (mut page, mut lmin, mut lmax, mut size, mut count, mut id) = (0u32, 0i32, 0i32, 0u32, 0u32, 0u8);
    let mut usages: Vec<u32> = Vec::new();
    let mut usage_min: Option<u32> = None;
    let mut offset = 0u32; // bits into the current input report
    let (mut x, mut y, mut wheel, mut buttons) = (None, None, None, [None; 3]);
    let (mut x_id, mut absolute) = (0u8, false);
    let mut i = 0;
    while i < d.len() {
        let b = d[i];
        if b == 0xFE {
            // A long item: skipped.
            i += 3 + usize::from(*d.get(i + 1)?);
            continue;
        }
        let n = [0, 1, 2, 4][usize::from(b & 3)];
        if i + 1 + n > d.len() {
            break;
        }
        let raw = d[i + 1..i + 1 + n].iter().rev().fold(0u32, |a, &c| a << 8 | u32::from(c));
        let signed = match n {
            1 => i32::from(raw as u8 as i8),
            2 => i32::from(raw as u16 as i16),
            _ => raw as i32,
        };
        let usage = |raw: u32| if n == 4 { raw } else { page << 16 | raw };
        match ((b >> 2) & 3, b >> 4) {
            (1, 0) => page = raw,
            (1, 1) => lmin = signed,
            // (A maximum is unsigned unless the minimum is negative.)
            (1, 2) => lmax = if lmin >= 0 && n < 4 { raw as i32 } else { signed },
            (1, 7) => size = raw,
            (1, 8) => {
                id = raw as u8;
                offset = 0;
            }
            (1, 9) => count = raw,
            (2, 0) => usages.push(usage(raw)),
            (2, 1) => usage_min = Some(usage(raw)),
            (0, 8) => {
                // An input item: `count` fields of `size` bits.
                for k in 0..count {
                    let u = match usages.get(k as usize).or(usages.last()) {
                        Some(&u) => u,
                        None => usage_min.map_or(0, |m| m + k),
                    };
                    let f = Field { offset, size, min: lmin, max: lmax };
                    if raw & 1 == 0 {
                        // (Not a constant: padding.)
                        match u {
                            0x0001_0030 => {
                                x = Some(f);
                                x_id = id;
                                absolute = raw & 4 == 0;
                            }
                            0x0001_0031 => y = Some(f),
                            0x0001_0038 => wheel = Some(f),
                            0x0009_0001..=0x0009_0003 => buttons[(u & 0xFFFF) as usize - 1] = Some(f),
                            _ => {}
                        }
                    }
                    offset += size;
                }
                usages.clear();
                usage_min = None;
            }
            (0, _) => {
                // Output, feature, collections: their usages are used up.
                usages.clear();
                usage_min = None;
            }
            _ => {}
        }
        i += 1 + n;
    }
    let (x, y) = (x?, y?);
    (absolute && x.max > x.min && y.max > y.min).then_some(Absolute { report_id: x_id, x, y, buttons, wheel })
}

/// The value of field `f` of a report.
fn field(data: &[u8], f: Field) -> i32 {
    let mut v = 0u32;
    for i in 0..f.size.min(32) {
        let bit = f.offset + i;
        if data.get((bit / 8) as usize).is_some_and(|&byte| byte >> (bit % 8) & 1 != 0) {
            v |= 1 << i;
        }
    }
    if f.min < 0 && (1..32).contains(&f.size) && v & (1 << (f.size - 1)) != 0 {
        (v | !0 << f.size) as i32
    } else {
        v as i32
    }
}

impl Pointer {
    fn report(&mut self, data: &[u8]) {
        match &self.layout {
            Layout::Boot => {
                // Buttons, then moves right and down, and maybe the wheel (up: positive).
                if data.len() >= 3 {
                    let wheel = data.get(3).map_or(0, |&w| i32::from(w as i8));
                    crate::mouse::report(data[0] & 7, i32::from(data[1] as i8), -i32::from(data[2] as i8), -wheel);
                }
            }
            Layout::Absolute(l) => {
                let data = match l.report_id {
                    0 => data,
                    id if data.first() == Some(&id) => &data[1..],
                    _ => return, // another report
                };
                let scale = |f: Field, extent: i32| {
                    let v = field(data, f).clamp(f.min, f.max);
                    (i64::from(v - f.min) * i64::from(extent - 1) / i64::from(f.max - f.min)) as i32
                };
                let (tx, ty) = (scale(l.x, self.screen.0), scale(l.y, self.screen.1));
                let mut buttons = 0;
                for (bit, f) in l.buttons.iter().enumerate() {
                    if f.is_some_and(|f| field(data, f) != 0) {
                        buttons |= 1 << bit;
                    }
                }
                let wheel = l.wheel.map_or(0, |f| field(data, f));
                if crate::mouse::take_resync() {
                    self.at = None; // (a program set the mouse up: where X put the pointer isn't known)
                }
                self.move_to(tx, ty);
                if buttons != self.buttons || wheel != 0 {
                    // (After the move: a click lands where the pointer went.)
                    self.buttons = buttons;
                    crate::mouse::report(buttons, 0, 0, -wheel);
                }
            }
        }
    }

    /// Move the pointer to (x, y) on the screen with relative moves.
    fn move_to(&mut self, x: i32, y: i32) {
        let b = self.buttons;
        // Into a corner or against an edge: moves past it (X stops the
        // pointer there). At most 45 pixels each: while X's mouse driver
        // is still working out the protocol, a move of more than 50 makes
        // it give up on the right one.
        let pin = |dx: i32, dy: i32, extent: i32| {
            for _ in 0..=extent / PIN_STEP {
                crate::mouse::report(b, dx, dy, 0);
            }
        };
        let (mut cx, mut cy) = match self.at {
            Some(at) => at,
            None => {
                pin(-PIN_STEP, 0, self.screen.0);
                pin(0, PIN_STEP, self.screen.1);
                (0, 0)
            }
        };
        if x == 0 && cx != 0 {
            pin(-PIN_STEP, 0, self.screen.0);
            cx = 0;
        }
        if y == 0 && cy != 0 {
            pin(0, PIN_STEP, self.screen.1);
            cy = 0;
        }
        // Then steps of at most 2 pixels a side (at most 2.9 long: X
        // accelerates moves of more than 4).
        let (dx, dy) = (x - cx, y - cy);
        let n = (dx.abs().max(dy.abs()) + 1) / 2;
        let (mut px, mut py) = (0, 0);
        for i in 1..=n {
            let (sx, sy) = (dx * i / n, dy * i / n);
            crate::mouse::report(b, sx - px, -(sy - py), 0); // (PS/2: up is positive)
            px = sx;
            py = sy;
        }
        self.at = Some((x, y));
    }
}

impl Keyboard {
    /// A new report: compare with the last one.
    fn report(&mut self, now: [u8; 8]) {
        if now[2] == 1 {
            return; // "too many keys": keep the old state
        }
        let prev = self.prev;
        for bit in 0..8 {
            let (was, is) = (prev[0] & (1 << bit) != 0, now[0] & (1 << bit) != 0);
            if was != is {
                emit(MODIFIERS[bit], is);
            }
        }
        for &key in &prev[2..8] {
            if key > 3 && !now[2..8].contains(&key) {
                emit(usage_to_scancode(key), false);
                if self.held.is_some_and(|(k, _)| k == key) {
                    self.held = None;
                }
            }
        }
        for &key in &now[2..8] {
            if key > 3 && !prev[2..8].contains(&key) {
                emit(usage_to_scancode(key), true);
                self.held = Some((key, 0));
            }
        }
        self.prev = now;
    }

    /// Key repeat, once per timer tick.
    fn tick(&mut self) {
        if let Some((key, ticks)) = self.held.as_mut() {
            *ticks += 1;
            let delay = ms_to_ticks(REPEAT_DELAY_MS);
            if *ticks > delay && (*ticks - delay) % ms_to_ticks(REPEAT_EVERY_MS) == 0 {
                let code = usage_to_scancode(*key);
                emit(code, true);
            }
        }
    }
}

/// Send a key press or release as PS/2 (set 1) scancodes. 0xE0xx codes are
/// the extended keys (arrows, ...), sent with their 0xE0 prefix.
fn emit(code: u16, press: bool) {
    if code == 0 {
        return;
    }
    let send = |b: u8| {
        if !crate::tty::intercept_scancode(b) {
            crate::task::keyboard::add_scancode(b);
        }
    };
    if code >> 8 == 0xE0 {
        send(0xE0);
    }
    send((code as u8) | if press { 0 } else { 0x80 });
}

/// Modifier bits of a report: left Ctrl, Shift, Alt, GUI, then right.
const MODIFIERS: [u16; 8] = [0x1D, 0x2A, 0x38, 0xE05B, 0xE01D, 0x36, 0xE038, 0xE05C];

/// HID keyboard usage -> PS/2 scancode (set 1), 0 if none.
fn usage_to_scancode(usage: u8) -> u16 {
    const LETTERS: [u8; 26] = [
        0x1E, 0x30, 0x2E, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31, 0x18, 0x19, 0x10, 0x13,
        0x1F, 0x14, 0x16, 0x2F, 0x11, 0x2D, 0x15, 0x2C,
    ];
    match usage {
        0x04..=0x1D => u16::from(LETTERS[(usage - 0x04) as usize]),
        0x1E..=0x27 => u16::from(usage - 0x1E + 0x02), // 1-9, 0
        0x28 => 0x1C,                                  // Enter
        0x29 => 0x01,                                  // Escape
        0x2A => 0x0E,                                  // Backspace
        0x2B => 0x0F,                                  // Tab
        0x2C => 0x39,                                  // Space
        0x2D => 0x0C,
        0x2E => 0x0D,
        0x2F => 0x1A,
        0x30 => 0x1B,
        0x31 | 0x32 => 0x2B, // \ (and the non-US # key)
        0x33 => 0x27,
        0x34 => 0x28,
        0x35 => 0x29,
        0x36 => 0x33,
        0x37 => 0x34,
        0x38 => 0x35,
        0x39 => 0x3A,                                  // Caps Lock
        0x3A..=0x43 => u16::from(usage - 0x3A + 0x3B), // F1-F10
        0x44 => 0x57,
        0x45 => 0x58,
        0x47 => 0x46, // Scroll Lock
        0x49 => 0xE052,
        0x4A => 0xE047,
        0x4B => 0xE049,
        0x4C => 0xE053,
        0x4D => 0xE04F,
        0x4E => 0xE051,
        0x4F => 0xE04D, // Right
        0x50 => 0xE04B, // Left
        0x51 => 0xE050, // Down
        0x52 => 0xE048, // Up
        0x53 => 0x45,   // Num Lock
        0x54 => 0xE035,
        0x55 => 0x37,
        0x56 => 0x4A,
        0x57 => 0x4E,
        0x58 => 0xE01C, // keypad Enter
        0x59 => 0x4F,
        0x5A => 0x50,
        0x5B => 0x51,
        0x5C => 0x4B,
        0x5D => 0x4C,
        0x5E => 0x4D,
        0x5F => 0x47,
        0x60 => 0x48,
        0x61 => 0x49,
        0x62 => 0x52,
        0x63 => 0x53,
        0x64 => 0x56,
        0x65 => 0xE05D, // Menu
        _ => 0,
    }
}

/// From the timer interrupt: handle finished transfers (key reports) and
/// key repeat. Skips a tick if the driver is busy (it's being set up).
pub fn poll() {
    if let Some(mut guard) = XHCI.try_lock() {
        if let Some(hc) = guard.as_mut() {
            hc.poll();
        }
    }
}

/// How many USB keyboards are set up.
pub fn keyboards() -> usize {
    XHCI.try_lock().and_then(|g| g.as_ref().map(|h| h.keyboards.iter().filter(|k| k.pointer.is_none()).count())).unwrap_or(0)
}

/// How many USB pointers (mice, tablets) are set up.
pub fn pointers() -> usize {
    XHCI.try_lock().and_then(|g| g.as_ref().map(|h| h.keyboards.iter().filter(|k| k.pointer.is_some()).count())).unwrap_or(0)
}

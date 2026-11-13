// Copyright (c) 2026 tre4surehunter9
//! virtio-input (QEMU's -device virtio-keyboard-pci, virtio-tablet-pci,
//! virtio-mouse-pci): keyboards and pointers made for virtual machines.
//! Each sends Linux input events (struct input_event's type, code and
//! value) on its event queue: keys and buttons (EV_KEY), relative moves
//! and the wheel (EV_REL), absolute positions (EV_ABS), and EV_SYN when a
//! group of them is complete.
//!
//! Keys become PS/2 scancodes, as the USB keyboards' do (Linux's key codes
//! 1..88 *are* set 1 scancodes; the others are the 0xE0 ones); pointers
//! become /dev/input/mice moves (a tablet's positions through
//! mouse::Tablet). Polled from the timer, like the USB devices.
//!
//! The device's configuration answers questions: write what's asked
//! (select: 1 its name, 0x11 the event types/codes it sends, 0x12 an
//! axis's range) and a detail (subsel), read the size and the answer.

use alloc::vec::Vec;
use spin::Mutex;

use crate::virtio::{self, Device, Queue};

const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const EV_REL: u16 = 2;
const EV_ABS: u16 = 3;
const REL_X: u16 = 0;
const REL_Y: u16 = 1;
const REL_WHEEL: u16 = 8;
const ABS_X: u16 = 0;
const ABS_Y: u16 = 1;
const BTN_LEFT: u16 = 0x110;
const BTN_RIGHT: u16 = 0x111;
const BTN_MIDDLE: u16 = 0x112;
const CFG_ID_NAME: u8 = 0x01;
const CFG_EV_BITS: u8 = 0x11;
const CFG_ABS_INFO: u8 = 0x12;
/// Event buffers always with the device (8 bytes each, in one page).
const EVENTS: u16 = 64;

enum Kind {
    Keyboard,
    /// Absolute: each axis's range (min, max).
    Tablet { x: (i32, i32), y: (i32, i32), tablet: crate::mouse::Tablet },
    Mouse,
}

struct Input {
    _dev: Device,
    events: Queue,
    buf: (u64, *mut u8),
    kind: Kind,
    /// Since the last EV_SYN: the position or moves, the wheel, buttons.
    x: i32,
    y: i32,
    wheel: i32,
    buttons: u8,
}

unsafe impl Send for Input {}

static INPUTS: Mutex<Vec<Input>> = Mutex::new(Vec::new());

/// Whether the device sends events of `ev` type, code `code`.
fn sends(dev: &Device, ev: u8, code: u16) -> bool {
    dev.set_config8(0, CFG_EV_BITS);
    dev.set_config8(1, ev);
    let size = dev.config8(2) as u16;
    code / 8 < size && dev.config8(8 + (code / 8) as usize) >> (code % 8) & 1 != 0
}

/// An absolute axis's range.
fn axis(dev: &Device, code: u8) -> (i32, i32) {
    dev.set_config8(0, CFG_ABS_INFO);
    dev.set_config8(1, code);
    if dev.config8(2) < 8 {
        return (0, 0x7fff);
    }
    (dev.config32(8) as i32, dev.config32(12) as i32)
}

/// The device's name (for the boot log).
fn name(dev: &Device) -> alloc::string::String {
    dev.set_config8(0, CFG_ID_NAME);
    dev.set_config8(1, 0);
    let n = (dev.config8(2) as usize).min(128);
    (0..n).map(|i| dev.config8(8 + i) as char).collect()
}

/// Find the virtio input devices and set them up. Returns their names.
pub fn init() -> Vec<alloc::string::String> {
    let mut names = Vec::new();
    let mut inputs = INPUTS.lock();
    for pci in virtio::find(18) {
        let Some((dev, _)) = Device::new(pci, 0) else { continue };
        dev.no_interrupts(); // (polled from the timer)
        crate::mouse::reserve(); // (events are taken from the timer)
        let kind = if sends(&dev, EV_ABS as u8, ABS_X) {
            Kind::Tablet { x: axis(&dev, ABS_X as u8), y: axis(&dev, ABS_Y as u8), tablet: crate::mouse::Tablet::new() }
        } else if sends(&dev, EV_REL as u8, REL_X) {
            Kind::Mouse
        } else {
            Kind::Keyboard
        };
        let Some(mut events) = dev.queue(0, EVENTS) else { continue };
        let Some(buf) = virtio::dma_page() else { continue };
        names.push(name(&dev));
        dev.start();
        for i in 0..events.size().min(EVENTS) {
            events.submit(i, buf.0 + 8 * i as u64, 8, true);
        }
        events.kick();
        inputs.push(Input { _dev: dev, events, buf, kind, x: 0, y: 0, wheel: 0, buttons: 0 });
    }
    names
}

/// Linux key code -> PS/2 set 1 scancode (0xE0xx: an extended key).
pub fn scancode(code: u16) -> u16 {
    match code {
        1..=88 => code, // (the same numbers, by design)
        96 => 0xE01C,   // keypad Enter
        97 => 0xE01D,   // right Ctrl
        98 => 0xE035,   // keypad /
        99 => 0xE037,   // SysRq / Print Screen
        100 => 0xE038,  // right Alt
        102 => 0xE047,  // Home
        103 => 0xE048,  // Up
        104 => 0xE049,  // Page Up
        105 => 0xE04B,  // Left
        106 => 0xE04D,  // Right
        107 => 0xE04F,  // End
        108 => 0xE050,  // Down
        109 => 0xE051,  // Page Down
        110 => 0xE052,  // Insert
        111 => 0xE053,  // Delete
        125 => 0xE05B,  // left Windows key
        126 => 0xE05C,  // right Windows key
        127 => 0xE05D,  // Menu
        _ => 0,
    }
}

impl Input {
    fn event(&mut self, ty: u16, code: u16, value: i32) {
        match (ty, code) {
            (EV_KEY, BTN_LEFT | BTN_RIGHT | BTN_MIDDLE) => {
                let bit = match code {
                    BTN_LEFT => 1,
                    BTN_RIGHT => 2,
                    _ => 4,
                };
                if value != 0 { self.buttons |= bit } else { self.buttons &= !bit }
            }
            // Keys: 1 pressed, 0 released, 2 repeated (as pressed again).
            (EV_KEY, _) => crate::xhci::emit(scancode(code), value != 0),
            (EV_REL, REL_X) => self.x += value,
            (EV_REL, REL_Y) => self.y += value,
            (EV_REL, REL_WHEEL) => self.wheel += value,
            (EV_ABS, ABS_X) => self.x = value,
            (EV_ABS, ABS_Y) => self.y = value,
            (EV_SYN, _) => self.sync(),
            _ => {}
        }
    }

    /// A group of events is complete: tell programs.
    fn sync(&mut self) {
        match &mut self.kind {
            Kind::Keyboard => {}
            Kind::Mouse => {
                crate::mouse::report(self.buttons, self.x, -self.y, self.wheel); // (PS/2: up is positive)
                self.x = 0;
                self.y = 0;
            }
            Kind::Tablet { x, y, tablet } => {
                let scale = |v: i32, (min, max): (i32, i32), extent: i32| {
                    if max <= min {
                        return 0;
                    }
                    (i64::from(v.clamp(min, max) - min) * i64::from(extent - 1) / i64::from(max - min)) as i32
                };
                let (w, h) = tablet.screen;
                let (sx, sy) = (scale(self.x, *x, w), scale(self.y, *y, h));
                tablet.update(sx, sy, self.buttons, self.wheel);
            }
        }
        self.wheel = 0;
    }

    fn poll(&mut self) {
        let mut any = false;
        while let Some((id, len)) = self.events.take_used() {
            if len >= 8 && id < EVENTS {
                let e = unsafe { core::slice::from_raw_parts(self.buf.1.add(8 * id as usize), 8) };
                let ty = u16::from_le_bytes([e[0], e[1]]);
                let code = u16::from_le_bytes([e[2], e[3]]);
                let value = i32::from_le_bytes([e[4], e[5], e[6], e[7]]);
                self.event(ty, code, value);
            }
            // The buffer goes back for the next event.
            self.events.submit(id, self.buf.0 + 8 * id as u64, 8, true);
            any = true;
        }
        if any {
            self.events.kick();
        }
    }
}

/// From the timer: take the devices' events. (No allocation here: the
/// timer can interrupt kernel code that's allocating.)
pub fn poll() {
    if let Some(mut inputs) = INPUTS.try_lock() {
        for i in inputs.iter_mut() {
            i.poll();
        }
    }
}

/// How many virtio input devices are set up.
pub fn count() -> usize {
    INPUTS.try_lock().map_or(0, |i| i.len())
}

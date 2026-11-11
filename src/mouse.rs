// Copyright (c) 2026 tre4surehunter9
//! The mouse: a PS/2 mouse on the keyboard controller's second port
//! (IRQ 12), read by programs from /dev/input/mice (and /dev/psaux).
//!
//! Like Linux's mousedev, /dev/input/mice speaks the PS/2 mouse protocol:
//! 3-byte packets (buttons and signs, dx, dy), or 4 bytes (a wheel) once a
//! program switches it to the IntelliMouse protocol the usual way (sample
//! rates 200, 100, 80). Commands written to it are answered as a PS/2 mouse
//! would (0xFA, and the reply to "get ID", "reset", "status").

use alloc::collections::VecDeque;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;
use x86_64::instructions::port::Port;

struct Mice {
    /// Bytes waiting to be read.
    queue: VecDeque<u8>,
    /// The packet being received from the mouse.
    packet: [u8; 3],
    got: usize,
    /// IntelliMouse protocol (4-byte packets) after the magic rate sequence.
    imps2: bool,
    /// The last sample rates set (to spot the sequence), and whether the
    /// last command byte was "set rate" (its argument comes next).
    rates: [u8; 3],
    rate_next: bool,
}

static MICE: Mutex<Mice> =
    Mutex::new(Mice { queue: VecDeque::new(), packet: [0; 3], got: 0, imps2: false, rates: [0; 3], rate_next: false });
static PRESENT: AtomicBool = AtomicBool::new(false);
const MAX_QUEUED: usize = 4096;

fn wait_write() {
    for _ in 0..100_000 {
        if unsafe { Port::<u8>::new(0x64).read() } & 2 == 0 {
            return;
        }
    }
}

fn wait_read() -> bool {
    for _ in 0..100_000 {
        if unsafe { Port::<u8>::new(0x64).read() } & 1 != 0 {
            return true;
        }
    }
    false
}

fn controller(cmd: u8) {
    wait_write();
    unsafe { Port::<u8>::new(0x64).write(cmd) };
}

/// Send a byte to the mouse; true if it acknowledged (0xFA).
fn to_mouse(byte: u8) -> bool {
    controller(0xD4); // the next data byte goes to the second port
    wait_write();
    unsafe { Port::<u8>::new(0x60).write(byte) };
    wait_read() && unsafe { Port::<u8>::new(0x60).read() } == 0xFA
}

/// Turn on the PS/2 mouse, if there is one. Returns whether it answered.
pub fn init() -> bool {
    x86_64::instructions::interrupts::without_interrupts(|| {
        controller(0xA8); // enable the second port
        // The configuration byte: interrupts from the second port on, its
        // clock on.
        controller(0x20);
        if !wait_read() {
            return false;
        }
        let config = unsafe { Port::<u8>::new(0x60).read() };
        controller(0x60);
        wait_write();
        unsafe { Port::<u8>::new(0x60).write((config | 0x02) & !0x20) };
        // Defaults, then report movements.
        if !to_mouse(0xF6) || !to_mouse(0xF4) {
            return false;
        }
        PRESENT.store(true, Ordering::SeqCst);
        crate::interrupts::unmask_irq(12);
        true
    })
}

pub fn present() -> bool {
    PRESENT.load(Ordering::SeqCst)
}

/// From IRQ 12: a byte from the mouse. Whole packets go to the queue.
pub fn interrupt(byte: u8) {
    let Some(mut m) = MICE.try_lock() else { return };
    // The first byte of a packet always has bit 3 set: resynchronize on it.
    if m.got == 0 && byte & 0x08 == 0 {
        return;
    }
    let i = m.got;
    m.packet[i] = byte;
    m.got += 1;
    if m.got == 3 {
        m.got = 0;
        let p = m.packet;
        let imps2 = m.imps2;
        if m.queue.len() + 4 <= MAX_QUEUED {
            m.queue.extend(p);
            if imps2 {
                m.queue.push_back(0); // no wheel movement
            }
        }
        drop(m);
        crate::task::keyboard::note_arrival(); // wakes readers
    }
}

/// A packet from another mouse (a USB one): buttons (bit 0 left, 1 right,
/// 2 middle), movement right and *up* (as PS/2 counts), and the wheel.
pub fn report(buttons: u8, dx: i32, dy: i32, wheel: i32) {
    let mut m = MICE.lock();
    let dx = dx.clamp(-255, 255);
    let dy = dy.clamp(-255, 255);
    let flags = 0x08 | (buttons & 7) | if dx < 0 { 0x10 } else { 0 } | if dy < 0 { 0x20 } else { 0 };
    if m.queue.len() + 4 <= MAX_QUEUED {
        m.queue.extend([flags, dx as u8, dy as u8]);
        if m.imps2 {
            m.queue.push_back(wheel.clamp(-8, 7) as i8 as u8);
        }
    }
    drop(m);
    crate::task::keyboard::note_arrival();
}

pub fn readable() -> bool {
    !MICE.lock().queue.is_empty()
}

/// read(): queued bytes, waiting for some unless non-blocking.
pub fn read(buf: &mut [u8], nonblocking: bool) -> i64 {
    if buf.is_empty() {
        return 0;
    }
    loop {
        {
            let mut m = MICE.lock();
            if !m.queue.is_empty() {
                let n = buf.len().min(m.queue.len());
                for (dst, src) in buf.iter_mut().zip(m.queue.drain(..n)) {
                    *dst = src;
                }
                return n as i64;
            }
        }
        if nonblocking {
            return -crate::syscall::EAGAIN;
        }
        if crate::process::signal_pending() {
            return -crate::syscall::EINTR;
        }
        crate::process::block_on(crate::process::CONSOLE_KEY);
    }
}

/// write(): PS/2 commands, answered as a mouse would.
pub fn write(bytes: &[u8]) -> i64 {
    let mut m = MICE.lock();
    for &b in bytes {
        if m.rate_next {
            m.rate_next = false;
            m.rates = [m.rates[1], m.rates[2], b];
            if m.rates == [200, 100, 80] {
                m.imps2 = true;
            }
            m.queue.push_back(0xFA);
            continue;
        }
        m.queue.push_back(0xFA);
        match b {
            0xF3 => m.rate_next = true, // set sample rate: the rate follows
            0xF2 => {
                let id = if m.imps2 { 3 } else { 0 };
                m.queue.push_back(id); // get device ID
            }
            0xFF => {
                m.imps2 = false;
                m.queue.extend([0xAA, 0x00]); // reset: self-test passed, ID 0
            }
            0xE9 => m.queue.extend([0x00, 0x02, 0x64]), // status: 2 counts/mm, 100/s
            0xF6 => m.imps2 = false, // defaults
            _ => {}
        }
    }
    drop(m);
    crate::task::keyboard::note_arrival();
    bytes.len() as i64
}

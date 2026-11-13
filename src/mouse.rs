// Copyright (c) 2026 tre4surehunter9
//! The mouse: a PS/2 mouse on the keyboard controller's second port
//! (IRQ 12), read by programs from /dev/input/mice (and /dev/psaux).
//!
//! Like Linux's mousedev, /dev/input/mice speaks the PS/2 mouse protocol:
//! 3-byte packets (buttons and signs, dx, dy), or 4 bytes (a wheel) once a
//! program switches it to the IntelliMouse protocol the usual way (sample
//! rates 200, 100, 80). Commands written to it are answered as a PS/2 mouse
//! would (0xFA, and the reply to "get ID", "reset", "status"), and, as a
//! mouse stops to answer (and Linux's mousedev does), the answers come
//! before any movement, and movement not yet read is dropped: X's mouse
//! driver sends its setup commands again while a USB tablet's moves stream
//! in, and finding its answers among them confused it.

use alloc::collections::VecDeque;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;
use x86_64::instructions::port::Port;

struct Mice {
    /// Bytes waiting to be read.
    queue: VecDeque<u8>,
    /// Answers to commands written: read before the queue.
    replies: VecDeque<u8>,
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
    Mutex::new(Mice {
        queue: VecDeque::new(),
        replies: VecDeque::new(),
        packet: [0; 3],
        got: 0,
        imps2: false,
        rates: [0; 3],
        rate_next: false,
    });
static PRESENT: AtomicBool = AtomicBool::new(false);
/// A program sent the mouse commands (set it up) since a USB tablet last
/// looked: its moves meanwhile may have been dropped.
static RESYNC: AtomicBool = AtomicBool::new(false);

/// For absolute pointers: whether to put the pointer back in step.
pub fn take_resync() -> bool {
    RESYNC.swap(false, Ordering::SeqCst)
}
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
        // Whatever replies are still waiting (an extra acknowledgment) would
        // be taken for the start of a packet: drop them.
        for _ in 0..16 {
            if unsafe { Port::<u8>::new(0x64).read() } & 1 == 0 {
                break;
            }
            unsafe { Port::<u8>::new(0x60).read() };
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
    // An acknowledgment (0xFA) or self-test reply (0xAA) is never a packet.
    if m.got == 0 && (byte & 0x08 == 0 || byte == 0xFA || byte == 0xAA) {
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

/// Room for the most packets ever queued, made now: other mice report
/// from the timer, which can interrupt kernel code that's allocating
/// (the heap's lock held), so reporting mustn't allocate.
pub fn reserve() {
    let mut m = MICE.lock();
    let room = MAX_QUEUED.saturating_sub(m.queue.len());
    m.queue.reserve(room);
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
    let m = MICE.lock();
    !m.queue.is_empty() || !m.replies.is_empty()
}

/// read(): queued bytes, waiting for some unless non-blocking.
pub fn read(buf: &mut [u8], nonblocking: bool) -> i64 {
    if buf.is_empty() {
        return 0;
    }
    loop {
        {
            let mut m = MICE.lock();
            let m = &mut *m;
            if !m.replies.is_empty() || !m.queue.is_empty() {
                let mut n = 0;
                for q in [&mut m.replies, &mut m.queue] {
                    let k = (buf.len() - n).min(q.len());
                    for (dst, src) in buf[n..].iter_mut().zip(q.drain(..k)) {
                        *dst = src;
                    }
                    n += k;
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
    if !bytes.is_empty() {
        m.queue.clear(); // (the mouse stops to answer)
        RESYNC.store(true, Ordering::SeqCst);
    }
    for &b in bytes {
        if m.rate_next {
            m.rate_next = false;
            m.rates = [m.rates[1], m.rates[2], b];
            if m.rates == [200, 100, 80] {
                m.imps2 = true;
            }
            m.replies.push_back(0xFA);
            continue;
        }
        m.replies.push_back(0xFA);
        match b {
            0xF3 => m.rate_next = true, // set sample rate: the rate follows
            0xF2 => {
                let id = if m.imps2 { 3 } else { 0 };
                m.replies.push_back(id); // get device ID
            }
            0xFF => {
                m.imps2 = false;
                m.replies.extend([0xAA, 0x00]); // reset: self-test passed, ID 0
            }
            0xE9 => m.replies.extend([0x00, 0x02, 0x64]), // status: 2 counts/mm, 100/s
            0xF6 => m.imps2 = false, // defaults
            _ => {}
        }
    }
    drop(m);
    crate::task::keyboard::note_arrival();
    bytes.len() as i64
}

/// The largest move of an absolute pointer's pins (see Tablet::move_to).
const PIN_STEP: i32 = 45;

/// An absolute pointer (a USB or virtio tablet: QEMU's, whose pointer
/// follows yours over its window), turned into the relative moves of
/// /dev/input/mice: programs (X) are told where the pointer goes as moves
/// from where it was.
pub struct Tablet {
    /// The screen's size (positions are given in it).
    pub screen: (i32, i32),
    /// Where the pointer is now, as far as programs were told (None until
    /// it's been put in the corner once).
    at: Option<(i32, i32)>,
    buttons: u8,
}

impl Tablet {
    pub fn new() -> Tablet {
        reserve(); // (it reports from the timer)
        let screen = crate::vga_buffer::framebuffer().map_or((1280, 720), |f| (f.2 as i32, f.3 as i32));
        Tablet { screen, at: None, buttons: 0 }
    }

    /// The pointer is at (x, y) on the screen with `buttons` down (bit 0
    /// left, 1 right, 2 middle), and the wheel turned `wheel` (up:
    /// positive).
    pub fn update(&mut self, x: i32, y: i32, buttons: u8, wheel: i32) {
        if take_resync() {
            self.at = None; // (a program set the mouse up: where X put the pointer isn't known)
        }
        self.move_to(x, y);
        if buttons != self.buttons || wheel != 0 {
            // (After the move: a click lands where the pointer went.)
            self.buttons = buttons;
            report(buttons, 0, 0, -wheel);
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
                report(b, dx, dy, 0);
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
            report(b, sx - px, -(sy - py), 0); // (PS/2: up is positive)
            px = sx;
            py = sy;
        }
        self.at = Some((x, y));
    }
}

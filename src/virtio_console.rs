// Copyright (c) 2026 tre4surehunter9
//! virtio-console (QEMU's -device virtio-serial-pci -device virtconsole):
//! a second terminal, reached from the host without the network, through
//! whatever QEMU connects it to (a Unix socket, a pseudo-terminal on the
//! host, a TCP port...). Linux calls it /dev/hvc0.
//!
//! The device is a byte pipe each way (two queues: received, to send). The
//! terminal is a pseudo-terminal pair, so it has the line discipline
//! (line editing, Ctrl-C, raw mode for full-screen programs): bytes from
//! the host are "typed" into its master end, what's displayed goes back.
//! A login runs on its slave end (/dev/pts/N): the kernel starts it (dash -c 'exec login </dev/pts/N...'), in a session of
//! its own; when it ends (logout), a fresh terminal and login follow.
//!
//! Pumped from where the GPU's screen is (the idle loop, and the timer
//! when it interrupted a program: never kernel code, which may hold
//! locks this needs).

use spin::Mutex;

use crate::pty::{self, End};
use crate::virtio::{self, Device, Queue};

/// Receive buffers always with the device (in one page).
const RX_BUFFERS: u16 = 8;
const RX_SIZE: usize = 512;
const EAGAIN: i64 = 11;

struct Session {
    master: End,
    pid: u64,
}

struct Console {
    _dev: Device,
    rx: Queue,
    tx: Queue,
    rx_buf: (u64, *mut u8),
    tx_buf: (u64, *mut u8),
    /// The send buffer is with the device.
    tx_busy: bool,
    session: Option<Session>,
    /// When a login was last started (ticks): at most one a second.
    started: u64,
}

unsafe impl Send for Console {}

static CONSOLE: Mutex<Option<Console>> = Mutex::new(None);

/// Find a virtio console and set it up. True if there's one.
pub fn init() -> bool {
    let Some(pci) = virtio::find(3).into_iter().next() else { return false };
    let Some((dev, _)) = Device::new(pci, 0) else { return false };
    dev.no_interrupts(); // (polled)
    let Some(mut rx) = dev.queue(0, RX_BUFFERS) else { return false };
    let Some(tx) = dev.queue(1, 2) else { return false };
    let (Some(rx_buf), Some(tx_buf)) = (virtio::dma_page(), virtio::dma_page()) else { return false };
    dev.start();
    for i in 0..rx.size().min(RX_BUFFERS) {
        rx.submit(i, rx_buf.0 + (RX_SIZE * i as usize) as u64, RX_SIZE as u32, true);
    }
    rx.kick();
    *CONSOLE.lock() = Some(Console { _dev: dev, rx, tx, rx_buf, tx_buf, tx_busy: false, session: None, started: 0 });
    true
}

/// Start a login on a fresh terminal.
fn start_login(c: &mut Console) {
    let now = crate::process::ticks();
    if c.started != 0 && now.saturating_sub(c.started) < 1_000_000_000 / crate::process::TICK_NS {
        return;
    }
    c.started = now.max(1);
    let Ok(master) = pty::open_master(true) else { return };
    pty::unlock_for_kernel(&master);
    let n = master.pty.index;
    // (dash: a POSIX shell, for exec and the redirections.)
    let Ok(sh) = crate::filesystem::read_bytes("/bin/dash") else { return };
    let script = alloc::format!("exec login </dev/pts/{n} >/dev/pts/{n} 2>&1");
    match crate::process::spawn_session(&sh, &["dash", "-c", &script], crate::process::DEFAULT_ENV) {
        Ok(pid) => c.session = Some(Session { master, pid }),
        Err(_) => {}
    }
}

impl Console {
    fn pump(&mut self) {
        // From the host: typed into the terminal.
        let mut any = false;
        while let Some((id, len)) = self.rx.take_used() {
            if id < RX_BUFFERS {
                let at = RX_SIZE * id as usize;
                let data = unsafe { core::slice::from_raw_parts(self.rx_buf.1.add(at), (len as usize).min(RX_SIZE)) };
                if let Some(s) = &self.session {
                    let _ = pty::master_write(&s.master, data); // (full: dropped, as a serial line would)
                }
                self.rx.submit(id, self.rx_buf.0 + at as u64, RX_SIZE as u32, true);
                any = true;
            }
        }
        if any {
            self.rx.kick();
        }
        // To the host: what the terminal displays (one buffer in flight).
        if self.tx_busy && self.tx.take_used().is_some() {
            self.tx_busy = false;
        }
        let mut ended = false;
        if let Some(s) = &self.session {
            if !self.tx_busy {
                let buf = unsafe { core::slice::from_raw_parts_mut(self.tx_buf.1, 4096) };
                match pty::master_read(&s.master, buf) {
                    n if n > 0 => {
                        self.tx.submit(0, self.tx_buf.0, n as u32, false);
                        self.tx.kick();
                        self.tx_busy = true;
                    }
                    n if n == -EAGAIN => {}
                    _ => ended = true, // EIO: everyone on the terminal is gone
                }
            }
            if !crate::process::alive(s.pid) && !self.tx_busy {
                ended = true;
            }
        }
        if ended {
            self.session = None; // (the master closes: the terminal hangs up)
        }
        if self.session.is_none() {
            start_login(self);
        }
    }
}

/// Move bytes both ways, and keep a login on the terminal. Not from an
/// interrupt of kernel code (see the module's comment).
pub fn pump() {
    if let Some(mut guard) = CONSOLE.try_lock() {
        if let Some(c) = guard.as_mut() {
            c.pump();
        }
    }
}

/// Whether there's a virtio console.
pub fn present() -> bool {
    CONSOLE.try_lock().is_some_and(|c| c.is_some())
}

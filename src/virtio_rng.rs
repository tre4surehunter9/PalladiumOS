// Copyright (c) 2026 tre4surehunter9
//! virtio-rng (QEMU's -device virtio-rng-pci): randomness from the host
//! (its /dev/urandom by default). One queue: hand the device a buffer,
//! and it fills (part of) it with random bytes and says how many.
//!
//! What it gives is mixed (XOR) into the kernel's own random numbers
//! (random.rs: RDRAND, or a generator seeded from the clock), for
//! getrandom(), /dev/random and /dev/urandom: never weaker than without it.

use spin::Mutex;

use crate::virtio::{self, Device, Queue};

/// How many random bytes one request asks for (the buffer: a page).
const BUFFER: usize = 4096;

struct Rng {
    _dev: Device,
    queue: Queue,
    buf: (u64, *mut u8),
}

unsafe impl Send for Rng {}

static RNG: Mutex<Option<Rng>> = Mutex::new(None);

/// Find a virtio-rng device and set it up. True if there's one.
pub fn init() -> bool {
    let Some(pci) = virtio::find(4).into_iter().next() else { return false };
    let Some((dev, _)) = Device::new(pci, 0) else { return false };
    dev.no_interrupts(); // (polled: each request is waited for)
    let Some(queue) = dev.queue(0, 4) else { return false };
    let Some(buf) = virtio::dma_page() else { return false };
    dev.start();
    *RNG.lock() = Some(Rng { _dev: dev, queue, buf });
    true
}

/// XOR random bytes from the device into `out` (as many as it gives in
/// time; none if there's no device, or it's busy on another core).
pub fn mix_into(out: &mut [u8]) {
    let Some(mut guard) = RNG.try_lock() else { return };
    let Some(rng) = guard.as_mut() else { return };
    let mut done = 0;
    while done < out.len() {
        let want = (out.len() - done).min(BUFFER);
        rng.queue.submit(0, rng.buf.0, want as u32, true);
        rng.queue.kick();
        let mut spins = 0u32;
        let got = loop {
            if let Some((_, len)) = rng.queue.take_used() {
                break len as usize;
            }
            spins += 1;
            if spins > 50_000_000 {
                return; // (no answer: the rest stays as it was)
            }
            core::hint::spin_loop();
        };
        if got == 0 {
            return;
        }
        let got = got.min(want);
        let bytes = unsafe { core::slice::from_raw_parts(rng.buf.1, got) };
        for (o, b) in out[done..done + got].iter_mut().zip(bytes) {
            *o ^= b;
        }
        done += got;
    }
}

/// Whether a virtio-rng device is in use.
pub fn present() -> bool {
    RNG.lock().is_some()
}

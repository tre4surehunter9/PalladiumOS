// Copyright (c) 2026 tre4surehunter9
//! Random numbers for getrandom() and AT_RANDOM (libc's stack-protector
//! canary and hash seeds). Uses the CPU's RDRAND when it has one, else a
//! xorshift generator seeded from the time-stamp counter. Not suitable for
//! cryptography without RDRAND or a virtio-rng device (whose bytes, from
//! the host, fill() mixes in).

use core::sync::atomic::{AtomicU64, Ordering};

static STATE: AtomicU64 = AtomicU64::new(0);

fn has_rdrand() -> bool {
    let r = core::arch::x86_64::__cpuid(1);
    r.ecx & (1 << 30) != 0
}

fn rdrand() -> Option<u64> {
    for _ in 0..10 {
        let mut v: u64;
        let ok: u8;
        unsafe { core::arch::asm!("rdrand {}", "setc {}", out(reg) v, out(reg_byte) ok) };
        if ok != 0 {
            return Some(v);
        }
    }
    None
}

pub fn next_u64() -> u64 {
    if has_rdrand() {
        if let Some(v) = rdrand() {
            return v;
        }
    }
    let tsc = unsafe { core::arch::x86_64::_rdtsc() };
    let mut x = STATE.load(Ordering::Relaxed) ^ tsc.rotate_left(17) ^ 0x9e37_79b9_7f4a_7c15;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    STATE.store(x, Ordering::Relaxed);
    x.wrapping_mul(0x2545_f491_4f6c_dd1d)
}

pub fn fill(buf: &mut [u8]) {
    for chunk in buf.chunks_mut(8) {
        let v = next_u64().to_le_bytes();
        chunk.copy_from_slice(&v[..chunk.len()]);
    }
    // And the host's randomness, with a virtio-rng device (mixed in).
    crate::virtio_rng::mix_into(buf);
}

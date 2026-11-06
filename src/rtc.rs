// Copyright (c) 2026 tre4surehunter9
//! The real-time clock (CMOS): the date and time, read once at boot. After
//! that, the time is kept by adding the timer ticks since boot.

use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::instructions::port::Port;

/// Unix time (seconds since 1970-01-01 UTC) when the kernel booted.
static BOOT_TIME: AtomicU64 = AtomicU64::new(0);

fn cmos(reg: u8) -> u8 {
    unsafe {
        Port::<u8>::new(0x70).write(reg | 0x80); // 0x80: keep NMIs disabled
        Port::<u8>::new(0x71).read()
    }
}

fn updating() -> bool {
    cmos(0x0A) & 0x80 != 0
}

/// Days from 1970-01-01 to the given date (proleptic Gregorian calendar).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp as i64 + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Read the CMOS clock (assumed to be UTC, which is what QEMU provides).
pub fn init() {
    // Read twice until two reads agree, so we don't catch it mid-update.
    let read = || {
        while updating() {}
        [cmos(0x00), cmos(0x02), cmos(0x04), cmos(0x07), cmos(0x08), cmos(0x09), cmos(0x32)]
    };
    let mut values = read();
    loop {
        let again = read();
        if again == values {
            break;
        }
        values = again;
    }
    let status_b = cmos(0x0B);
    let bcd = |v: u8| if status_b & 0x04 == 0 { (v & 0x0F) + (v >> 4) * 10 } else { v };
    let [sec, min, hour, day, month, year, century] = values;
    let (sec, min, day, month, year) = (bcd(sec), bcd(min), bcd(day), bcd(month), bcd(year));
    // 12-hour mode has the PM flag in bit 7.
    let pm = status_b & 0x02 == 0 && hour & 0x80 != 0;
    let mut hour = bcd(hour & 0x7F);
    if pm {
        hour = (hour % 12) + 12;
    }
    let century = if century != 0 { bcd(century) as i64 } else { 20 };
    let full_year = century * 100 + year as i64;
    let days = days_from_civil(full_year, month as u32, day as u32);
    let unix = days * 86400 + hour as i64 * 3600 + min as i64 * 60 + sec as i64;
    BOOT_TIME.store(unix.max(0) as u64, Ordering::Relaxed);
}

/// Current Unix time in nanoseconds.
pub fn now_ns() -> u64 {
    BOOT_TIME.load(Ordering::Relaxed) * 1_000_000_000 + crate::process::ticks() * crate::process::TICK_NS
}

/// Current Unix time in seconds.
pub fn now() -> u64 {
    now_ns() / 1_000_000_000
}

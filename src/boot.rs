// Copyright (c) 2026 tre4surehunter9
//! The boot log: what the kernel did while starting, shown like Linux.
//!
//! Facts about the machine are timestamped, like the kernel's own log:
//!
//! ```text
//! [    0.000] CPU: QEMU Virtual CPU version 2.5+
//! ```
//!
//! and each step reports how it went, like systemd:
//!
//! ```text
//! [  OK  ] Mounted the ext2 disk at /
//! [ WARN ] No system disk: /usr is empty
//! [FAILED] Disk found but not mounted: not an ext2 filesystem
//! ```
//!
//! Lines go to the screen and the serial port (only once `set_verbose` is
//! called: test kernels stay quiet), and are kept for `dmesg`
//! (/proc/kmsg). Nothing here needs the heap, so it works from the first
//! line of the kernel.

use core::fmt::{self, Write};
use spin::Mutex;

/// The kept log (the first 16 KiB of it).
const LOG_SIZE: usize = 16 * 1024;
static LOG: Mutex<([u8; LOG_SIZE], usize)> = Mutex::new(([0; LOG_SIZE], 0));
static VERBOSE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Print boot messages (the real kernel; test kernels don't).
pub fn set_verbose(on: bool) {
    VERBOSE.store(on, core::sync::atomic::Ordering::Relaxed);
}

/// A line being put together, on the stack.
struct Line {
    buf: [u8; 256],
    len: usize,
}

impl Write for Line {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let n = s.len().min(self.buf.len() - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        Ok(())
    }
}

/// How a step went.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Warn,
    Failed,
}

const GREEN: &str = "\x1b[1;32m";
const YELLOW: &str = "\x1b[1;33m";
const RED: &str = "\x1b[1;31m";
const RESET: &str = "\x1b[0m";

fn emit(line: &Line) {
    let bytes = &line.buf[..line.len];
    {
        let mut log = LOG.lock();
        let (buf, used) = &mut *log;
        // Full: the oldest lines go (as Linux's dmesg), so what happens
        // after a long time up is still there.
        if *used + bytes.len() > LOG_SIZE {
            let need = *used + bytes.len() - LOG_SIZE;
            let cut = buf[need..*used].iter().position(|&b| b == b'\n').map_or(*used, |p| need + p + 1);
            buf.copy_within(cut..*used, 0);
            *used -= cut;
        }
        let n = bytes.len().min(LOG_SIZE - *used);
        buf[*used..*used + n].copy_from_slice(&bytes[..n]);
        *used += n;
    }
    if VERBOSE.load(core::sync::atomic::Ordering::Relaxed) {
        x86_64::instructions::interrupts::without_interrupts(|| {
            let mut w = crate::vga_buffer::WRITER.lock();
            w.write_ansi(bytes);
            w.update_hw_cursor();
        });
        if let Ok(s) = core::str::from_utf8(bytes) {
            crate::serial_print!("{}", s);
        }
    }
}

/// Seconds since boot, as "    1.234" (timer ticks, so 10 ms steps).
fn timestamp(line: &mut Line) {
    let ms = crate::process::ticks() * crate::process::TICK_NS / 1_000_000;
    let _ = write!(line, "[{:>5}.{:03}] ", ms / 1000, ms % 1000);
}

/// A timestamped fact: `info!(...)` style, but a function.
pub fn info(args: fmt::Arguments) {
    let mut line = Line { buf: [0; 256], len: 0 };
    timestamp(&mut line);
    let _ = line.write_fmt(args);
    let _ = line.write_str("\n");
    emit(&line);
}

/// A step's result: "[  OK  ] Did the thing".
/// How many WARN or FAILED lines have been logged since boot (the log
/// itself may have dropped old ones).
pub fn warnings() -> u64 {
    WARNINGS.load(core::sync::atomic::Ordering::Relaxed)
}

static WARNINGS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

pub fn status(status: Status, args: fmt::Arguments) {
    if !matches!(status, Status::Ok) {
        WARNINGS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    }
    let mut line = Line { buf: [0; 256], len: 0 };
    let _ = match status {
        Status::Ok => write!(line, "[{}  OK  {}] ", GREEN, RESET),
        Status::Warn => write!(line, "[{} WARN {}] ", YELLOW, RESET),
        Status::Failed => write!(line, "[{}FAILED{}] ", RED, RESET),
    };
    let _ = line.write_fmt(args);
    let _ = line.write_str("\n");
    emit(&line);
}

#[macro_export]
macro_rules! boot_info {
    ($($arg:tt)*) => ($crate::boot::info(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! boot_ok {
    ($($arg:tt)*) => ($crate::boot::status($crate::boot::Status::Ok, format_args!($($arg)*)));
}

#[macro_export]
macro_rules! boot_warn {
    ($($arg:tt)*) => ($crate::boot::status($crate::boot::Status::Warn, format_args!($($arg)*)));
}

#[macro_export]
macro_rules! boot_failed {
    ($($arg:tt)*) => ($crate::boot::status($crate::boot::Status::Failed, format_args!($($arg)*)));
}

/// The log so far (for /proc/kmsg and dmesg).
pub fn log() -> alloc::string::String {
    let log = LOG.lock();
    alloc::string::String::from_utf8_lossy(&log.0[..log.1]).into_owned()
}

/// The CPU's name, from cpuid (e.g. "QEMU Virtual CPU version 2.5+").
pub fn cpu_name() -> alloc::string::String {
    use core::arch::x86_64::__cpuid;
    if __cpuid(0x8000_0000).eax < 0x8000_0004 {
        return "unknown x86_64 CPU".into();
    }
    let mut bytes = alloc::vec::Vec::with_capacity(48);
    for leaf in 0x8000_0002..=0x8000_0004u32 {
        let r = __cpuid(leaf);
        for reg in [r.eax, r.ebx, r.ecx, r.edx] {
            bytes.extend_from_slice(&reg.to_le_bytes());
        }
    }
    let name = alloc::string::String::from_utf8_lossy(&bytes).into_owned();
    name.trim_matches(|c: char| c == '\0' || c.is_whitespace()).into()
}

/// CPU features the kernel (or programs) care about, as "sse2 rdrand ...".
pub fn cpu_features() -> alloc::string::String {
    use core::arch::x86_64::__cpuid;
    let (l1, l7) = (__cpuid(1), __cpuid(7));
    let ext = __cpuid(0x8000_0001);
    let flags = [
        ("sse", l1.edx & (1 << 25) != 0),
        ("sse2", l1.edx & (1 << 26) != 0),
        ("sse3", l1.ecx & 1 != 0),
        ("sse4.2", l1.ecx & (1 << 20) != 0),
        ("avx", l1.ecx & (1 << 28) != 0),
        ("avx2", l7.ebx & (1 << 5) != 0),
        ("rdrand", l1.ecx & (1 << 30) != 0),
        ("nx", ext.edx & (1 << 20) != 0),
        ("syscall", ext.edx & (1 << 11) != 0),
    ];
    let on: alloc::vec::Vec<&str> = flags.iter().filter(|(_, b)| *b).map(|(n, _)| *n).collect();
    on.join(" ")
}

/// A Unix time as "2026-11-06 19:44:00 UTC".
pub fn format_date(t: u64) -> alloc::string::String {
    let (days, secs) = ((t / 86400) as i64, t % 86400);
    // days -> civil date (Howard Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + (month <= 2) as i64;
    alloc::format!("{}-{:02}-{:02} {:02}:{:02}:{:02} UTC", year, month, day, secs / 3600, secs / 60 % 60, secs % 60)
}

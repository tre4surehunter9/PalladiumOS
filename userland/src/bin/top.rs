// Copyright (c) 2026 tre4surehunter9
//! top: live view of processes, refreshed every second. Press q to quit.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use userlib::io::read_text;
use userlib::procinfo::{all, format_time, meminfo};
use userlib::*;

userlib::entry!(main);

fn main(_args: Args) -> i32 {
    let Ok(saved) = tcgetattr(STDIN) else {
        eprintln!("top: standard input is not a terminal");
        return 1;
    };
    // Raw mode, and reads wait at most 1 second (VMIN 0, VTIME 10): that's
    // both the refresh timer and the check for the q key.
    let mut raw = saved;
    raw.c_lflag &= !(ICANON | ECHO | ISIG);
    raw.c_cc[5] = 10; // VTIME, in tenths of a second
    raw.c_cc[6] = 0; // VMIN
    tcsetattr(STDIN, &raw);

    let (rows, cols) = window_size(STDOUT);
    let mut last: BTreeMap<u32, u64> = BTreeMap::new();
    let mut last_time = monotonic_time();
    loop {
        let procs = all();
        let now = monotonic_time();
        let elapsed = ((now.0 - last_time.0) * 100 + now.1 / 10_000_000).saturating_sub(last_time.1 / 10_000_000).max(1);
        last_time = now;
        // CPU% since the last refresh.
        let mut rows_data: Vec<(u64, &userlib::procinfo::Proc)> = procs
            .iter()
            .map(|p| (p.utime.saturating_sub(*last.get(&p.pid).unwrap_or(&p.utime)) * 100 / elapsed, p))
            .collect();
        rows_data.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.pid.cmp(&b.1.pid)));
        last = procs.iter().map(|p| (p.pid, p.utime)).collect();

        let uptime = read_text("/proc/uptime").unwrap_or_default();
        let up_secs: u64 = uptime.split('.').next().and_then(|s| s.parse().ok()).unwrap_or(0);
        let (total, free) = (meminfo("MemTotal"), meminfo("MemFree"));
        let running = procs.iter().filter(|p| p.state == 'R').count();

        let mut out = String::from("\x1b[H\x1b[2J");
        out.push_str(&format!(
            "top - up {}:{:02}:{:02}, {} processes, {} running    (press q to quit)\n",
            up_secs / 3600,
            up_secs / 60 % 60,
            up_secs % 60,
            procs.len(),
            running
        ));
        out.push_str(&format!("Mem: {} K total, {} K used, {} K free\n\n", total, total - free, free));
        out.push_str("\x1b[7m  PID  PPID S  %CPU     TIME    RSS COMMAND                              \x1b[0m\n");
        for (cpu, p) in rows_data.iter().take((rows as usize).saturating_sub(5)) {
            let mut line = format!("{:5} {:5} {} {:5} {:>8} {:5}K {}", p.pid, p.ppid, p.state, cpu, format_time(p.utime), p.rss_kb, p.cmdline);
            line.truncate(cols as usize - 1);
            out.push_str(&line);
            out.push('\n');
        }
        let _ = write_all(STDOUT, out.as_bytes());

        let mut key = [0u8; 8];
        let n = read(STDIN, &mut key);
        if n > 0 && key[..n as usize].iter().any(|&k| k == b'q' || k == 3) {
            break; // q or Ctrl-C
        }
    }
    tcsetattr(STDIN, &saved);
    let _ = write_all(STDOUT, b"\x1b[H\x1b[2J");
    0
}

// Copyright (c) 2026 tre4surehunter9
//! Reading process information from /proc (for ps, top, pgrep, ...).

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::io::read_text;
use crate::{close, for_each_entry, open, O_DIRECTORY, O_RDONLY};

/// Clock ticks per second in /proc/PID/stat times (like Linux's USER_HZ).
pub const HZ: u64 = 100;

#[derive(Clone, Debug)]
pub struct Proc {
    pub pid: u32,
    pub ppid: u32,
    pub pgid: u32,
    pub name: String,
    /// R running, S sleeping, Z zombie.
    pub state: char,
    /// CPU time in 1/100 s.
    pub utime: u64,
    /// When it started, in 1/100 s since boot.
    pub start: u64,
    /// Memory in use (resident pages * 4 KiB).
    pub rss_kb: u64,
    /// The command line (arguments joined with spaces).
    pub cmdline: String,
}

/// Parse /proc/PID/stat: "pid (name) S ppid pgrp ..." (the name may contain
/// spaces or parentheses, so it's everything up to the last ')').
pub fn parse_stat(text: &str) -> Option<Proc> {
    let open_paren = text.find('(')?;
    let close_paren = text.rfind(')')?;
    let pid = text[..open_paren].trim().parse().ok()?;
    let name = text[open_paren + 1..close_paren].to_string();
    let f: Vec<&str> = text[close_paren + 1..].split_whitespace().collect();
    let num = |i: usize| f.get(i).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
    Some(Proc {
        pid,
        ppid: num(1) as u32,
        pgid: num(2) as u32,
        name,
        state: f.first().and_then(|s| s.chars().next()).unwrap_or('?'),
        utime: num(11),
        start: num(19),
        rss_kb: num(21) * 4,
        cmdline: String::new(),
    })
}

/// One process, from /proc/PID.
pub fn read_proc(pid: u32) -> Option<Proc> {
    let mut p = parse_stat(&read_text(&alloc::format!("/proc/{}/stat", pid))?)?;
    let cmd = read_text(&alloc::format!("/proc/{}/cmdline", pid)).unwrap_or_default();
    p.cmdline = cmd.trim_end_matches('\0').replace('\0', " ");
    if p.cmdline.is_empty() {
        p.cmdline = alloc::format!("[{}]", p.name);
    }
    Some(p)
}

/// The pids in /proc, sorted.
pub fn pids() -> Vec<u32> {
    let mut pids = Vec::new();
    let fd = open("/proc", O_RDONLY | O_DIRECTORY);
    if fd < 0 {
        return pids;
    }
    let _ = for_each_entry(fd as i32, |e| {
        if let Ok(pid) = e.name.parse() {
            pids.push(pid);
        }
    });
    close(fd as i32);
    pids.sort();
    pids
}

/// Every process (that still exists by the time it's read).
pub fn all() -> Vec<Proc> {
    pids().into_iter().filter_map(read_proc).collect()
}

/// A value like "MemTotal:  131072 kB" from /proc/meminfo, in kB.
pub fn meminfo(key: &str) -> u64 {
    let text = read_text("/proc/meminfo").unwrap_or_default();
    text.lines()
        .find_map(|l| l.strip_prefix(key)?.strip_prefix(':')?.trim().trim_end_matches("kB").trim().parse().ok())
        .unwrap_or(0)
}

/// "M:SS" for a time in 1/100 s.
pub fn format_time(hundredths: u64) -> String {
    let secs = hundredths / HZ;
    alloc::format!("{}:{:02}", secs / 60, secs % 60)
}

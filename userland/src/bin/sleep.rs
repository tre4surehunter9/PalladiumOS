// Copyright (c) 2026 tre4surehunter9
//! sleep: wait for a number of seconds (decimals allowed: sleep 0.5).
#![no_std]
#![no_main]

use userlib::{eprintln, sleep_ms};

userlib::entry!(main);

/// Parse "1", "2.5", ".25" into milliseconds.
fn parse_ms(s: &str) -> Option<u64> {
    let (whole, frac) = s.split_once('.').unwrap_or((s, ""));
    if whole.is_empty() && frac.is_empty() {
        return None;
    }
    let whole: u64 = if whole.is_empty() { 0 } else { whole.parse().ok()? };
    let mut ms = 0;
    for (i, c) in frac.chars().take(3).enumerate() {
        ms += c.to_digit(10)? as u64 * [100, 10, 1][i];
    }
    if !frac.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(whole * 1000 + ms)
}

fn main(args: userlib::Args) -> i32 {
    let Some(arg) = args.get(1) else {
        eprintln!("usage: sleep SECONDS");
        return 1;
    };
    match parse_ms(arg) {
        Some(ms) => {
            if sleep_ms(ms) < 0 { 1 } else { 0 }
        }
        None => {
            eprintln!("sleep: invalid time interval '{}'", arg);
            1
        }
    }
}

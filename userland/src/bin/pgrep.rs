// Copyright (c) 2026 tre4surehunter9
//! pgrep / pkill: find processes by name, and signal them.
//!   pgrep [-l] [-x] NAME      print pids (-l: with names, -x: exact name)
//!   pkill [-SIGNAL] [-x] NAME send a signal (default SIGTERM)
//! (One program: it acts as pkill when run as "pkill".)
#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use userlib::procinfo::all;
use userlib::*;

userlib::entry!(main);

fn main(args: Args) -> i32 {
    let killing = args.get(0).is_some_and(|a| a.ends_with("pkill"));
    let (mut long, mut exact, mut sig) = (false, false, SIGTERM);
    let mut pattern = None;
    for a in args.skip(1) {
        match a {
            "-l" => long = true,
            "-x" => exact = true,
            s if s.starts_with('-') && killing => {
                let name = s[1..].strip_prefix("SIG").unwrap_or(&s[1..]);
                match s[1..].parse::<i32>().ok().or_else(|| SIGNAL_NAMES.iter().find(|(_, n)| n.eq_ignore_ascii_case(name)).map(|(n, _)| *n)) {
                    Some(n) => sig = n,
                    None => {
                        eprintln!("pkill: bad signal '{}'", s);
                        return 2;
                    }
                }
            }
            s => pattern = Some(s),
        }
    }
    let Some(pattern) = pattern else {
        eprintln!("usage: {} [options] NAME", if killing { "pkill [-SIGNAL]" } else { "pgrep [-l] [-x]" });
        return 2;
    };
    let me = getpid() as u32;
    let found: Vec<_> = all()
        .into_iter()
        .filter(|p| p.pid != me && p.state != 'Z')
        .filter(|p| if exact { p.name == pattern } else { p.name.contains(pattern) })
        .collect();
    for p in &found {
        if killing {
            let r = kill(p.pid as i32, sig);
            if r < 0 {
                eprintln!("pkill: {} ({}): {}", p.pid, p.name, strerror(-r));
            }
        } else if long {
            println!("{} {}", p.pid, p.name);
        } else {
            println!("{}", p.pid);
        }
    }
    if found.is_empty() { 1 } else { 0 }
}

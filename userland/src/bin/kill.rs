// Copyright (c) 2026 tre4surehunter9
//! kill: send a signal to processes.
//!   kill PID...           send SIGTERM
//!   kill -9 PID / kill -KILL PID / kill -s INT PID
//!   kill -l               list signal names
//! A negative PID means a process group.
#![no_std]
#![no_main]

use userlib::*;

userlib::entry!(main);

fn parse_signal(s: &str) -> Option<i32> {
    if let Ok(n) = s.parse::<i32>() {
        return (0..=64).contains(&n).then_some(n);
    }
    let name = s.strip_prefix("SIG").unwrap_or(s);
    SIGNAL_NAMES.iter().find(|(_, n)| n.eq_ignore_ascii_case(name)).map(|(n, _)| *n)
}

fn main(args: Args) -> i32 {
    let mut sig = SIGTERM;
    let mut rest = args.skip(1).peekable();
    match rest.peek().copied() {
        Some("-l") => {
            for (n, name) in SIGNAL_NAMES {
                println!("{:2}) SIG{}", n, name);
            }
            return 0;
        }
        Some("-s") => {
            rest.next();
            match rest.next().and_then(parse_signal) {
                Some(s) => sig = s,
                None => {
                    eprintln!("kill: bad signal name");
                    return 1;
                }
            }
        }
        Some("--") => {
            rest.next(); // end of options: `kill -- -5` signals process group 5
        }
        Some(a) if a.starts_with('-') && a.len() > 1 => {
            if let Some(s) = parse_signal(&a[1..]) {
                sig = s;
                rest.next();
            }
        }
        _ => {}
    }
    let mut status = 0;
    let mut any = false;
    for arg in rest {
        any = true;
        let Ok(pid) = arg.parse::<i32>() else {
            eprintln!("kill: {}: arguments must be process IDs", arg);
            status = 1;
            continue;
        };
        let r = kill(pid, sig);
        if r < 0 {
            eprintln!("kill: ({}) - {}", pid, strerror(-r));
            status = 1;
        }
    }
    if !any {
        eprintln!("usage: kill [-SIGNAL] PID...   (kill -l lists signals)");
        return 2;
    }
    status
}

// Copyright (c) 2026 tre4surehunter9
//! chmod: change permission bits.
//!   chmod 755 FILE...       octal
//!   chmod u+x,go-w FILE...  symbolic: [ugoa][+-=][rwx]
#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use userlib::*;

userlib::entry!(main);

/// Apply a mode like "755" or "u+x,go-w" to `old`.
fn apply(spec: &str, old: u32) -> Option<u32> {
    if spec.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
        return u32::from_str_radix(spec, 8).ok().filter(|m| *m <= 0o7777);
    }
    let mut mode = old & 0o7777;
    for clause in spec.split(',') {
        let op_at = clause.find(['+', '-', '='])?;
        let (who, rest) = clause.split_at(op_at);
        let op = rest.as_bytes()[0];
        let mut who_mask = 0;
        for c in who.chars() {
            who_mask |= match c {
                'u' => 0o700,
                'g' => 0o070,
                'o' => 0o007,
                'a' => 0o777,
                _ => return None,
            };
        }
        if who_mask == 0 {
            who_mask = 0o777;
        }
        let mut bits = 0;
        for c in rest[1..].chars() {
            bits |= match c {
                'r' => 0o444,
                'w' => 0o222,
                'x' => 0o111,
                _ => return None,
            };
        }
        let bits = bits & who_mask;
        mode = match op {
            b'+' => mode | bits,
            b'-' => mode & !bits,
            _ => (mode & !who_mask) | bits,
        };
    }
    Some(mode)
}

fn main(args: Args) -> i32 {
    let args: Vec<&str> = args.skip(1).collect();
    if args.len() < 2 {
        eprintln!("usage: chmod MODE FILE...");
        return 1;
    }
    let mut status = 0;
    for path in &args[1..] {
        let old = match stat(path) {
            Ok(st) => st.st_mode,
            Err(e) => {
                eprintln!("chmod: {}: {}", path, strerror(-e));
                status = 1;
                continue;
            }
        };
        let Some(mode) = apply(args[0], old) else {
            eprintln!("chmod: invalid mode: '{}'", args[0]);
            return 1;
        };
        let r = userlib::chmod(path, mode);
        if r < 0 {
            eprintln!("chmod: {}: {}", path, strerror(-r));
            status = 1;
        }
    }
    status
}

// Copyright (c) 2026 tre4surehunter9
//! uname: print system information (supports -a, -s, -n, -r, -v, -m).
#![no_std]
#![no_main]

use userlib::{eprintln, field_str, print, println};

userlib::entry!(main);

fn main(args: userlib::Args) -> i32 {
    let u = match userlib::uname() {
        Ok(u) => u,
        Err(e) => {
            eprintln!("uname: failed ({})", e);
            return 1;
        }
    };
    let fields = [
        ('s', &u.sysname),
        ('n', &u.nodename),
        ('r', &u.release),
        ('v', &u.version),
        ('m', &u.machine),
    ];

    let mut wanted = [false; 5];
    for arg in args.skip(1) {
        let Some(flags) = arg.strip_prefix('-') else {
            eprintln!("uname: extra operand '{}'", arg);
            return 1;
        };
        for c in flags.chars() {
            match c {
                'a' => wanted = [true; 5],
                _ => match fields.iter().position(|(f, _)| *f == c) {
                    Some(i) => wanted[i] = true,
                    None => {
                        eprintln!("uname: invalid option -- '{}'", c);
                        return 1;
                    }
                },
            }
        }
    }
    if !wanted.contains(&true) {
        wanted[0] = true; // default: like `uname -s`
    }

    let mut first = true;
    for (i, (_, value)) in fields.iter().enumerate() {
        if wanted[i] {
            if !first {
                print!(" ");
            }
            print!("{}", field_str(*value));
            first = false;
        }
    }
    println!();
    0
}

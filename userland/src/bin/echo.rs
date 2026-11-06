// Copyright (c) 2026 tre4surehunter9
//! echo: print the arguments separated by spaces (POSIX echo, with -n).
#![no_std]
#![no_main]

use userlib::print;

userlib::entry!(main);

fn main(args: userlib::Args) -> i32 {
    let mut args = args.skip(1).peekable();
    let newline = if args.peek() == Some(&"-n") {
        args.next();
        false
    } else {
        true
    };
    let mut first = true;
    for arg in args {
        if !first {
            print!(" ");
        }
        print!("{}", arg);
        first = false;
    }
    if newline {
        print!("\n");
    }
    0
}

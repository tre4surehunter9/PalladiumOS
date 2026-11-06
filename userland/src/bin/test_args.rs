// Copyright (c) 2026 tre4surehunter9
//! Test program: exit(0) only if argv is exactly ["test_args", "one", "two words"].
#![no_std]
#![no_main]

userlib::entry!(main);

fn main(args: userlib::Args) -> i32 {
    let expected = ["test_args", "one", "two words"];
    if args.count() != expected.len() {
        return 10 + args.count() as i32;
    }
    for (i, (got, want)) in args.zip(expected.iter()).enumerate() {
        if got != *want {
            return 20 + i as i32;
        }
    }
    0
}

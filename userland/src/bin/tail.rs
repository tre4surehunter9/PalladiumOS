// Copyright (c) 2026 tre4surehunter9
//! tail: print the last lines of files.  tail [-n N | -N] [FILE...]
#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::VecDeque;
use alloc::format;
use alloc::vec::Vec;
use userlib::io::{count_option, for_each_input, Lines, Out};
use userlib::{eprintln, Args};

userlib::entry!(main);

fn main(args: Args) -> i32 {
    let args: Vec<&str> = args.skip(1).collect();
    let (count, files) = match count_option(&args, 10) {
        Ok(v) => v,
        Err(bad) => {
            eprintln!("tail: invalid number of lines: '{}'", bad);
            return 1;
        }
    };
    let mut out = Out::new();
    let headers = files.len() > 1;
    let mut first = true;
    for_each_input("tail", &files, |fd, name| {
        if headers {
            out.write(format!("{}==> {} <==\n", if first { "" } else { "\n" }, name).as_bytes());
        }
        first = false;
        let mut last: VecDeque<Vec<u8>> = VecDeque::new();
        let mut lines = Lines::new(fd);
        while let Some(Ok(line)) = lines.next_line() {
            if count == 0 {
                continue;
            }
            if last.len() == count {
                last.pop_front();
            }
            last.push_back(line);
        }
        for line in &last {
            out.write(line);
        }
    })
}

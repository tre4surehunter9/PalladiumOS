// Copyright (c) 2026 tre4surehunter9
//! Test program: recurse forever. Must hit the stack guard page and be
//! killed with a segfault (status 139), without taking the kernel down.
#![no_std]
#![no_main]

userlib::entry!(main);

#[inline(never)]
#[allow(unconditional_recursion)] // (on purpose: it overflows the stack)
fn recurse(n: u64) -> u64 {
    let buf = [n; 64];
    core::hint::black_box(&buf);
    recurse(n + 1) + buf[0]
}

fn main(_args: userlib::Args) -> i32 {
    recurse(0) as i32
}

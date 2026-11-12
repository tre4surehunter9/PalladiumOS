// Copyright (c) 2026 tre4surehunter9
//! volume: the sound card's volume (through /dev/mixer).
//!
//!   volume            show the master and PCM volumes
//!   volume N          set the master volume (0-100; 0 mutes)
//!   volume pcm N      set the PCM (sound from programs) volume
#![no_std]
#![no_main]

use userlib::sys::{syscall3, IOCTL};
use userlib::*;

userlib::entry!(main);

const READ_VOLUME: u64 = 0x8004_4D00;
const READ_PCM: u64 = 0x8004_4D04;
const WRITE_VOLUME: u64 = 0xC004_4D00;
const WRITE_PCM: u64 = 0xC004_4D04;

fn ioctl_int(fd: i32, request: u64, value: i32) -> Result<i32, isize> {
    let mut v = value;
    let r = unsafe { syscall3(IOCTL, fd as u64, request, &mut v as *mut i32 as u64) };
    if r < 0 { Err(r as isize) } else { Ok(v) }
}

fn show(name: &str, v: i32) {
    let (l, r) = (v & 0xff, (v >> 8) & 0xff);
    if l == r {
        println!("{}: {}%", name, l);
    } else {
        println!("{}: {}% left, {}% right", name, l, r);
    }
}

fn main(args: Args) -> i32 {
    let fd = open("/dev/mixer", O_RDWR);
    if fd < 0 {
        eprintln!("volume: no sound card ({})", strerror(-fd));
        return 1;
    }
    let fd = fd as i32;
    let (request, value) = match (args.get(1), args.get(2)) {
        (None, _) => {
            show("master", ioctl_int(fd, READ_VOLUME, 0).unwrap_or(0));
            show("pcm", ioctl_int(fd, READ_PCM, 0).unwrap_or(0));
            return 0;
        }
        (Some("pcm"), Some(n)) => (WRITE_PCM, n),
        (Some(n), None) => (WRITE_VOLUME, n),
        _ => {
            eprintln!("usage: volume [N]  or  volume pcm N   (N: 0-100)");
            return 1;
        }
    };
    let Some(n) = value.trim_end_matches('%').parse::<i32>().ok().filter(|n| (0..=100).contains(n)) else {
        eprintln!("volume: {}: give a number from 0 to 100", value);
        return 1;
    };
    match ioctl_int(fd, request, n | n << 8) {
        Ok(v) => {
            show(if request == WRITE_PCM { "pcm" } else { "master" }, v);
            0
        }
        Err(e) => {
            eprintln!("volume: {}", strerror(-e));
            1
        }
    }
}

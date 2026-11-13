// Copyright (c) 2026 tre4surehunter9
//! QEMU's firmware configuration device (fw_cfg): named files the machine's
//! command line gives the guest (-fw_cfg name=opt/...,string=...). The
//! runner tells the kernel things this way that it can't find out reliably
//! otherwise (the screen size it asked for: with QEMU's window, the virtio
//! GPU reports the window's size, whatever the window manager made it).
//!
//! I/O ports: 0x510 selects an item (16 bits), 0x511 reads its bytes in
//! order. Item 0 is the signature "QEMU"; item 0x19 the file directory: a
//! count, then per file its size, item number and name (big-endian).

use alloc::string::String;
use alloc::vec::Vec;
use x86_64::instructions::port::Port;

const SELECTOR: u16 = 0x510;
const DATA: u16 = 0x511;
const SIGNATURE: u16 = 0x0000;
const FILE_DIR: u16 = 0x0019;

fn select(item: u16) {
    unsafe { Port::<u16>::new(SELECTOR).write(item) };
}

fn read(out: &mut [u8]) {
    let mut port = Port::<u8>::new(DATA);
    for b in out {
        *b = unsafe { port.read() };
    }
}

/// The contents of the fw_cfg file `name`, if this is QEMU and it has one.
pub fn file(name: &str) -> Option<Vec<u8>> {
    select(SIGNATURE);
    let mut sig = [0u8; 4];
    read(&mut sig);
    if &sig != b"QEMU" {
        return None; // (real hardware: nothing there, reads give 0xff)
    }
    select(FILE_DIR);
    let mut count = [0u8; 4];
    read(&mut count);
    let mut found = None;
    for _ in 0..u32::from_be_bytes(count).min(4096) {
        let mut entry = [0u8; 64];
        read(&mut entry);
        let end = entry[8..].iter().position(|&c| c == 0).unwrap_or(56);
        if &entry[8..8 + end] == name.as_bytes() && found.is_none() {
            let size = u32::from_be_bytes(entry[0..4].try_into().unwrap()) as usize;
            found = Some((u16::from_be_bytes([entry[4], entry[5]]), size.min(4096)));
        }
    }
    let (item, size) = found?;
    select(item);
    let mut data = alloc::vec![0u8; size];
    read(&mut data);
    Some(data)
}

/// The screen size the runner asked for (opt/palladium/resolution:
/// "WIDTHxHEIGHT").
pub fn resolution() -> Option<(u32, u32)> {
    let text = String::from_utf8(file("opt/palladium/resolution")?).ok()?;
    let (w, h) = text.trim().split_once('x')?;
    let (w, h) = (w.parse().ok()?, h.parse().ok()?);
    ((640..=7680).contains(&w) && (480..=4320).contains(&h)).then_some((w, h))
}

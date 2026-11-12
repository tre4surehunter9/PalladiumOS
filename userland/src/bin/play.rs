// Copyright (c) 2026 tre4surehunter9
//! play: play sound through /dev/dsp.
//!
//!   play FILE.wav...        WAV files (PCM: 8- or 16-bit, mono or stereo,
//!                           8000 to 48000 Hz)
//!   play -t [HZ [SECONDS]]  a tone (440 Hz, 1 second by default)
#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;
use userlib::sys::{syscall3, IOCTL};
use userlib::*;

userlib::entry!(main);

const SNDCTL_DSP_SYNC: u64 = 0x5001;
const SNDCTL_DSP_SPEED: u64 = 0xC004_5002;
const SNDCTL_DSP_SETFMT: u64 = 0xC004_5005;
const SNDCTL_DSP_CHANNELS: u64 = 0xC004_5006;
const AFMT_U8: i32 = 0x08;
const AFMT_S16_LE: i32 = 0x10;

fn ioctl_int(fd: i32, request: u64, value: i32) -> Result<i32, isize> {
    let mut v = value;
    let r = unsafe { syscall3(IOCTL, fd as u64, request, &mut v as *mut i32 as u64) };
    if r < 0 { Err(r as isize) } else { Ok(v) }
}

/// Open /dev/dsp with this format.
fn open_dsp(rate: u32, channels: u16, bits: u16) -> Result<i32, &'static str> {
    let fd = open("/dev/dsp", O_WRONLY);
    if fd < 0 {
        return Err(match -fd {
            19 | 2 => "no sound card (QEMU: -device AC97)",
            16 => "/dev/dsp is busy (another program is playing)",
            _ => strerror(-fd),
        });
    }
    let fd = fd as i32;
    let fmt = if bits == 16 { AFMT_S16_LE } else { AFMT_U8 };
    let ok = ioctl_int(fd, SNDCTL_DSP_SETFMT, fmt).is_ok_and(|f| f == fmt)
        && ioctl_int(fd, SNDCTL_DSP_CHANNELS, channels as i32).is_ok_and(|c| c == channels as i32)
        && ioctl_int(fd, SNDCTL_DSP_SPEED, rate as i32).is_ok();
    if !ok {
        close(fd);
        return Err("the sound card doesn't take this format");
    }
    Ok(fd)
}

fn write_all(fd: i32, mut data: &[u8]) -> bool {
    while !data.is_empty() {
        let n = write(fd, data);
        if n <= 0 {
            return false;
        }
        data = &data[n as usize..];
    }
    true
}

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

fn read_file(path: &str) -> Result<Vec<u8>, isize> {
    let fd = open(path, O_RDONLY);
    if fd < 0 {
        return Err(fd);
    }
    let mut data = Vec::new();
    let mut buf = vec![0u8; 65536];
    loop {
        let n = read(fd as i32, &mut buf);
        if n < 0 {
            close(fd as i32);
            return Err(n);
        }
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n as usize]);
    }
    close(fd as i32);
    Ok(data)
}

fn play_wav(path: &str) -> Result<(), alloc::string::String> {
    let data = read_file(path).map_err(|e| alloc::format!("{}: {}", path, strerror(-e)))?;
    if data.len() < 12 || &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        return Err(alloc::format!("{}: not a WAV file", path));
    }
    // The chunks: "fmt " (the format), then "data" (the samples).
    let mut fmt = None;
    let mut i = 12;
    while i + 8 <= data.len() {
        let id = &data[i..i + 4];
        let len = u32_at(&data, i + 4) as usize;
        let body = i + 8;
        let end = (body + len).min(data.len());
        if id == b"fmt " && len >= 16 {
            fmt = Some((u16_at(&data, body), u16_at(&data, body + 2), u32_at(&data, body + 4), u16_at(&data, body + 14)));
        } else if id == b"data" {
            let Some((format, channels, rate, bits)) = fmt else {
                return Err(alloc::format!("{}: no format before the samples", path));
            };
            if format != 1 || !(bits == 8 || bits == 16) || !(channels == 1 || channels == 2) {
                return Err(alloc::format!(
                    "{}: only uncompressed 8- or 16-bit mono or stereo (this is format {}, {} bits, {} channels)",
                    path, format, bits, channels
                ));
            }
            let fd = open_dsp(rate, channels, bits).map_err(|e| alloc::format!("play: {}", e))?;
            println!("{}: {} Hz, {}-bit, {}", path, rate, bits, if channels == 2 { "stereo" } else { "mono" });
            let ok = write_all(fd, &data[body..end]);
            unsafe { syscall3(IOCTL, fd as u64, SNDCTL_DSP_SYNC, 0) };
            close(fd);
            return if ok { Ok(()) } else { Err(alloc::format!("{}: writing to /dev/dsp failed", path)) };
        }
        i = body + len + (len & 1); // chunks are padded to even lengths
    }
    Err(alloc::format!("{}: no samples", path))
}

/// sin(2 pi x) for x in [0, 1), without a maths library (Bhaskara's
/// approximation: within 0.2%).
fn sine(x: f32) -> f32 {
    let (x, sign) = if x < 0.5 { (x, 1.0) } else { (x - 0.5, -1.0) };
    let p = x * 2.0; // [0, 1): fraction of pi
    let num = 16.0 * p * (1.0 - p);
    sign * num / (5.0 - 4.0 * p * (1.0 - p))
}

fn play_tone(hz: u32, ms: u32) -> Result<(), alloc::string::String> {
    let rate = 22050;
    let fd = open_dsp(rate, 1, 16).map_err(|e| alloc::format!("play: {}", e))?;
    let frames = (rate as u64 * ms as u64 / 1000) as usize;
    let mut buf = Vec::with_capacity(frames * 2);
    let fade = (rate as usize / 100).min(frames / 2); // 10 ms in and out: no clicks
    for i in 0..frames {
        let x = ((i as u64 * hz as u64) % rate as u64) as f32 / rate as f32;
        let mut v = sine(x) * 12000.0;
        let edge = i.min(frames - 1 - i);
        if edge < fade {
            v *= edge as f32 / fade as f32;
        }
        buf.extend_from_slice(&(v as i16).to_le_bytes());
    }
    let ok = write_all(fd, &buf);
    unsafe { syscall3(IOCTL, fd as u64, SNDCTL_DSP_SYNC, 0) };
    close(fd);
    if ok { Ok(()) } else { Err("play: writing to /dev/dsp failed".into()) }
}

fn main(args: Args) -> i32 {
    let words: Vec<&str> = args.skip(1).collect();
    if words.is_empty() || words[0] == "-h" || words[0] == "--help" {
        eprintln!("usage: play FILE.wav...    or    play -t [HZ [SECONDS]]");
        return 1;
    }
    if words[0] == "-t" {
        let hz = words.get(1).map_or(Some(440), |w| w.parse().ok()).filter(|&h| (20..=20000).contains(&h));
        let ms = match words.get(2) {
            None => Some(1000),
            Some(w) => {
                let (s, frac) = w.split_once('.').unwrap_or((w, ""));
                let frac_ms: u32 = alloc::format!("{:0<3}", &frac[..frac.len().min(3)]).parse().unwrap_or(0);
                s.parse::<u32>().ok().map(|s| s * 1000 + frac_ms).filter(|&m| m > 0 && m <= 60_000)
            }
        };
        let (Some(hz), Some(ms)) = (hz, ms) else {
            eprintln!("play: -t takes a frequency (20-20000 Hz) and seconds (up to 60)");
            return 1;
        };
        return match play_tone(hz, ms) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("{}", e);
                1
            }
        };
    }
    let mut status = 0;
    for path in words {
        if let Err(e) = play_wav(path) {
            eprintln!("{}", e);
            status = 1;
        }
    }
    status
}

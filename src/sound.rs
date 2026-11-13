// Copyright (c) 2026 tre4surehunter9
//! Sound: a driver for the AC'97 audio controller (Intel's ICH audio, QEMU's
//! `-device AC97`) and the OSS interface programs use: /dev/dsp (write
//! samples) and /dev/mixer (volumes).
//!
//! The controller plays from a ring of 32 buffers by DMA, each described in
//! a buffer descriptor list (address, length). The driver fills buffer
//! `tail`, hands it over by moving the "last valid index" to it, and the
//! controller plays up to there and stops (and goes on when given more).
//! The card always gets 16-bit stereo; programs may write 8- or 16-bit, mono
//! or stereo, at any rate from 8000 to 48000 Hz: the codec's variable rate
//! plays it at that rate (or, on a codec without it, the driver resamples
//! to 48000 Hz). A write that finds the ring full waits for the card.
//!
//! Not supported: recording, more than one program at once (/dev/dsp is
//! busy while open), mmap of the buffer, Intel HD Audio (the later
//! controller real PCs have).

use alloc::sync::Arc;
use spin::Mutex;
use x86_64::instructions::port::Port;

use crate::memory;
use crate::syscall::{EAGAIN, EBUSY, EFAULT, EINTR, EINVAL, ENODEV, ENOTTY};

const BUFS: usize = 32;
/// Bytes in one DMA buffer (one page): 1024 frames of 16-bit stereo.
const BUF_BYTES: usize = 4096;

// The mixer (native audio mixer, BAR 0)
const NAM_RESET: u16 = 0x00;
const NAM_MASTER: u16 = 0x02;
const NAM_HEADPHONE: u16 = 0x04;
const NAM_PCM_OUT: u16 = 0x18;
const NAM_EXT_ID: u16 = 0x28;
const NAM_EXT_CTRL: u16 = 0x2A;
const NAM_FRONT_RATE: u16 = 0x2C;
// The bus master (native audio bus master, BAR 1): PCM out's box
const PO_BDBAR: u16 = 0x10;
const PO_CIV: u16 = 0x14;
const PO_LVI: u16 = 0x15;
const PO_SR: u16 = 0x16;
const PO_PICB: u16 = 0x18;
const PO_CR: u16 = 0x1B;
const GLOB_CNT: u16 = 0x2C;
const GLOB_STA: u16 = 0x30;

const CR_RUN: u8 = 0x01;
const CR_RESET: u8 = 0x02;
const SR_HALTED: u16 = 0x01;
/// Buffer descriptor flag: when the buffers run out, play silence.
const BD_BUP: u16 = 1 << 14;

// OSS formats
pub const AFMT_U8: u32 = 0x08;
pub const AFMT_S16_LE: u32 = 0x10;
pub const AFMT_S8: u32 = 0x40;
/// 32-bit samples (played as their top 16 bits: the card's is 16-bit).
pub const AFMT_S32_LE: u32 = 0x1000;

struct Ac97 {
    nam: u16,
    nabm: u16,
    bdl_phys: u64,
    buf_phys: [u64; BUFS],
    /// The codec plays any rate (variable rate audio); else always 48 kHz.
    vra: bool,
}

impl Ac97 {
    fn nam_read(&self, r: u16) -> u16 {
        unsafe { Port::<u16>::new(self.nam + r).read() }
    }
    fn nam_write(&self, r: u16, v: u16) {
        unsafe { Port::<u16>::new(self.nam + r).write(v) }
    }
    fn in8(&self, r: u16) -> u8 {
        unsafe { Port::<u8>::new(self.nabm + r).read() }
    }
    fn out8(&self, r: u16, v: u8) {
        unsafe { Port::<u8>::new(self.nabm + r).write(v) }
    }
    fn in16(&self, r: u16) -> u16 {
        unsafe { Port::<u16>::new(self.nabm + r).read() }
    }
    fn out16(&self, r: u16, v: u16) {
        unsafe { Port::<u16>::new(self.nabm + r).write(v) }
    }
    fn in32(&self, r: u16) -> u32 {
        unsafe { Port::<u32>::new(self.nabm + r).read() }
    }
    fn out32(&self, r: u16, v: u32) {
        unsafe { Port::<u32>::new(self.nabm + r).write(v) }
    }

    fn buf(&self, i: usize) -> *mut u8 {
        memory::phys_ptr(x86_64::PhysAddr::new(self.buf_phys[i]))
    }

    /// Buffer descriptor `i`: its length in samples (16-bit units).
    fn set_bd(&self, i: usize, samples: u16) {
        let bd = memory::phys_ptr(x86_64::PhysAddr::new(self.bdl_phys + 8 * i as u64));
        unsafe {
            core::ptr::write_volatile(bd as *mut u32, self.buf_phys[i] as u32);
            core::ptr::write_volatile(bd.add(4) as *mut u16, samples);
            core::ptr::write_volatile(bd.add(6) as *mut u16, BD_BUP);
        }
    }

    /// Stop PCM out and reset its registers (the ring starts at 0 again).
    fn reset_output(&self) {
        self.out8(PO_CR, 0);
        self.out8(PO_CR, CR_RESET);
        for _ in 0..100_000 {
            if self.in8(PO_CR) & CR_RESET == 0 {
                break;
            }
        }
        self.out32(PO_BDBAR, self.bdl_phys as u32);
    }

    /// The codec's playback rate: what it took (it may round).
    fn set_rate(&self, rate: u32) -> u32 {
        if !self.vra {
            return 48000;
        }
        self.nam_write(NAM_FRONT_RATE, rate as u16);
        match self.nam_read(NAM_FRONT_RATE) as u32 {
            0 => rate,
            r => r,
        }
    }
}

struct Sound {
    card: Option<Ac97>,
    /// /dev/dsp is open (one program at a time).
    open: bool,
    /// The program's format: rate, channels (1 or 2), AFMT_*.
    rate: u32,
    channels: u32,
    format: u32,
    /// The rate the card plays at (the program's, or 48000 to resample to).
    dev_rate: u32,
    /// SETFRAGMENT: 2^shift bytes (of the program's data) per fragment,
    /// and how many fragments may be queued.
    frag_shift: u32,
    frag_count: usize,
    /// Buffer being filled, and the bytes in it.
    tail: usize,
    fill: usize,
    /// The card has been started (CR_RUN) since the last reset.
    running: bool,
    /// A partial frame of the program's data (between writes).
    partial: [u8; 8],
    partial_len: usize,
    /// Resampling: 16.16 fixed-point position between input frames.
    phase: u32,
    /// Bytes (card format) handed to the card, for GETOPTR.
    submitted: u64,
    /// OSS volumes: left | right << 8, 0-100.
    volume: u32,
    pcm: u32,
}

static SOUND: Mutex<Sound> = Mutex::new(Sound {
    card: None,
    open: false,
    rate: 8000,
    channels: 1,
    format: AFMT_U8,
    dev_rate: 8000,
    frag_shift: 12,
    frag_count: 16,
    tail: 0,
    fill: 0,
    running: false,
    partial: [0; 8],
    partial_len: 0,
    phase: 0,
    submitted: 0,
    volume: 100 | 100 << 8,
    pcm: 100 | 100 << 8,
});

/// What a process waiting for room in the ring blocks on (with a deadline
/// of the next tick, when it looks again).
const SOUND_KEY: u64 = 0x7E7E_0000_534E_4420;

/// Find and start an AC'97 controller. Returns its description for the
/// boot log, or None if there is none.
pub fn init() -> Option<&'static str> {
    // A multimedia audio controller (class 4, subclass 1): AC'97.
    let pci = crate::net::pci::scan().into_iter().find(|d| d.class == 0x04 && d.subclass == 0x01)?;
    let (bar0, bar1) = (pci.bar(0), pci.bar(1));
    if bar0 & 1 == 0 || bar1 & 1 == 0 {
        return None; // AC'97 uses I/O ports
    }
    pci.enable();
    let nam = (bar0 & !3) as u16;
    let nabm = (bar1 & !3) as u16;
    // DMA memory below 4 GiB: the descriptor list and the buffers.
    let mut alloc = memory::FRAME_ALLOCATOR.lock();
    let frames = alloc.as_mut()?;
    let bdl_phys = frames.allocate_contiguous(1)?.start_address().as_u64();
    let mut buf_phys = [0u64; BUFS];
    for b in buf_phys.iter_mut() {
        *b = frames.allocate_contiguous(1)?.start_address().as_u64();
    }
    drop(alloc);
    if bdl_phys >= 1 << 32 || buf_phys.iter().any(|&b| b >= 1 << 32) {
        return None;
    }
    let mut card = Ac97 { nam, nabm, bdl_phys, buf_phys, vra: false };
    unsafe { core::ptr::write_bytes(memory::phys_ptr(x86_64::PhysAddr::new(bdl_phys)), 0, 4096) };
    for i in 0..BUFS {
        unsafe { core::ptr::write_bytes(card.buf(i), 0, BUF_BYTES) };
    }
    // Out of cold reset, then wait for the codec.
    card.out32(GLOB_CNT, 0x2);
    let mut ready = false;
    for _ in 0..1_000_000 {
        if card.in32(GLOB_STA) & (1 << 8) != 0 {
            ready = true;
            break;
        }
    }
    if !ready {
        return None;
    }
    card.nam_write(NAM_RESET, 1);
    card.nam_write(NAM_MASTER, 0x0000); // full volume, not muted
    card.nam_write(NAM_HEADPHONE, 0x0000);
    card.nam_write(NAM_PCM_OUT, 0x0808); // 0 dB
    if card.nam_read(NAM_EXT_ID) & 1 != 0 {
        let ctrl = card.nam_read(NAM_EXT_CTRL);
        card.nam_write(NAM_EXT_CTRL, ctrl | 1);
        card.vra = card.nam_read(NAM_EXT_CTRL) & 1 != 0;
    }
    card.reset_output();
    let vra = card.vra;
    SOUND.lock().card = Some(card);
    Some(if vra { "AC'97 audio (any rate)" } else { "AC'97 audio (48 kHz)" })
}

pub fn present() -> bool {
    SOUND.lock().card.is_some()
}

/// /proc/sound: the card, the program's format, and where the ring is.
pub fn status() -> alloc::string::String {
    let s = SOUND.lock();
    let Some(card) = s.card.as_ref() else { return "No sound card\n".into() };
    alloc::format!(
        "AC'97 audio{}\nopen: {}\nformat: {} Hz, {} channel(s), {}\ncard rate: {} Hz\nfragment: {} bytes, {} queued at most\n\
         ring: tail {} fill {} queued {} running {}\ncard: civ {} lvi {} sr {:#06x} picb {} cr {:#04x}\nplayed: {} bytes\n",
        if card.vra { " (any rate)" } else { " (48 kHz)" },
        s.open,
        s.rate,
        s.channels,
        match s.format { AFMT_S32_LE => "32-bit", AFMT_S16_LE => "16-bit", AFMT_S8 => "signed 8-bit", _ => "unsigned 8-bit" },
        s.dev_rate,
        s.frag(),
        s.max_queued(),
        s.tail,
        s.fill,
        s.queued(),
        s.running,
        card.in8(PO_CIV),
        card.in8(PO_LVI),
        card.in16(PO_SR),
        card.in16(PO_PICB),
        card.in8(PO_CR),
        s.submitted,
    )
}

/// /dev/dsp is open and the card has sound queued.
pub fn playing() -> bool {
    let s = SOUND.lock();
    s.card.is_some() && s.open && s.queued() > 0
}

/// An open /dev/dsp: closing it lets what was written finish playing.
pub struct Dsp;

impl Drop for Dsp {
    fn drop(&mut self) {
        let mut s = SOUND.lock();
        // What's left in the buffer being filled: hand it over if there's
        // room (else it's dropped; close doesn't wait).
        if s.fill > 0 && s.queued() + 1 < BUFS {
            s.submit();
        }
        // Nothing left to play: stop the card (as drivers do once a stream
        // has drained), rather than leave it running on an empty ring.
        if s.queued() == 0 && s.running {
            s.restart();
        }
        s.open = false;
    }
}

/// open("/dev/dsp"): the default format (8 kHz, 8-bit unsigned, mono).
pub fn open_dsp() -> Result<Arc<Dsp>, i64> {
    let mut s = SOUND.lock();
    if s.card.is_none() {
        return Err(ENODEV);
    }
    if s.open {
        return Err(EBUSY);
    }
    s.open = true;
    s.rate = 8000;
    s.channels = 1;
    s.format = AFMT_U8;
    s.frag_shift = 12;
    s.frag_count = 16;
    s.partial_len = 0;
    s.phase = 0;
    s.fill = 0;
    let mut settle = false;
    if s.queued() == 0 {
        // Left running by the last program (it ended with sound still
        // playing): stop it, and give it a moment stopped before it starts
        // again. (QEMU only turns its output off after a while; started
        // again at once, it pulls the next sound in a burst.)
        settle = s.running;
        s.restart();
    }
    s.apply_rate();
    drop(s);
    if settle {
        let until = crate::process::ticks() + 3;
        while crate::process::ticks() < until && !crate::process::signal_pending() {
            crate::process::block_on_until(SOUND_KEY, until);
        }
    }
    Ok(Arc::new(Dsp))
}

pub fn open_mixer() -> Result<(), i64> {
    if present() { Ok(()) } else { Err(ENODEV) }
}

impl Sound {
    fn card(&self) -> &Ac97 {
        self.card.as_ref().unwrap()
    }

    /// Buffers handed to the card and not yet played (including the one
    /// playing).
    fn queued(&self) -> usize {
        if !self.running {
            return 0;
        }
        let card = self.card();
        if card.in16(PO_SR) & SR_HALTED != 0 {
            return 0;
        }
        let civ = card.in8(PO_CIV) as usize % BUFS;
        (self.tail + BUFS - civ) % BUFS
    }

    /// Stop the card and start the ring over (nothing is queued).
    fn restart(&mut self) {
        self.card().reset_output();
        self.running = false;
        self.tail = 0;
        self.fill = 0;
    }

    /// The card's rate for the program's (resampling if the codec can't).
    fn apply_rate(&mut self) {
        // (Only while nothing plays: the codec's rate is for everything.)
        if self.queued() == 0 {
            self.dev_rate = self.card().set_rate(self.rate);
            self.phase = 0;
        }
    }

    fn frame_bytes(&self) -> usize {
        self.channels as usize * self.sample_bytes()
    }

    fn sample_bytes(&self) -> usize {
        match self.format {
            AFMT_S32_LE => 4,
            AFMT_S16_LE => 2,
            _ => 1,
        }
    }

    /// The card's bytes for `n` bytes of the program's data.
    fn to_card(&self, n: usize) -> usize {
        (n as u64 * 4 * self.dev_rate as u64 / (self.frame_bytes() as u64 * self.rate as u64)) as usize
    }

    /// The program's bytes for `n` of the card's.
    fn to_program(&self, n: usize) -> usize {
        (n as u64 * self.frame_bytes() as u64 * self.rate as u64 / (4 * self.dev_rate as u64)) as usize
    }

    /// Bytes per buffer (the card's), from SETFRAGMENT: a power of two from
    /// 256 to 4096.
    fn frag(&self) -> usize {
        let want = self.to_card(1 << self.frag_shift).max(256).min(BUF_BYTES);
        1 << (usize::BITS - 1 - want.leading_zeros())
    }

    fn max_queued(&self) -> usize {
        self.frag_count.clamp(2, BUFS - 2)
    }

    /// Hand buffer `tail` (with `fill` bytes) to the card.
    fn submit(&mut self) {
        let (tail, fill) = (self.tail, self.fill);
        let card = self.card();
        card.set_bd(tail, (fill / 2) as u16);
        card.out8(PO_LVI, tail as u8);
        if !self.running {
            card.out8(PO_CR, CR_RUN);
        }
        self.running = true;
        self.submitted += fill as u64;
        self.tail = (tail + 1) % BUFS;
        self.fill = 0;
    }

    /// Put one 16-bit stereo frame in the buffer being filled. False if the
    /// ring is full.
    fn put_frame(&mut self, l: i16, r: i16) -> bool {
        if self.fill >= self.frag() {
            if self.queued() >= self.max_queued() {
                return false;
            }
            self.submit();
        }
        let p = unsafe { self.card().buf(self.tail).add(self.fill) };
        unsafe {
            core::ptr::write_volatile(p as *mut i16, l);
            core::ptr::write_volatile(p.add(2) as *mut i16, r);
        }
        self.fill += 4;
        true
    }

    /// One frame of the program's data, resampled to the card's rate. False
    /// (and nothing taken) if the ring is full.
    fn put_input_frame(&mut self, l: i16, r: i16) -> bool {
        if self.dev_rate == self.rate {
            return self.put_frame(l, r);
        }
        // Repeat or skip input frames: each one covers
        // dev_rate / rate output frames.
        let step = ((self.rate as u64) << 16) / self.dev_rate as u64;
        let (phase, fill, tail) = (self.phase, self.fill, self.tail);
        let mut ph = phase as u64;
        while ph < 1 << 16 {
            if !self.put_frame(l, r) {
                // Undo this frame's output: it's tried again whole.
                if self.tail == tail {
                    self.fill = fill;
                }
                return false;
            }
            ph += step;
        }
        self.phase = (ph - (1 << 16)) as u32;
        true
    }

    /// Take as much of `data` as fits. Returns the bytes taken.
    fn take(&mut self, data: &[u8]) -> usize {
        let fb = self.frame_bytes();
        let mut used = 0;
        while used < data.len() {
            let need = fb - self.partial_len;
            let n = need.min(data.len() - used);
            let start = self.partial_len;
            self.partial[start..start + n].copy_from_slice(&data[used..used + n]);
            if self.partial_len + n < fb {
                self.partial_len += n;
                used += n;
                break;
            }
            let f = self.partial;
            let sample = |i: usize| -> i16 {
                match self.format {
                    AFMT_S16_LE => i16::from_le_bytes([f[2 * i], f[2 * i + 1]]),
                    AFMT_S32_LE => i16::from_le_bytes([f[4 * i + 2], f[4 * i + 3]]),
                    AFMT_S8 => (f[i] as i8 as i16) << 8,
                    _ => ((f[i] as i16) - 128) << 8,
                }
            };
            let (l, r) = if self.channels == 2 { (sample(0), sample(1)) } else { (sample(0), sample(0)) };
            if !self.put_input_frame(l, r) {
                break; // (the partial frame stays as it was)
            }
            self.partial_len = 0;
            used += n;
        }
        used
    }

    /// Room for more, in the program's bytes (GETOSPACE, poll).
    fn space(&self) -> usize {
        // The buffer being filled, plus one more per buffer the card can
        // still be given.
        let can_submit = self.max_queued().saturating_sub(self.queued());
        self.to_program(((can_submit + 1) * self.frag()).saturating_sub(self.fill))
    }

    /// The card's bytes still to play.
    fn delay(&self) -> usize {
        let queued = self.queued();
        if queued == 0 {
            return self.fill;
        }
        let left_in_current = self.card().in16(PO_PICB) as usize * 2;
        (queued - 1) * self.frag() + left_in_current + self.fill
    }
}

/// write() to /dev/dsp: waits for room unless non-blocking.
pub fn write(data: &[u8], nonblocking: bool) -> i64 {
    let mut done = 0;
    loop {
        {
            let mut s = SOUND.lock();
            if s.card.is_none() {
                return -ENODEV;
            }
            done += s.take(&data[done..]);
            // Nothing playing and a full buffer waiting: start it now.
            if s.fill >= s.frag() && s.queued() < s.max_queued() {
                s.submit();
            }
        }
        if done == data.len() {
            return done as i64;
        }
        if nonblocking {
            return if done > 0 { done as i64 } else { -EAGAIN };
        }
        if crate::process::signal_pending() {
            return if done > 0 { done as i64 } else { -EINTR };
        }
        crate::process::block_on_until(SOUND_KEY, crate::process::ticks() + 1);
    }
}

/// Room to write without waiting (poll's POLLOUT).
pub fn writable() -> bool {
    let s = SOUND.lock();
    s.card.is_some() && s.space() > 0
}

/// Wait until everything written has played (SNDCTL_DSP_SYNC).
fn sync() -> i64 {
    loop {
        {
            let mut s = SOUND.lock();
            if s.fill > 0 && s.queued() < s.max_queued() {
                s.submit();
            }
            if s.fill == 0 && s.queued() == 0 {
                return 0;
            }
        }
        if crate::process::signal_pending() {
            return -EINTR;
        }
        crate::process::block_on_until(SOUND_KEY, crate::process::ticks() + 1);
    }
}

// OSS ioctls ('P' for /dev/dsp, 'M' for the mixer)
const SNDCTL_DSP_RESET: u64 = 0x5000;
const SNDCTL_DSP_SYNC: u64 = 0x5001;
const SNDCTL_DSP_SPEED: u64 = 0xC004_5002;
const SNDCTL_DSP_STEREO: u64 = 0xC004_5003;
const SNDCTL_DSP_GETBLKSIZE: u64 = 0xC004_5004;
const SNDCTL_DSP_SETFMT: u64 = 0xC004_5005;
const SNDCTL_DSP_CHANNELS: u64 = 0xC004_5006;
const SOUND_PCM_READ_RATE: u64 = 0x8004_5002;
const SOUND_PCM_READ_CHANNELS: u64 = 0x8004_5006;
const SOUND_PCM_READ_BITS: u64 = 0x8004_5005;
const SNDCTL_DSP_POST: u64 = 0x5008;
const SNDCTL_DSP_SETFRAGMENT: u64 = 0xC004_500A;
const SNDCTL_DSP_GETFMTS: u64 = 0x8004_500B;
const SNDCTL_DSP_GETOSPACE: u64 = 0x8010_500C;
pub const SNDCTL_DSP_NONBLOCK: u64 = 0x500E;
const SNDCTL_DSP_GETCAPS: u64 = 0x8004_500F;
const SNDCTL_DSP_GETTRIGGER: u64 = 0x8004_5010;
const SNDCTL_DSP_SETTRIGGER: u64 = 0x4004_5010;
const SNDCTL_DSP_GETOPTR: u64 = 0x800C_5012;
const SNDCTL_DSP_GETODELAY: u64 = 0x8004_5017;
const OSS_GETVERSION: u64 = 0x8004_4D76;
const SOUND_VERSION: i32 = 0x030802;
const DSP_CAP_REALTIME: i32 = 0x200;
const DSP_CAP_TRIGGER: i32 = 0x1000;
const PCM_ENABLE_OUTPUT: i32 = 2;

fn read_int(arg: u64) -> Result<i32, i64> {
    let b = unsafe { crate::syscall::user_slice(arg, 4) }.ok_or(EFAULT)?;
    Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn write_bytes(arg: u64, data: &[u8]) -> i64 {
    match unsafe { crate::syscall::user_slice_mut(arg, data.len() as u64) } {
        Some(b) => {
            b.copy_from_slice(data);
            0
        }
        None => -EFAULT,
    }
}

fn write_ints(arg: u64, v: &[i32]) -> i64 {
    let mut b = alloc::vec::Vec::with_capacity(v.len() * 4);
    for x in v {
        b.extend_from_slice(&x.to_le_bytes());
    }
    write_bytes(arg, &b)
}

/// ioctl() on /dev/dsp.
pub fn dsp_ioctl(request: u64, arg: u64) -> i64 {
    let int = || read_int(arg);
    match request {
        SNDCTL_DSP_RESET => {
            let mut s = SOUND.lock();
            s.restart();
            s.partial_len = 0;
            s.phase = 0;
            0
        }
        SNDCTL_DSP_SYNC => sync(),
        SNDCTL_DSP_POST => {
            let mut s = SOUND.lock();
            if s.fill > 0 && s.queued() < s.max_queued() {
                s.submit();
            }
            0
        }
        SNDCTL_DSP_SPEED => {
            let rate = match int() {
                Ok(r) => r.clamp(8000, 48000) as u32,
                Err(e) => return -e,
            };
            let mut s = SOUND.lock();
            s.rate = rate;
            s.apply_rate();
            // (With variable rate, the codec's own; else what the program
            // asked for: the driver resamples.)
            let actual = if s.card().vra && s.dev_rate != 48000 { s.dev_rate } else { rate };
            s.rate = actual;
            write_ints(arg, &[actual as i32])
        }
        SOUND_PCM_READ_RATE => write_ints(arg, &[SOUND.lock().rate as i32]),
        SNDCTL_DSP_STEREO | SNDCTL_DSP_CHANNELS => {
            let v = match int() {
                Ok(v) => v,
                Err(e) => return -e,
            };
            let ch = if request == SNDCTL_DSP_STEREO { if v != 0 { 2 } else { 1 } } else if v <= 1 { 1 } else { 2 };
            SOUND.lock().channels = ch;
            write_ints(arg, &[if request == SNDCTL_DSP_STEREO { (ch == 2) as i32 } else { ch as i32 }])
        }
        SOUND_PCM_READ_CHANNELS => write_ints(arg, &[SOUND.lock().channels as i32]),
        SNDCTL_DSP_SETFMT => {
            let v = match int() {
                Ok(v) => v as u32,
                Err(e) => return -e,
            };
            let mut s = SOUND.lock();
            if matches!(v, AFMT_U8 | AFMT_S8 | AFMT_S16_LE | AFMT_S32_LE) {
                s.format = v;
            } else if v != 0 {
                s.format = AFMT_S16_LE; // the nearest it can do (OSS returns what it chose)
            }
            let f = s.format;
            write_ints(arg, &[f as i32])
        }
        SOUND_PCM_READ_BITS => write_ints(arg, &[SOUND.lock().sample_bytes() as i32 * 8]),
        SNDCTL_DSP_GETFMTS => write_ints(arg, &[(AFMT_U8 | AFMT_S8 | AFMT_S16_LE | AFMT_S32_LE) as i32]),
        SNDCTL_DSP_SETFRAGMENT => {
            let v = match int() {
                Ok(v) => v as u32,
                Err(e) => return -e,
            };
            let mut s = SOUND.lock();
            s.frag_shift = (v & 0xffff).clamp(7, 16);
            let count = (v >> 16) as usize;
            s.frag_count = if count == 0x7fff || count == 0 { 16 } else { count };
            0
        }
        SNDCTL_DSP_GETBLKSIZE => {
            let s = SOUND.lock();
            let n = s.to_program(s.frag()) as i32;
            write_ints(arg, &[n])
        }
        SNDCTL_DSP_GETOSPACE => {
            // struct audio_buf_info { fragments, fragstotal, fragsize, bytes }
            let s = SOUND.lock();
            let fragsize = s.to_program(s.frag()).max(1);
            let bytes = s.space();
            let total = s.max_queued();
            write_ints(arg, &[(bytes / fragsize) as i32, total as i32, fragsize as i32, bytes as i32])
        }
        SNDCTL_DSP_GETODELAY => {
            let s = SOUND.lock();
            let n = s.to_program(s.delay()) as i32;
            write_ints(arg, &[n])
        }
        SNDCTL_DSP_GETOPTR => {
            // struct count_info { bytes, blocks, ptr }: what has played.
            let s = SOUND.lock();
            let left = s.delay().saturating_sub(s.fill) as u64;
            let played = s.submitted.saturating_sub(left);
            let bytes = s.to_program(played as usize) as i32;
            let blocks = (played / s.frag() as u64) as i32;
            write_ints(arg, &[bytes, blocks, (played % (BUFS * s.frag()) as u64) as i32])
        }
        SNDCTL_DSP_GETCAPS => write_ints(arg, &[1 | DSP_CAP_REALTIME | DSP_CAP_TRIGGER]),
        SNDCTL_DSP_GETTRIGGER => write_ints(arg, &[PCM_ENABLE_OUTPUT]),
        SNDCTL_DSP_SETTRIGGER => 0,
        OSS_GETVERSION => write_ints(arg, &[SOUND_VERSION]),
        _ if (request >> 8) & 0xff == b'P' as u64 => -EINVAL,
        _ => -ENOTTY,
    }
}

// Mixer channels (SOUND_MIXER_*) and the requests on them
const MIXER_VOLUME: u64 = 0;
const MIXER_PCM: u64 = 4;
const MIXER_RECSRC: u64 = 0xFF;
const MIXER_DEVMASK: u64 = 0xFE;
const MIXER_RECMASK: u64 = 0xFD;
const MIXER_CAPS: u64 = 0xFC;
const MIXER_STEREODEVS: u64 = 0xFB;
const SOUND_MIXER_INFO: u64 = 0x805C_4D65;

/// An OSS volume (left | right << 8, 0-100) as an AC'97 attenuation:
/// `zero` is the register value for full volume (0 dB), 31 the quietest,
/// and 0 mutes.
fn ac97_volume(v: u32, zero: u16) -> u16 {
    let atten = |x: u32| -> u16 { zero + ((100 - x.min(100)) as u16 * (31 - zero)) / 100 };
    let (l, r) = (v & 0xff, (v >> 8) & 0xff);
    if l == 0 && r == 0 {
        return 0x8000; // mute
    }
    atten(l) << 8 | atten(r)
}

/// ioctl() on /dev/mixer: the master and PCM volumes.
pub fn mixer_ioctl(request: u64, arg: u64) -> i64 {
    let dir = request >> 30;
    let ty = (request >> 8) & 0xff;
    let nr = request & 0xff;
    if request == SOUND_MIXER_INFO {
        // struct mixer_info { char id[16]; char name[32]; int modify_counter; int fillers[10]; }
        let mut info = [0u8; 92];
        info[..5].copy_from_slice(b"AC97\0");
        info[16..32].copy_from_slice(b"AC'97 audio\0\0\0\0\0");
        return write_bytes(arg, &info);
    }
    if request == OSS_GETVERSION {
        return write_ints(arg, &[SOUND_VERSION]);
    }
    if ty != b'M' as u64 {
        return -ENOTTY;
    }
    let devs = (1 << MIXER_VOLUME | 1 << MIXER_PCM) as i32;
    match (dir, nr) {
        (2, MIXER_DEVMASK) | (2, MIXER_STEREODEVS) => write_ints(arg, &[devs]),
        (2, MIXER_RECMASK) | (2, MIXER_RECSRC) | (2, MIXER_CAPS) => write_ints(arg, &[0]),
        (2, MIXER_VOLUME) => write_ints(arg, &[SOUND.lock().volume as i32]),
        (2, MIXER_PCM) => write_ints(arg, &[SOUND.lock().pcm as i32]),
        (3, MIXER_VOLUME) | (3, MIXER_PCM) => {
            let v = match read_int(arg) {
                Ok(v) => v as u32,
                Err(e) => return -e,
            };
            let v = (v & 0xff).min(100) | ((v >> 8) & 0xff).min(100) << 8;
            let mut s = SOUND.lock();
            if s.card.is_none() {
                return -ENODEV;
            }
            if nr == MIXER_VOLUME {
                s.volume = v;
                let reg = ac97_volume(v, 0);
                s.card().nam_write(NAM_MASTER, reg);
                s.card().nam_write(NAM_HEADPHONE, reg);
            } else {
                s.pcm = v;
                s.card().nam_write(NAM_PCM_OUT, ac97_volume(v, 8));
            }
            write_ints(arg, &[v as i32])
        }
        (3, _) => -EINVAL,
        (2, _) => -EINVAL,
        _ => -ENOTTY,
    }
}

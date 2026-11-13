// Copyright (c) 2026 tre4surehunter9
//! virtio-sound (QEMU's -device virtio-sound-pci): a sound card made for
//! virtual machines, for sound.rs's OSS interface (/dev/dsp) as the AC'97
//! card is. Output only.
//!
//! Queues: control (requests: what streams there are, a stream's format,
//! prepare, start, stop, release), events, and the output stream's
//! buffers. A buffer is a chain: the stream's number then the samples
//! (read by the device), and a status (written by it). The device hands a
//! buffer back once it has taken its sound: buffers queued play in order,
//! as the AC'97 ring's do.
//!
//! sound.rs fills BUFS buffers in turn (always 16-bit stereo) and hands
//! them over; here the ring position is counted from the buffers handed
//! back.

use spin::Mutex;

use crate::virtio::{self, Device, Queue};

const R_PCM_INFO: u32 = 0x0100;
const R_PCM_SET_PARAMS: u32 = 0x0101;
const R_PCM_PREPARE: u32 = 0x0102;
const R_PCM_RELEASE: u32 = 0x0103;
const R_PCM_START: u32 = 0x0104;
const R_PCM_STOP: u32 = 0x0105;
const S_OK: u32 = 0x8000;
const DIRECTION_OUTPUT: u8 = 0;
const FMT_S16: u8 = 5;
/// The rates the protocol names, in its order (VIRTIO_SND_PCM_RATE_*).
const RATES: [u32; 14] = [5512, 8000, 11025, 16000, 22050, 32000, 44100, 48000, 64000, 88200, 96000, 176400, 192000, 384000];

/// The output buffers' state (what take_used changes: queued() only reads
/// sound.rs's state, so this is behind its own lock).
struct Tx {
    queue: Queue,
    /// Buffers handed over, and handed back, since the last reset.
    submitted: u64,
    completed: u64,
}

pub struct VirtioSnd {
    _dev: Device,
    control: Queue,
    _events: Queue,
    tx: Mutex<Tx>,
    stream: u32,
    /// Rates the stream plays (bit n: RATES[n]).
    rates: u64,
    rate: u32,
    /// The stream has been started since the last reset.
    started: bool,
    /// Per buffer: two pages (the stream's number, then up to 4096 bytes
    /// of samples) and its status (8 bytes in `status`).
    bufs: [(u64, *mut u8); BUFS],
    status: (u64, *mut u8),
}

unsafe impl Send for VirtioSnd {}

pub const BUFS: usize = 32;
const HEADER: usize = 4;

impl VirtioSnd {
    /// A control request: its fields after the code, and room for the
    /// answer's after the status. The answer, if the status is OK.
    fn control(&mut self, code: u32, fields: &[u8], answer: usize) -> Option<alloc::vec::Vec<u8>> {
        let (req_phys, req) = self.control.scratch();
        let resp_phys = req_phys + 2048; // (the answer: in the same page's second half)
        unsafe {
            core::ptr::write_volatile(req as *mut u32, code);
            core::ptr::copy_nonoverlapping(fields.as_ptr(), req.add(4), fields.len());
            core::ptr::write_bytes(req.add(2048), 0, 4 + answer);
        }
        let len = 4 + fields.len() as u32;
        if !self.control.request_chain(&[(req_phys, len, false), (resp_phys, 4 + answer as u32, true)]) {
            return None;
        }
        let resp = unsafe { core::slice::from_raw_parts(req.add(2048), 4 + answer) };
        (u32::from_le_bytes(resp[..4].try_into().unwrap()) == S_OK).then(|| resp[4..].to_vec())
    }

    fn stream_request(&mut self, code: u32) -> bool {
        let s = self.stream.to_le_bytes();
        self.control(code, &s, 0).is_some()
    }

    fn set_params(&mut self) -> bool {
        let rate_index = RATES.iter().position(|&r| r == self.rate).unwrap_or(7) as u8;
        let mut f = alloc::vec::Vec::with_capacity(20);
        f.extend_from_slice(&self.stream.to_le_bytes());
        f.extend_from_slice(&((BUFS * 4096) as u32).to_le_bytes()); // buffer bytes
        f.extend_from_slice(&4096u32.to_le_bytes()); // period bytes
        f.extend_from_slice(&0u32.to_le_bytes()); // features
        f.extend_from_slice(&[2, FMT_S16, rate_index, 0]); // channels, format, rate
        self.control(R_PCM_SET_PARAMS, &f, 0).is_some()
    }

    /// Buffer `i`'s samples.
    pub fn buf(&self, i: usize) -> *mut u8 {
        unsafe { self.bufs[i].1.add(HEADER) }
    }

    /// Hand buffer `i` (`bytes` of samples) to the device, starting the
    /// stream if it isn't playing.
    pub fn submit(&mut self, i: usize, bytes: usize) {
        {
            let mut tx = self.tx.lock();
            let (phys, _) = self.bufs[i];
            let status = self.status.0 + 8 * i as u64;
            tx.queue.submit_chain(2 * i as u16, &[(phys, (HEADER + bytes) as u32, false), (status, 8, true)]);
            tx.queue.kick();
            tx.submitted += 1;
        }
        if !self.started {
            self.started = self.stream_request(R_PCM_START);
        }
    }

    /// Buffers handed over and not yet handed back.
    pub fn queued(&self) -> usize {
        let mut tx = self.tx.lock();
        while tx.queue.take_used().is_some() {
            tx.completed += 1;
        }
        (tx.submitted - tx.completed.min(tx.submitted)) as usize
    }

    /// Stop the stream and start over: nothing queued, the ring at 0.
    pub fn reset_output(&mut self) {
        if self.started {
            self.stream_request(R_PCM_STOP);
            self.started = false;
        }
        // Released, the device hands back every buffer it still has.
        self.stream_request(R_PCM_RELEASE);
        for _ in 0..1_000_000 {
            if self.queued() == 0 {
                break;
            }
            core::hint::spin_loop();
        }
        {
            let mut tx = self.tx.lock();
            while tx.queue.take_used().is_some() {}
            tx.submitted = 0;
            tx.completed = 0;
        }
        self.set_params();
        self.stream_request(R_PCM_PREPARE);
    }

    /// Play at `rate` if the stream can (else 48000): the rate it took.
    pub fn set_rate(&mut self, rate: u32) -> u32 {
        let r = match RATES.iter().position(|&r| r == rate) {
            Some(i) if self.rates & (1 << i) != 0 => rate,
            _ => 48000,
        };
        if r != self.rate {
            self.rate = r;
            self.reset_output();
        }
        r
    }

    /// Whether the stream plays rates other than 48000.
    pub fn any_rate(&self) -> bool {
        self.rates & !(1 << 7) != 0
    }

    /// (sound.rs's /proc/sound.)
    pub fn describe(&self) -> alloc::string::String {
        let tx = self.tx.lock();
        alloc::format!("virtio stream {} at {} Hz, started {}, buffers handed over {}, back {}\n", self.stream, self.rate, self.started, tx.submitted, tx.completed)
    }
}

/// Find a virtio sound card with an output stream and get it ready.
pub fn init() -> Option<VirtioSnd> {
    let pci = virtio::find(25).into_iter().next()?;
    let (dev, _) = Device::new(pci, 0)?;
    dev.no_interrupts(); // (polled, as the AC'97 card is)
    let streams = dev.config32(4);
    let control = dev.queue(0, 8)?;
    let events = dev.queue(1, 8)?;
    let tx_queue = dev.queue(2, 2 * BUFS as u16)?;
    if tx_queue.size() < 2 * BUFS as u16 {
        return None; // (two descriptors a buffer)
    }
    let mut bufs = [(0u64, core::ptr::null_mut()); BUFS];
    {
        let mut alloc = crate::memory::FRAME_ALLOCATOR.lock();
        let a = alloc.as_mut()?;
        for b in bufs.iter_mut() {
            let phys = a.allocate_contiguous(2)?.start_address().as_u64();
            *b = (phys, crate::memory::phys_to_virt(phys) as *mut u8);
        }
    }
    let status = virtio::dma_page()?;
    dev.start();
    let mut snd = VirtioSnd {
        _dev: dev,
        control,
        _events: events,
        tx: Mutex::new(Tx { queue: tx_queue, submitted: 0, completed: 0 }),
        stream: 0,
        rates: 1 << 7,
        rate: 48000,
        started: false,
        bufs,
        status,
    };
    // The streams: the first output one that plays 16-bit stereo.
    let n = streams.min(16);
    let mut q = alloc::vec::Vec::new();
    q.extend_from_slice(&0u32.to_le_bytes()); // start id
    q.extend_from_slice(&n.to_le_bytes()); // count
    q.extend_from_slice(&32u32.to_le_bytes()); // each one's size
    let info = snd.control(R_PCM_INFO, &q, 32 * n as usize)?;
    let found = info.chunks(32).enumerate().find(|(_, s)| {
        let formats = u64::from_le_bytes(s[8..16].try_into().unwrap());
        s[24] == DIRECTION_OUTPUT && formats & (1 << FMT_S16) != 0 && s[25] <= 2 && s[26] >= 2
    });
    let (id, s) = found?;
    snd.stream = id as u32;
    snd.rates = u64::from_le_bytes(s[16..24].try_into().unwrap());
    for i in 0..BUFS {
        unsafe { core::ptr::write_volatile(snd.bufs[i].1 as *mut u32, snd.stream) };
    }
    if !snd.set_params() || !snd.stream_request(R_PCM_PREPARE) {
        return None;
    }
    Some(snd)
}

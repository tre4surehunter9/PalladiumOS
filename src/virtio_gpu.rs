// Copyright (c) 2026 tre4surehunter9
//! virtio-gpu, 2D (QEMU's -device virtio-vga or virtio-gpu-pci): the
//! screen through a virtio device instead of VGA's video memory.
//!
//! The picture is in ordinary memory: a framebuffer the kernel allocates,
//! attached as the backing of a 2D resource on the device, and that
//! resource shown on the first scanout (the screen). The device doesn't
//! watch that memory: the driver tells it which part changed (TRANSFER_TO_
//! HOST_2D: copy it into the device's resource; RESOURCE_FLUSH: show it).
//! The console and /dev/fb0 (X) draw into the framebuffer as before; the
//! timer hands the screen over (`tick`) about 33 times a second while the
//! console has drawn something, or always while a program (X) has the
//! screen, since its drawing through the mapped framebuffer isn't seen.
//!
//! The firmware and the bootloader meanwhile use the card's VGA mode (with
//! virtio-vga), so BIOS and UEFI booting show the boot as before until this
//! takes over.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

use crate::virtio::{self, Device, Queue};

// Commands and answers.
const GET_DISPLAY_INFO: u32 = 0x0100;
const RESOURCE_CREATE_2D: u32 = 0x0101;
const SET_SCANOUT: u32 = 0x0103;
const RESOURCE_FLUSH: u32 = 0x0104;
const TRANSFER_TO_HOST_2D: u32 = 0x0105;
const RESOURCE_ATTACH_BACKING: u32 = 0x0106;
const RESP_OK_NODATA: u32 = 0x1100;
const RESP_OK_DISPLAY_INFO: u32 = 0x1101;

/// The pixel format: bytes B, G, R, unused (as the console draws them).
const FORMAT_B8G8R8X8: u32 = 2;
/// The screen's resource.
const SCREEN: u32 = 1;
/// Hand the screen over every this many timer ticks (10 ms each).
const FLUSH_TICKS: u64 = 3;

struct Gpu {
    /// (Kept: its registers stay mapped and in use; the cursor's queue
    /// comes from it.)
    #[allow(dead_code)]
    dev: Device,
    control: Queue,
    width: u32,
    height: u32,
}

static GPU: Mutex<Option<Gpu>> = Mutex::new(None);
static LAST_FLUSH: AtomicU64 = AtomicU64::new(0);

/// struct virtio_gpu_ctrl_hdr: type, flags, fence id, context, ring (24 bytes).
fn header(kind: u32) -> Vec<u8> {
    let mut v = alloc::vec![0u8; 24];
    v[0..4].copy_from_slice(&kind.to_le_bytes());
    v
}

fn push32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}

fn answer_type(resp: &[u8]) -> u32 {
    resp.get(0..4).map_or(0, |b| u32::from_le_bytes(b.try_into().unwrap()))
}

impl Gpu {
    /// A command with no data in its answer: did it work?
    fn command(&mut self, cmd: &[u8]) -> bool {
        answer_type(&self.control.request(cmd, 24)) == RESP_OK_NODATA
    }

    /// Copy rectangle (x, y, w, h) of the framebuffer to the device and
    /// show it.
    fn flush(&mut self, x: u32, y: u32, w: u32, h: u32) {
        let mut t = header(TRANSFER_TO_HOST_2D);
        for v in [x, y, w, h] {
            push32(&mut t, v);
        }
        // (offset into the backing of the rectangle's start)
        t.extend_from_slice(&((y as u64 * self.width as u64 + x as u64) * 4).to_le_bytes());
        push32(&mut t, SCREEN);
        push32(&mut t, 0);
        self.command(&t);
        let mut f = header(RESOURCE_FLUSH);
        for v in [x, y, w, h] {
            push32(&mut f, v);
        }
        push32(&mut f, SCREEN);
        push32(&mut f, 0);
        self.command(&f);
    }
}

/// Find a virtio GPU and put the screen on it. Returns its size.
pub fn init() -> Option<(u32, u32)> {
    let pci = virtio::find(16).into_iter().next()?;
    let (dev, _) = Device::new(pci, 0)?;
    let control = dev.queue(0, 64)?;
    dev.start();
    let mut gpu = Gpu { dev, control, width: 0, height: 0 };

    // The screen's size: the first scanout's (QEMU: xres, yres).
    let info = gpu.control.request(&header(GET_DISPLAY_INFO), 24 + 16 * 24);
    if answer_type(&info) != RESP_OK_DISPLAY_INFO {
        return None;
    }
    let field = |i: usize| u32::from_le_bytes(info[24 + i * 4..28 + i * 4].try_into().unwrap());
    let (width, height) = (field(2), field(3));
    if width == 0 || height == 0 || width > 7680 || height > 4320 {
        return None;
    }
    gpu.width = width;
    gpu.height = height;

    // Its framebuffer: contiguous memory, as the console and mmap want.
    let len = (width as usize * height as usize * 4).next_multiple_of(4096);
    let first = crate::memory::FRAME_ALLOCATOR.lock().as_mut()?.allocate_contiguous(len / 4096)?;
    let phys = first.start_address().as_u64();
    let buf = crate::memory::phys_to_virt(phys) as *mut u8;
    unsafe { core::ptr::write_bytes(buf, 0, len) };

    let mut create = header(RESOURCE_CREATE_2D);
    for v in [SCREEN, FORMAT_B8G8R8X8, width, height] {
        push32(&mut create, v);
    }
    let mut attach = header(RESOURCE_ATTACH_BACKING);
    push32(&mut attach, SCREEN);
    push32(&mut attach, 1); // one entry: (address, length, padding)
    attach.extend_from_slice(&phys.to_le_bytes());
    push32(&mut attach, len as u32);
    push32(&mut attach, 0);
    let mut scanout = header(SET_SCANOUT);
    for v in [0, 0, width, height, 0, SCREEN] {
        push32(&mut scanout, v);
    }
    if !(gpu.command(&create) && gpu.command(&attach) && gpu.command(&scanout)) {
        return None;
    }
    // The console moves over (its text redrawn there), then the first picture.
    crate::vga_buffer::switch_framebuffer(buf, len, width as usize, height as usize);
    gpu.flush(0, 0, width, height);
    let _ = crate::vga_buffer::take_dirty();
    *GPU.lock() = Some(gpu);
    Some((width, height))
}

/// Is the screen on a virtio GPU?
pub fn present() -> bool {
    GPU.lock().is_some()
}

/// From the timer: hand the screen over if it may have changed.
pub fn tick() {
    let now = crate::process::ticks();
    if now.saturating_sub(LAST_FLUSH.load(Ordering::Relaxed)) < FLUSH_TICKS {
        return;
    }
    let Some(mut guard) = GPU.try_lock() else { return };
    let Some(gpu) = guard.as_mut() else { return };
    let changed = crate::vga_buffer::take_dirty() || crate::vga_buffer::is_graphics();
    if changed {
        LAST_FLUSH.store(now, Ordering::Relaxed);
        let (w, h) = (gpu.width, gpu.height);
        gpu.flush(0, 0, w, h);
    }
}

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
//! The mouse pointer can be the device's too (a hardware cursor: QEMU
//! draws it, as your own pointer over its window): a 64x64 ARGB resource
//! shown and moved through the cursor queue, for X through two /dev/fb0
//! ioctls (cursor_image, cursor_move).
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
const UPDATE_CURSOR: u32 = 0x0300;
const MOVE_CURSOR: u32 = 0x0301;
const RESP_OK_NODATA: u32 = 0x1100;
const RESP_OK_DISPLAY_INFO: u32 = 0x1101;

/// The pixel format: bytes B, G, R, unused (as the console draws them).
const FORMAT_B8G8R8X8: u32 = 2;
/// The screen's resource, and the cursor's.
const SCREEN: u32 = 1;
const CURSOR: u32 = 2;
/// The pixel format of cursor images: B, G, R, alpha bytes (ARGB words).
const FORMAT_B8G8R8A8: u32 = 1;
const CURSOR_SIZE: u32 = 64;
/// Hand the screen over every this many timer ticks (10 ms each).
const FLUSH_TICKS: u64 = 3;

struct Gpu {
    /// (Kept: its registers stay mapped and in use.)
    #[allow(dead_code)]
    dev: Device,
    control: Queue,
    width: u32,
    height: u32,
    cursor: Option<Cursor>,
}

/// The hardware cursor: its queue, image memory, and what's shown.
struct Cursor {
    queue: Queue,
    image: *mut u8,
    hot: (u32, u32),
    at: (i32, i32),
    /// Shown, and is the device's copy of the image up to date with that?
    visible: bool,
    new_image: bool,
}

unsafe impl Send for Cursor {}

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
    let cursor_queue = dev.queue(1, 16);
    let mut gpu = Gpu { dev, control, width: 0, height: 0, cursor: None };

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
    // The cursor: a 64x64 resource with its own memory (if there's a queue).
    if let Some(queue) = cursor_queue {
        let bytes = (CURSOR_SIZE * CURSOR_SIZE * 4) as usize;
        let mem = crate::memory::FRAME_ALLOCATOR.lock().as_mut().and_then(|a| a.allocate_contiguous(bytes / 4096));
        if let Some(first) = mem {
            let cphys = first.start_address().as_u64();
            let image = crate::memory::phys_to_virt(cphys) as *mut u8;
            unsafe { core::ptr::write_bytes(image, 0, bytes) };
            let mut create = header(RESOURCE_CREATE_2D);
            for v in [CURSOR, FORMAT_B8G8R8A8, CURSOR_SIZE, CURSOR_SIZE] {
                push32(&mut create, v);
            }
            let mut attach = header(RESOURCE_ATTACH_BACKING);
            push32(&mut attach, CURSOR);
            push32(&mut attach, 1);
            attach.extend_from_slice(&cphys.to_le_bytes());
            push32(&mut attach, bytes as u32);
            push32(&mut attach, 0);
            if gpu.command(&create) && gpu.command(&attach) {
                gpu.cursor = Some(Cursor { queue, image, hot: (0, 0), at: (0, 0), visible: false, new_image: false });
            }
        }
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

/// struct virtio_gpu_update_cursor: the header, where (scanout, x, y),
/// the resource (0: none, hidden) and its hot spot.
fn cursor_command(kind: u32, x: i32, y: i32, resource: u32, hot: (u32, u32)) -> Vec<u8> {
    let mut c = header(kind);
    for v in [0, x.max(0) as u32, y.max(0) as u32, 0, resource, hot.0, hot.1, 0] {
        push32(&mut c, v);
    }
    c
}

/// A new pointer image (64 x 64 ARGB words, `argb` as bytes) with its hot
/// spot. False if there's no hardware cursor.
pub fn cursor_image(hot_x: u32, hot_y: u32, argb: &[u8]) -> bool {
    let mut guard = GPU.lock();
    let Some(gpu) = guard.as_mut() else { return false };
    let Some(cursor) = gpu.cursor.as_mut() else { return false };
    let n = argb.len().min((CURSOR_SIZE * CURSOR_SIZE * 4) as usize);
    unsafe { core::ptr::copy_nonoverlapping(argb.as_ptr(), cursor.image, n) };
    cursor.hot = (hot_x.min(CURSOR_SIZE - 1), hot_y.min(CURSOR_SIZE - 1));
    cursor.new_image = true;
    // The device's copy of the resource from its memory.
    let mut t = header(TRANSFER_TO_HOST_2D);
    for v in [0, 0, CURSOR_SIZE, CURSOR_SIZE] {
        push32(&mut t, v);
    }
    t.extend_from_slice(&0u64.to_le_bytes());
    push32(&mut t, CURSOR);
    push32(&mut t, 0);
    gpu.command(&t);
    true
}

/// Move the pointer to (x, y) (its hot spot there), shown or hidden. False
/// if there's no hardware cursor.
pub fn cursor_move(x: i32, y: i32, visible: bool) -> bool {
    let mut guard = GPU.lock();
    let Some(gpu) = guard.as_mut() else { return false };
    let Some(cursor) = gpu.cursor.as_mut() else { return false };
    cursor.at = (x, y);
    let cmd = if !visible {
        if !cursor.visible {
            return true;
        }
        cursor_command(UPDATE_CURSOR, x, y, 0, (0, 0))
    } else if !cursor.visible || cursor.new_image {
        // (Shown again, or a new image: the whole cursor.)
        cursor.new_image = false;
        cursor_command(UPDATE_CURSOR, x, y, CURSOR, cursor.hot)
    } else {
        cursor_command(MOVE_CURSOR, x, y, CURSOR, cursor.hot)
    };
    cursor.visible = visible;
    cursor.queue.request(&cmd, 24);
    true
}

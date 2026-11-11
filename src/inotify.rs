// Copyright (c) 2026 tre4surehunter9
//! inotify: programs watch files and directories and read events when they
//! change (file watchers, `tail -f`, build tools, Node's fs.watch).
//!
//! An instance (an fd, see vfs) holds watches (a path and the events
//! wanted) and a queue of events, each a struct inotify_event ready to be
//! read. The VFS calls `changed` after operations that change files; when
//! nothing is watched that costs one atomic load.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use spin::Mutex;

pub const IN_ACCESS: u32 = 0x1;
pub const IN_MODIFY: u32 = 0x2;
pub const IN_ATTRIB: u32 = 0x4;
pub const IN_CLOSE_WRITE: u32 = 0x8;
pub const IN_CLOSE_NOWRITE: u32 = 0x10;
pub const IN_MOVED_FROM: u32 = 0x40;
pub const IN_MOVED_TO: u32 = 0x80;
pub const IN_CREATE: u32 = 0x100;
pub const IN_DELETE: u32 = 0x200;
pub const IN_DELETE_SELF: u32 = 0x400;
pub const IN_MOVE_SELF: u32 = 0x800;
const IN_Q_OVERFLOW: u32 = 0x4000;
const IN_IGNORED: u32 = 0x8000;
pub const IN_ONLYDIR: u32 = 0x0100_0000;
pub const IN_DONT_FOLLOW: u32 = 0x0200_0000;
const IN_MASK_ADD: u32 = 0x2000_0000;
const IN_ISDIR: u32 = 0x4000_0000;
const IN_ONESHOT: u32 = 0x8000_0000;
/// The events a watch can ask for.
const IN_ALL_EVENTS: u32 = 0xfff;
const MAX_QUEUED: usize = 16384;

struct Watch {
    wd: i32,
    path: String,
    mask: u32,
}

pub struct Instance {
    watches: Vec<Watch>,
    next_wd: i32,
    events: VecDeque<Vec<u8>>,
    overflowed: bool,
}

static INSTANCES: Mutex<Vec<Weak<Mutex<Instance>>>> = Mutex::new(Vec::new());
/// How many watches exist (none: `changed` returns at once).
static WATCHES: AtomicUsize = AtomicUsize::new(0);
static COOKIE: AtomicU32 = AtomicU32::new(1);

/// A cookie pairing IN_MOVED_FROM with IN_MOVED_TO.
pub fn next_cookie() -> u32 {
    COOKIE.fetch_add(1, Ordering::Relaxed)
}

pub fn new_instance() -> Arc<Mutex<Instance>> {
    let i = Arc::new(Mutex::new(Instance { watches: Vec::new(), next_wd: 1, events: VecDeque::new(), overflowed: false }));
    let mut all = INSTANCES.lock();
    all.retain(|w| w.strong_count() > 0);
    all.push(Arc::downgrade(&i));
    i
}

impl Drop for Instance {
    fn drop(&mut self) {
        WATCHES.fetch_sub(self.watches.len(), Ordering::Relaxed);
    }
}

/// The key a reader sleeps on.
pub fn key(i: &Arc<Mutex<Instance>>) -> u64 {
    0x7E7E_0000_0000_0000 ^ (Arc::as_ptr(i) as u64) ^ 0x1
}

impl Instance {
    fn queue(&mut self, wd: i32, mask: u32, cookie: u32, name: &str) {
        if self.events.len() >= MAX_QUEUED {
            if !self.overflowed {
                self.overflowed = true;
                self.events.push_back(encode(-1, IN_Q_OVERFLOW, 0, ""));
            }
            return;
        }
        let ev = encode(wd, mask, cookie, name);
        // Like Linux, an event identical to the last one queued is merged.
        if self.events.back() != Some(&ev) {
            self.events.push_back(ev);
        }
    }

    /// Add a watch on `path` (real, absolute), or change the existing one.
    pub fn add_watch(&mut self, path: String, mask: u32) -> i32 {
        if let Some(w) = self.watches.iter_mut().find(|w| w.path == path) {
            w.mask = if mask & IN_MASK_ADD != 0 { w.mask | mask } else { mask };
            return w.wd;
        }
        let wd = self.next_wd;
        self.next_wd += 1;
        self.watches.push(Watch { wd, path, mask });
        WATCHES.fetch_add(1, Ordering::Relaxed);
        wd
    }

    pub fn rm_watch(&mut self, wd: i32) -> bool {
        match self.watches.iter().position(|w| w.wd == wd) {
            Some(i) => {
                self.watches.remove(i);
                WATCHES.fetch_sub(1, Ordering::Relaxed);
                self.queue(wd, IN_IGNORED, 0, "");
                true
            }
            None => false,
        }
    }

    pub fn readable(&self) -> bool {
        !self.events.is_empty()
    }

    /// As many whole events as fit in `buf`: bytes copied, or None if not
    /// even the first fits.
    pub fn take(&mut self, buf: &mut [u8]) -> Option<usize> {
        let mut n = 0;
        while let Some(ev) = self.events.front() {
            if n + ev.len() > buf.len() {
                break;
            }
            buf[n..n + ev.len()].copy_from_slice(ev);
            n += ev.len();
            self.events.pop_front();
        }
        if self.events.is_empty() {
            self.overflowed = false;
        }
        (n > 0 || self.events.is_empty()).then_some(n)
    }
}

/// struct inotify_event { int wd; u32 mask, cookie, len; char name[len]; }:
/// the name NUL-padded to a multiple of 16 bytes (len 0 without a name).
fn encode(wd: i32, mask: u32, cookie: u32, name: &str) -> Vec<u8> {
    let len = if name.is_empty() { 0 } else { (name.len() + 1).next_multiple_of(16) };
    let mut ev = alloc::vec![0u8; 16 + len];
    ev[0..4].copy_from_slice(&wd.to_le_bytes());
    ev[4..8].copy_from_slice(&mask.to_le_bytes());
    ev[8..12].copy_from_slice(&cookie.to_le_bytes());
    ev[12..16].copy_from_slice(&(len as u32).to_le_bytes());
    ev[16..16 + name.len()].copy_from_slice(name.as_bytes());
    ev
}

/// `path` (absolute) changed: `mask` is one event (IN_CREATE, IN_MODIFY,
/// IN_DELETE, ...). Watches on its directory get it with the name;
/// watches on `path` itself get it too (IN_DELETE as IN_DELETE_SELF, then
/// IN_IGNORED; IN_MOVED_FROM as IN_MOVE_SELF).
pub fn changed(path: &str, mask: u32, is_dir: bool, cookie: u32) {
    if WATCHES.load(Ordering::Relaxed) == 0 {
        return;
    }
    let (dir, name) = match path.rfind('/') {
        Some(0) => ("/", &path[1..]),
        Some(i) => (&path[..i], &path[i + 1..]),
        None => return,
    };
    let isdir = if is_dir { IN_ISDIR } else { 0 };
    let self_mask = match mask {
        IN_DELETE => IN_DELETE_SELF,
        IN_MOVED_FROM => IN_MOVE_SELF,
        IN_CREATE | IN_MOVED_TO => 0,
        m => m,
    };
    let instances: Vec<Arc<Mutex<Instance>>> = INSTANCES.lock().iter().filter_map(Weak::upgrade).collect();
    for inst in instances {
        let mut woke = false;
        {
            let mut i = inst.lock();
            let mut fired = Vec::new();
            for w in &i.watches {
                if w.path == dir && w.mask & mask != 0 {
                    fired.push((w.wd, mask | isdir, cookie, String::from(name), w.mask & IN_ONESHOT != 0));
                }
                if self_mask != 0 && w.path == path && w.mask & self_mask & IN_ALL_EVENTS != 0 {
                    fired.push((w.wd, self_mask | if self_mask == IN_DELETE_SELF { 0 } else { isdir }, 0, String::new(), w.mask & IN_ONESHOT != 0));
                }
            }
            for (wd, m, c, n, oneshot) in fired {
                i.queue(wd, m, c, &n);
                woke = true;
                // A one-shot watch, or the watched file itself deleted: gone.
                if oneshot || m == IN_DELETE_SELF {
                    i.rm_watch(wd);
                }
            }
        }
        if woke {
            crate::process::wake(key(&inst));
        }
    }
}

// Copyright (c) 2026 tre4surehunter9
//! A folder shared by the host (QEMU's -virtfs / virtio-9p-pci): the 9P2000.L
//! file protocol over virtio. Mounted at /host: files there are the host's,
//! read and written as they are (QEMU does the work on the host side, as
//! the user running QEMU).
//!
//! 9P: the client names files by fids (numbers it picks), walks from one fid
//! to a new one by names, opens a fid to read and write it, and a fid is let
//! go with clunk. Each file has a qid: its path field identifies it, so it's
//! the inode number here. Every request is one message (size, type, tag,
//! fields) answered by one reply, or by Rlerror with a Linux errno.
//!
//! On virtio: one queue; each request is two buffers, the message (read by
//! the device) and room for the reply (written by it). The device's
//! configuration has its mount tag (QEMU's mount_tag=).
//!
//! Here: every inode looked up keeps a walk fid (ino -> fid); an open file
//! has one opened fid too, shared by its opens and let go at the last close.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use crate::mounts::{FileSystem, FileType, FsResult, FsStats, Ino, Meta};
use crate::virtio::{self, Device, Queue};

const MSIZE: usize = 64 * 1024;
const NOFID: u32 = 0xffff_ffff;
const EIO: i64 = 5;

// Message types (9P2000.L), T = request; the reply is T + 1.
const TLERROR: u8 = 6;
const TSTATFS: u8 = 8;
const TLOPEN: u8 = 12;
const TLCREATE: u8 = 14;
const TSYMLINK: u8 = 16;
const TREADLINK: u8 = 22;
const TGETATTR: u8 = 24;
const TSETATTR: u8 = 26;
const TREADDIR: u8 = 40;
const TFSYNC: u8 = 50;
const TLINK: u8 = 70;
const TMKDIR: u8 = 72;
const TRENAMEAT: u8 = 74;
const TUNLINKAT: u8 = 76;
const TVERSION: u8 = 100;
const TATTACH: u8 = 104;
const TWALK: u8 = 110;
const TREAD: u8 = 116;
const TWRITE: u8 = 118;
const TCLUNK: u8 = 120;

// Open flags (Linux's).
const O_RDONLY: u32 = 0;
const O_RDWR: u32 = 2;
const O_CREAT: u32 = 0o100;
const O_EXCL: u32 = 0o200;
const O_DIRECTORY: u32 = 0o200000;
const AT_REMOVEDIR: u32 = 0x200;

// Tsetattr's valid bits.
const SET_MODE: u32 = 1;
const SET_UID: u32 = 2;
const SET_GID: u32 = 4;
const SET_SIZE: u32 = 8;
const SET_MTIME: u32 = 0x20;
const SET_MTIME_SET: u32 = 0x100;

/// A message being built.
struct Msg(Vec<u8>);

impl Msg {
    fn new(ty: u8) -> Msg {
        let mut v = Vec::with_capacity(64);
        v.extend_from_slice(&[0, 0, 0, 0, ty, 1, 0]); // size (later), type, tag 1
        Msg(v)
    }
    fn u16(mut self, v: u16) -> Msg {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn u32(mut self, v: u32) -> Msg {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn u64(mut self, v: u64) -> Msg {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn s(mut self, v: &str) -> Msg {
        self.0.extend_from_slice(&(v.len() as u16).to_le_bytes());
        self.0.extend_from_slice(v.as_bytes());
        self
    }
    fn bytes(mut self, v: &[u8]) -> Msg {
        self.0.extend_from_slice(v);
        self
    }
}

/// A reply being read (after its 7-byte header).
struct Reply<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reply<'a> {
    fn take(&mut self, n: usize) -> &'a [u8] {
        let end = (self.at + n).min(self.b.len());
        let s = &self.b[self.at..end];
        self.at = end;
        s
    }
    fn u8(&mut self) -> u8 {
        self.take(1).first().copied().unwrap_or(0)
    }
    fn u16(&mut self) -> u16 {
        let s = self.take(2);
        if s.len() == 2 { u16::from_le_bytes([s[0], s[1]]) } else { 0 }
    }
    fn u32(&mut self) -> u32 {
        let s = self.take(4);
        if s.len() == 4 { u32::from_le_bytes(s.try_into().unwrap()) } else { 0 }
    }
    fn u64(&mut self) -> u64 {
        let s = self.take(8);
        if s.len() == 8 { u64::from_le_bytes(s.try_into().unwrap()) } else { 0 }
    }
    fn s(&mut self) -> String {
        let n = self.u16() as usize;
        String::from_utf8_lossy(self.take(n)).into_owned()
    }
    /// A qid: type, version, path (the path is what identifies the file).
    fn qid(&mut self) -> (u8, u64) {
        let ty = self.u8();
        self.u32();
        (ty, self.u64())
    }
}

/// The transport: the device and its request buffers.
struct Transport {
    _dev: Device,
    queue: Queue,
    req: (u64, *mut u8),
    resp: (u64, *mut u8),
}

unsafe impl Send for Transport {}

impl Transport {
    /// Send a message; the reply's body (after the header) or an errno.
    fn call(&mut self, m: Msg, want: u8) -> FsResult<Vec<u8>> {
        let mut v = m.0;
        if v.len() > MSIZE {
            return Err(EIO);
        }
        let len = v.len() as u32;
        v[..4].copy_from_slice(&len.to_le_bytes());
        unsafe { core::ptr::copy_nonoverlapping(v.as_ptr(), self.req.1, v.len()) };
        if !self.queue.request_chain(&[(self.req.0, len, false), (self.resp.0, MSIZE as u32, true)]) {
            return Err(EIO);
        }
        let hdr = unsafe { core::slice::from_raw_parts(self.resp.1, 7) };
        let size = (u32::from_le_bytes(hdr[..4].try_into().unwrap()) as usize).clamp(7, MSIZE);
        let ty = hdr[4];
        let body = unsafe { core::slice::from_raw_parts(self.resp.1.add(7), size - 7) }.to_vec();
        if ty == TLERROR + 1 {
            let errno = body.get(..4).map_or(EIO as u32, |b| u32::from_le_bytes(b.try_into().unwrap()));
            return Err(errno.max(1) as i64);
        }
        if ty != want {
            return Err(EIO);
        }
        Ok(body)
    }
}

pub struct NineP {
    t: Transport,
    tag: String,
    root: Ino,
    /// Each known inode's walk fid.
    fids: BTreeMap<Ino, u32>,
    /// Open inodes: their opened fid, whether it can write, and how many
    /// opens share it.
    open: BTreeMap<Ino, (u32, bool, usize)>,
    next_fid: u32,
}

impl NineP {
    fn fid(&mut self) -> u32 {
        self.next_fid += 1;
        self.next_fid
    }

    fn fid_of(&self, ino: Ino) -> FsResult<u32> {
        self.fids.get(&ino).copied().ok_or(crate::syscall::ENOENT)
    }

    fn clunk(&mut self, fid: u32) {
        let _ = self.t.call(Msg::new(TCLUNK).u32(fid), TCLUNK + 1);
    }

    /// A new fid for the same file as `fid` (a walk of no names).
    fn clone_fid(&mut self, fid: u32) -> FsResult<u32> {
        let new = self.fid();
        self.t.call(Msg::new(TWALK).u32(fid).u32(new).u16(0), TWALK + 1)?;
        Ok(new)
    }

    /// Open a clone of `ino`'s fid with `flags`.
    fn open_fid(&mut self, ino: Ino, flags: u32) -> FsResult<u32> {
        let fid = self.fid_of(ino)?;
        let new = self.clone_fid(fid)?;
        match self.t.call(Msg::new(TLOPEN).u32(new).u32(flags), TLOPEN + 1) {
            Ok(_) => Ok(new),
            Err(e) => {
                self.clunk(new);
                Err(e)
            }
        }
    }

    /// An opened fid for reading (`write`: for writing) `ino`, and whether
    /// to clunk it after (not one an open shares).
    fn io_fid(&mut self, ino: Ino, write: bool) -> FsResult<(u32, bool)> {
        if let Some(&(fid, w, _)) = self.open.get(&ino) {
            if w || !write {
                return Ok((fid, false));
            }
        }
        let fid = self.open_fid(ino, if write { O_RDWR } else { O_RDONLY })?;
        Ok((fid, true))
    }

    fn getattr(&mut self, ino: Ino) -> FsResult<Meta> {
        let fid = self.fid_of(ino)?;
        let body = self.t.call(Msg::new(TGETATTR).u32(fid).u64(0x7ff), TGETATTR + 1)?;
        let mut r = Reply { b: &body, at: 0 };
        r.u64(); // valid
        r.qid();
        let mode = r.u32();
        let uid = r.u32();
        let gid = r.u32();
        let nlink = r.u64();
        r.u64(); // rdev
        let size = r.u64();
        r.u64(); // blksize
        r.u64(); // blocks
        r.u64(); // atime
        r.u64();
        let mtime = r.u64();
        let kind = match mode & 0o170000 {
            0o040000 => FileType::Dir,
            0o120000 => FileType::Symlink,
            _ => FileType::File,
        };
        Ok(Meta { ino, kind, size, perm: mode & 0o7777, nlink: nlink.min(u32::MAX as u64) as u32, mtime, uid, gid })
    }

    /// Remember `fid` as `ino`'s walk fid, in place of any older one: the
    /// host may have reused the number for a new file (a file deleted, then
    /// another made: ext4 does at once), and the old fid is the old file's.
    fn keep(&mut self, ino: Ino, fid: u32) {
        if let Some(old) = self.fids.insert(ino, fid) {
            if old != fid && ino != self.root {
                self.clunk(old);
            }
        }
    }

    fn setattr(&mut self, ino: Ino, valid: u32, mode: u32, uid: u32, gid: u32, size: u64, mtime: u64) -> FsResult<()> {
        let fid = self.fid_of(ino)?;
        let m = Msg::new(TSETATTR).u32(fid).u32(valid).u32(mode).u32(uid).u32(gid).u64(size).u64(0).u64(0).u64(mtime).u64(0);
        self.t.call(m, TSETATTR + 1).map(|_| ())
    }
}

impl FileSystem for NineP {
    fn name(&self) -> &'static str {
        "9p"
    }

    fn root(&self) -> Ino {
        self.root
    }

    fn lookup(&mut self, dir: Ino, name: &str) -> FsResult<Ino> {
        let dfid = self.fid_of(dir)?;
        let new = self.fid();
        let body = self.t.call(Msg::new(TWALK).u32(dfid).u32(new).u16(1).s(name), TWALK + 1)?;
        let mut r = Reply { b: &body, at: 0 };
        if r.u16() != 1 {
            return Err(crate::syscall::ENOENT); // (the walk stopped short: the fid wasn't made)
        }
        let (_, ino) = r.qid();
        self.keep(ino, new);
        Ok(ino)
    }

    fn meta(&mut self, ino: Ino) -> FsResult<Meta> {
        self.getattr(ino)
    }

    fn read(&mut self, ino: Ino, offset: u64, buf: &mut [u8]) -> FsResult<usize> {
        let (fid, temporary) = self.io_fid(ino, false)?;
        let mut done = 0;
        let result = loop {
            if done == buf.len() {
                break Ok(done);
            }
            let want = (buf.len() - done).min(MSIZE - 64) as u32;
            match self.t.call(Msg::new(TREAD).u32(fid).u64(offset + done as u64).u32(want), TREAD + 1) {
                Ok(body) => {
                    let mut r = Reply { b: &body, at: 0 };
                    let n = (r.u32() as usize).min(want as usize);
                    let data = r.take(n);
                    buf[done..done + data.len()].copy_from_slice(data);
                    done += data.len();
                    if data.len() < want as usize {
                        break Ok(done); // the end of the file
                    }
                }
                Err(e) => break if done > 0 { Ok(done) } else { Err(e) },
            }
        };
        if temporary {
            self.clunk(fid);
        }
        result
    }

    fn write(&mut self, ino: Ino, offset: u64, data: &[u8]) -> FsResult<usize> {
        let (fid, temporary) = self.io_fid(ino, true)?;
        let mut done = 0;
        let result = loop {
            if done == data.len() {
                break Ok(done);
            }
            let chunk = &data[done..(done + MSIZE - 64).min(data.len())];
            let m = Msg::new(TWRITE).u32(fid).u64(offset + done as u64).u32(chunk.len() as u32).bytes(chunk);
            match self.t.call(m, TWRITE + 1) {
                Ok(body) => {
                    let n = Reply { b: &body, at: 0 }.u32() as usize;
                    if n == 0 {
                        break Ok(done);
                    }
                    done += n.min(chunk.len());
                }
                Err(e) => break if done > 0 { Ok(done) } else { Err(e) },
            }
        };
        if temporary {
            self.clunk(fid);
        }
        result
    }

    fn truncate(&mut self, ino: Ino, len: u64) -> FsResult<()> {
        self.setattr(ino, SET_SIZE, 0, 0, 0, len, 0)
    }

    fn readdir(&mut self, dir: Ino) -> FsResult<Vec<(String, Ino, FileType)>> {
        let fid = self.open_fid(dir, O_RDONLY | O_DIRECTORY)?;
        let mut out = Vec::new();
        let mut offset = 0u64;
        let result = loop {
            let body = match self.t.call(Msg::new(TREADDIR).u32(fid).u64(offset).u32((MSIZE - 64) as u32), TREADDIR + 1) {
                Ok(b) => b,
                Err(e) => break Err(e),
            };
            let mut r = Reply { b: &body, at: 0 };
            let count = r.u32() as usize;
            if count == 0 {
                break Ok(());
            }
            let end = (4 + count).min(body.len());
            while r.at < end {
                let (qty, path) = r.qid();
                offset = r.u64();
                r.u8(); // (the dirent type: the qid's says the same)
                let name = r.s();
                if name == "." || name == ".." {
                    continue;
                }
                let kind = if qty & 0x80 != 0 {
                    FileType::Dir
                } else if qty & 0x02 != 0 {
                    FileType::Symlink
                } else {
                    FileType::File
                };
                out.push((name, path, kind));
            }
        };
        self.clunk(fid);
        result.map(|_| out)
    }

    fn create(&mut self, dir: Ino, name: &str, kind: FileType, perm: u32) -> FsResult<Ino> {
        let dfid = self.fid_of(dir)?;
        match kind {
            FileType::Dir => {
                self.t.call(Msg::new(TMKDIR).u32(dfid).s(name).u32(perm & 0o7777).u32(0), TMKDIR + 1)?;
            }
            FileType::File => {
                // Tlcreate turns the fid it's given into the new, open file.
                let new = self.clone_fid(dfid)?;
                let m = Msg::new(TLCREATE).u32(new).s(name).u32(O_RDWR | O_CREAT | O_EXCL).u32(perm & 0o7777).u32(0);
                let r = self.t.call(m, TLCREATE + 1);
                self.clunk(new);
                r?;
            }
            _ => return Err(crate::syscall::EPERM),
        }
        self.lookup(dir, name)
    }

    fn remove(&mut self, dir: Ino, name: &str, is_dir: bool) -> FsResult<()> {
        let dfid = self.fid_of(dir)?;
        let flags = if is_dir { AT_REMOVEDIR } else { 0 };
        self.t.call(Msg::new(TUNLINKAT).u32(dfid).s(name).u32(flags), TUNLINKAT + 1).map(|_| ())
    }

    fn rename(&mut self, old_dir: Ino, old_name: &str, new_dir: Ino, new_name: &str) -> FsResult<()> {
        let (o, n) = (self.fid_of(old_dir)?, self.fid_of(new_dir)?);
        self.t.call(Msg::new(TRENAMEAT).u32(o).s(old_name).u32(n).s(new_name), TRENAMEAT + 1).map(|_| ())
    }

    fn link(&mut self, dir: Ino, name: &str, target: Ino) -> FsResult<()> {
        let (d, t) = (self.fid_of(dir)?, self.fid_of(target)?);
        self.t.call(Msg::new(TLINK).u32(d).u32(t).s(name), TLINK + 1).map(|_| ())
    }

    fn symlink(&mut self, dir: Ino, name: &str, target: &str) -> FsResult<Ino> {
        let d = self.fid_of(dir)?;
        self.t.call(Msg::new(TSYMLINK).u32(d).s(name).s(target).u32(0), TSYMLINK + 1)?;
        self.lookup(dir, name)
    }

    fn readlink(&mut self, ino: Ino) -> FsResult<String> {
        let fid = self.fid_of(ino)?;
        let body = self.t.call(Msg::new(TREADLINK).u32(fid), TREADLINK + 1)?;
        Ok(Reply { b: &body, at: 0 }.s())
    }

    fn chmod(&mut self, ino: Ino, perm: u32) -> FsResult<()> {
        self.setattr(ino, SET_MODE, perm & 0o7777, 0, 0, 0, 0)
    }

    fn chown(&mut self, ino: Ino, uid: u32, gid: u32) -> FsResult<()> {
        self.setattr(ino, SET_UID | SET_GID, 0, uid, gid, 0, 0)
    }

    fn set_mtime(&mut self, ino: Ino, mtime: u64) -> FsResult<()> {
        self.setattr(ino, SET_MTIME | SET_MTIME_SET, 0, 0, 0, 0, mtime)
    }

    fn statfs(&mut self) -> FsStats {
        let none = FsStats { block_size: 4096, blocks: 0, free_blocks: 0, files: 0, free_files: 0 };
        let Ok(fid) = self.fid_of(self.root) else { return none };
        let Ok(body) = self.t.call(Msg::new(TSTATFS).u32(fid), TSTATFS + 1) else { return none };
        let mut r = Reply { b: &body, at: 0 };
        r.u32(); // type
        let bsize = r.u32() as u64;
        let blocks = r.u64();
        r.u64(); // bfree
        let bavail = r.u64();
        let files = r.u64();
        let ffree = r.u64();
        FsStats { block_size: bsize.max(512), blocks, free_blocks: bavail, files, free_files: ffree }
    }

    fn open_inode(&mut self, ino: Ino) {
        if let Some(e) = self.open.get_mut(&ino) {
            e.2 += 1;
            return;
        }
        let is_dir = self.getattr(ino).is_ok_and(|m| m.kind == FileType::Dir);
        if is_dir {
            return; // (directories: readdir opens its own)
        }
        let opened = match self.open_fid(ino, O_RDWR) {
            Ok(fid) => Some((fid, true)),
            Err(_) => self.open_fid(ino, O_RDONLY).ok().map(|fid| (fid, false)),
        };
        if let Some((fid, w)) = opened {
            self.open.insert(ino, (fid, w, 1));
        }
    }

    fn close_inode(&mut self, ino: Ino) {
        if let Some(e) = self.open.get_mut(&ino) {
            e.2 -= 1;
            if e.2 == 0 {
                let fid = e.0;
                self.open.remove(&ino);
                self.clunk(fid);
            }
        }
    }

    fn sync(&mut self) -> FsResult<()> {
        let fids: Vec<u32> = self.open.values().filter(|e| e.1).map(|e| e.0).collect();
        for fid in fids {
            let _ = self.t.call(Msg::new(TFSYNC).u32(fid).u32(0), TFSYNC + 1);
        }
        Ok(())
    }
}

/// Set up the first virtio-9p device and connect: a filesystem for its
/// shared folder, and its mount tag.
pub fn init() -> Option<(NineP, String)> {
    let pci = virtio::find(9).into_iter().next()?;
    let (dev, _) = Device::new(pci, 1)?; // (VIRTIO_9P_MOUNT_TAG: the tag is in the configuration)
    dev.no_interrupts(); // (polled: each request is waited for)
    let queue = dev.queue(0, 8)?;
    let tag_len = (dev.config8(0) as usize | (dev.config8(1) as usize) << 8).min(64);
    let tag: String = (0..tag_len).map(|i| dev.config8(2 + i) as char).collect();
    let pages = MSIZE / 4096;
    let mut alloc = crate::memory::FRAME_ALLOCATOR.lock();
    let a = alloc.as_mut()?;
    let req = a.allocate_contiguous(pages)?.start_address().as_u64();
    let resp = a.allocate_contiguous(pages)?.start_address().as_u64();
    drop(alloc);
    dev.start();
    let t = Transport {
        _dev: dev,
        queue,
        req: (req, crate::memory::phys_to_virt(req) as *mut u8),
        resp: (resp, crate::memory::phys_to_virt(resp) as *mut u8),
    };
    let mut fs = NineP { t, tag: tag.clone(), root: 0, fids: BTreeMap::new(), open: BTreeMap::new(), next_fid: 0 };
    let body = fs.t.call(Msg::new(TVERSION).u32(MSIZE as u32).s("9P2000.L"), TVERSION + 1).ok()?;
    let mut r = Reply { b: &body, at: 0 };
    r.u32();
    if r.s() != "9P2000.L" {
        return None;
    }
    let root_fid = fs.fid();
    let body = fs.t.call(Msg::new(TATTACH).u32(root_fid).u32(NOFID).s("root").s("").u32(0), TATTACH + 1).ok()?;
    let (_, root) = Reply { b: &body, at: 0 }.qid();
    fs.root = root;
    fs.fids.insert(root, root_fid);
    Some((fs, tag))
}

impl NineP {
    /// Its mount tag.
    pub fn tag(&self) -> &str {
        &self.tag
    }
}

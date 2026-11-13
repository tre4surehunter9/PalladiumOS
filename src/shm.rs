// Copyright (c) 2026 tre4surehunter9
//! System V shared memory: shmget, shmat, shmdt, shmctl.
//!
//! A segment is an anonymous SharedMem (see mmap.rs) kept in a table under
//! its id and key, with its owner, mode and times. shmat maps it like a
//! shared mmap, so its pages come on first touch, fork gives the child the
//! same attachments, and exit and exec detach them. IPC_RMID takes the
//! segment out of the table (no new attachments): its memory goes away
//! when the last mapping does, as on Linux. X uses it (the MIT-SHM
//! extension: images passed through shared memory instead of the socket),
//! and so does Mesa drawing to X.

use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;

use crate::mmap::SharedMem;
use crate::process::{self, Cred};
use crate::syscall::{user_slice, user_slice_mut, EACCES, EEXIST, EFAULT, EINVAL, ENOENT, ENOSPC, EPERM};

const IPC_PRIVATE: i32 = 0;
const IPC_CREAT: u64 = 0o1000;
const IPC_EXCL: u64 = 0o2000;
const IPC_RMID: u64 = 0;
const IPC_SET: u64 = 1;
const IPC_STAT: u64 = 2;
const IPC_INFO: u64 = 3;
const SHM_STAT: u64 = 13;
const SHM_INFO: u64 = 14;
const SHM_STAT_ANY: u64 = 15;
/// (musl asks for the 64-bit structures with this bit.)
const IPC_64: u64 = 0x100;
const SHM_RDONLY: u64 = 0o10000;
const SHM_RND: u64 = 0o20000;
const SHM_REMAP: u64 = 0o40000;
const SHM_EXEC: u64 = 0o100000;
/// Marked for removal (shm_perm.mode, as Linux shows it), and the size
/// limits.
const SHM_DEST: u32 = 0o1000;
const SHMMAX: u64 = 1 << 32;
const SHMMNI: usize = 4096;

struct Segment {
    id: i32,
    key: i32,
    size: u64,
    mem: Arc<SharedMem>,
    uid: u32,
    gid: u32,
    cuid: u32,
    cgid: u32,
    mode: u32,
    cpid: i32,
    lpid: i32,
    atime: u64,
    dtime: u64,
    ctime: u64,
    /// IPC_RMID was asked: gone from the table once nothing is attached.
    removed: bool,
}

impl Segment {
    /// Mappings of it (the table holds one reference itself).
    fn nattch(&self) -> u64 {
        Arc::strong_count(&self.mem) as u64 - 1
    }

    /// What `cred` may do with it: read (4), write (2), execute (1) bits.
    fn granted(&self, cred: &Cred) -> u32 {
        if cred.euid == 0 {
            return 7;
        }
        (if cred.euid == self.uid || cred.euid == self.cuid {
            self.mode >> 6
        } else if cred.in_group(self.gid) || cred.in_group(self.cgid) {
            self.mode >> 3
        } else {
            self.mode
        }) & 7
    }

    /// May `cred` read (and write, if `write`) it?
    fn allowed(&self, cred: &Cred, write: bool) -> bool {
        let want = if write { 6 } else { 4 };
        self.granted(cred) & want == want
    }

    fn owner(&self, cred: &Cred) -> bool {
        cred.euid == 0 || cred.euid == self.uid || cred.euid == self.cuid
    }
}

struct Table {
    segments: Vec<Segment>,
    next_id: i32,
}

static TABLE: Mutex<Table> = Mutex::new(Table { segments: Vec::new(), next_id: 1 });

/// Segments marked for removal that nothing maps any more: gone.
fn reap(t: &mut Table) {
    t.segments.retain(|s| !(s.removed && s.nattch() == 0));
}

/// shmget(key, size, flags): the segment with `key` (made if IPC_CREAT and
/// there's none; IPC_PRIVATE: always a new one). Returns its id.
pub fn sys_shmget(key: u64, size: u64, flags: u64) -> i64 {
    let key = key as i32;
    let cred = process::current_cred();
    let mut t = TABLE.lock();
    reap(&mut t);
    if key != IPC_PRIVATE {
        if let Some(s) = t.segments.iter().find(|s| s.key == key && !s.removed) {
            if flags & IPC_CREAT != 0 && flags & IPC_EXCL != 0 {
                return -EEXIST;
            }
            if size > s.size {
                return -EINVAL;
            }
            // The mode bits asked for (any of owner, group, other) must be
            // granted, as Linux checks.
            let asked = ((flags >> 6) | (flags >> 3) | flags) as u32 & 7;
            if asked & !s.granted(&cred) != 0 {
                return -EACCES;
            }
            return s.id as i64;
        }
        if flags & IPC_CREAT == 0 {
            return -ENOENT;
        }
    }
    if size == 0 || size > SHMMAX {
        return -EINVAL;
    }
    if t.segments.len() >= SHMMNI {
        return -ENOSPC;
    }
    let id = t.next_id;
    t.next_id = if t.next_id == i32::MAX { 1 } else { t.next_id + 1 };
    t.segments.push(Segment {
        id,
        key,
        size,
        mem: crate::mmap::new_shared(),
        uid: cred.euid,
        gid: cred.egid,
        cuid: cred.euid,
        cgid: cred.egid,
        mode: (flags & 0o777) as u32,
        cpid: process::current_pid() as i32,
        lpid: 0,
        atime: 0,
        dtime: 0,
        ctime: crate::rtc::now(),
        removed: false,
    });
    id as i64
}

/// shmat(id, addr, flags): map the segment (at `addr`, or anywhere if 0).
pub fn sys_shmat(id: u64, addr: u64, flags: u64) -> i64 {
    let cred = process::current_cred();
    let (mem, size) = {
        let mut t = TABLE.lock();
        reap(&mut t);
        let Some(s) = t.segments.iter_mut().find(|s| s.id == id as i32 && !s.removed) else { return -EINVAL };
        if !s.allowed(&cred, flags & SHM_RDONLY == 0) {
            return -EACCES;
        }
        s.atime = crate::rtc::now();
        s.lpid = process::current_pid() as i32;
        (s.mem.clone(), s.size)
    };
    let addr = if flags & SHM_RND != 0 { addr & !0xfff } else { addr };
    if addr & 0xfff != 0 {
        return -EINVAL;
    }
    let prot = 1 | if flags & SHM_RDONLY == 0 { 2 } else { 0 } | if flags & SHM_EXEC != 0 { 4 } else { 0 };
    crate::mmap::attach(addr, size, prot, flags & SHM_REMAP != 0, mem)
}

/// shmdt(addr): unmap the segment attached at `addr`.
pub fn sys_shmdt(addr: u64) -> i64 {
    match crate::mmap::detach(addr) {
        Ok(mem) => {
            let mut t = TABLE.lock();
            if let Some(s) = t.segments.iter_mut().find(|s| Arc::ptr_eq(&s.mem, &mem)) {
                s.dtime = crate::rtc::now();
                s.lpid = process::current_pid() as i32;
            }
            drop(mem);
            reap(&mut t);
            0
        }
        Err(e) => e,
    }
}

/// struct shmid_ds (Linux's 64-bit layout, 112 bytes).
fn shmid_ds(s: &Segment, b: &mut [u8]) {
    b.fill(0);
    let mode = s.mode | if s.removed { SHM_DEST } else { 0 };
    for (i, v) in [s.key as u32, s.uid, s.gid, s.cuid, s.cgid, mode].iter().enumerate() {
        b[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    b[24..26].copy_from_slice(&((s.id as u32 / SHMMNI as u32) as u16).to_le_bytes()); // seq
    b[48..56].copy_from_slice(&s.size.to_le_bytes());
    b[56..64].copy_from_slice(&s.atime.to_le_bytes());
    b[64..72].copy_from_slice(&s.dtime.to_le_bytes());
    b[72..80].copy_from_slice(&s.ctime.to_le_bytes());
    b[80..84].copy_from_slice(&s.cpid.to_le_bytes());
    b[84..88].copy_from_slice(&s.lpid.to_le_bytes());
    b[88..96].copy_from_slice(&s.nattch().to_le_bytes());
}

/// shmctl(id, cmd, buf): IPC_STAT, IPC_SET, IPC_RMID, and IPC_INFO,
/// SHM_INFO, SHM_STAT for ipcs.
pub fn sys_shmctl(id: u64, cmd: u64, buf: u64) -> i64 {
    let cmd = cmd & !IPC_64;
    let cred = process::current_cred();
    let mut t = TABLE.lock();
    reap(&mut t);
    match cmd {
        IPC_INFO => {
            // struct shminfo: shmmax, shmmin, shmmni, shmseg, shmall.
            let Some(b) = (unsafe { user_slice_mut(buf, 72) }) else { return -EFAULT };
            b.fill(0);
            for (i, v) in [SHMMAX, 1, SHMMNI as u64, SHMMNI as u64, SHMMAX / 4096].iter().enumerate() {
                b[i * 8..i * 8 + 8].copy_from_slice(&v.to_le_bytes());
            }
            t.segments.len().saturating_sub(1) as i64
        }
        SHM_INFO => {
            // struct shm_info: used_ids, shm_tot (pages), shm_rss, shm_swp...
            let Some(b) = (unsafe { user_slice_mut(buf, 48) }) else { return -EFAULT };
            b.fill(0);
            b[0..4].copy_from_slice(&(t.segments.len() as i32).to_le_bytes());
            let pages: u64 = t.segments.iter().map(|s| s.size.div_ceil(4096)).sum();
            b[8..16].copy_from_slice(&pages.to_le_bytes());
            t.segments.len().saturating_sub(1) as i64
        }
        SHM_STAT | SHM_STAT_ANY => {
            // `id` is an index into the table here; returns the segment's id.
            let Some(s) = t.segments.get(id as usize) else { return -EINVAL };
            if cmd == SHM_STAT && !s.allowed(&cred, false) {
                return -EACCES;
            }
            let Some(b) = (unsafe { user_slice_mut(buf, 112) }) else { return -EFAULT };
            shmid_ds(s, b);
            s.id as i64
        }
        IPC_STAT | IPC_SET | IPC_RMID => {
            let Some(s) = t.segments.iter_mut().find(|s| s.id == id as i32) else { return -EINVAL };
            match cmd {
                IPC_STAT => {
                    if !s.allowed(&cred, false) {
                        return -EACCES;
                    }
                    let Some(b) = (unsafe { user_slice_mut(buf, 112) }) else { return -EFAULT };
                    shmid_ds(s, b);
                    0
                }
                IPC_SET => {
                    if !s.owner(&cred) {
                        return -EPERM;
                    }
                    let Some(b) = (unsafe { user_slice(buf, 112) }) else { return -EFAULT };
                    let word = |i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
                    s.uid = word(4);
                    s.gid = word(8);
                    s.mode = (s.mode & !0o777) | (word(20) & 0o777);
                    s.ctime = crate::rtc::now();
                    0
                }
                _ => {
                    if !s.owner(&cred) {
                        return -EPERM;
                    }
                    s.removed = true;
                    s.key = IPC_PRIVATE; // (its key can be used again at once)
                    s.ctime = crate::rtc::now();
                    reap(&mut t);
                    0
                }
            }
        }
        _ => -EINVAL,
    }
}

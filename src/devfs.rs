// Copyright (c) 2026 tre4surehunter9
//! /dev: device files. Opening one gives a device instead of a file:
//! /dev/null (discards writes, reads are end-of-file), /dev/zero (reads
//! zeros), /dev/random and /dev/urandom (random bytes, as getrandom),
//! /dev/console (the screen and keyboard), /dev/tty (the opener's
//! controlling terminal), /dev/ptmx (a new pseudo-terminal) and /dev/pts/N
//! (the pseudo-terminals in use, see pty.rs).

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::mounts::{Device, FileSystem, FileType, FsResult, Ino, Meta};
use crate::syscall::{EACCES, ENOENT, ENOTDIR, EPERM};

const ROOT: Ino = 1;
/// /dev/pts, and /dev/pts/N at PTS_BASE + N.
const PTS_DIR: Ino = 50;
/// /dev/shm: an empty directory, where the kernel (and `alpine`) mount an
/// in-memory filesystem for shared memory (shm_open, named semaphores).
const SHM_DIR: Ino = 51;
const PTS_BASE: Ino = 100;
const DEVICES: &[(&str, Device)] = &[
    ("console", Device::Console),
    ("null", Device::Null),
    ("ptmx", Device::Ptmx),
    ("random", Device::Random),
    ("tty", Device::Tty),
    ("urandom", Device::Random),
    ("zero", Device::Zero),
];

pub struct DevFs;

fn pts_number(ino: Ino) -> Option<u32> {
    (PTS_BASE..PTS_BASE + crate::pty::MAX_PTYS as Ino).contains(&ino).then(|| (ino - PTS_BASE) as u32)
}

impl FileSystem for DevFs {
    fn name(&self) -> &'static str {
        "devfs"
    }

    fn root(&self) -> Ino {
        ROOT
    }

    fn lookup(&mut self, dir: Ino, name: &str) -> FsResult<Ino> {
        match dir {
            ROOT if name == "pts" => Ok(PTS_DIR),
            ROOT if name == "shm" => Ok(SHM_DIR),
            ROOT => DEVICES.iter().position(|(n, _)| *n == name).map(|i| i as Ino + 2).ok_or(ENOENT),
            PTS_DIR => {
                let n: u32 = name.parse().map_err(|_| ENOENT)?;
                crate::pty::get(n).map(|_| PTS_BASE + n as Ino).ok_or(ENOENT)
            }
            _ => Err(ENOTDIR),
        }
    }

    fn meta(&mut self, ino: Ino) -> FsResult<Meta> {
        let (kind, perm, uid, gid) = if ino == ROOT || ino == PTS_DIR {
            (FileType::Dir, 0o755, 0, 0)
        } else if ino == SHM_DIR {
            (FileType::Dir, 0o1777, 0, 0)
        } else if let Some(n) = pts_number(ino) {
            let (uid, gid, mode) = crate::pty::owner(n).ok_or(ENOENT)?;
            (FileType::CharDevice(Device::Pts(n)), mode, uid, gid)
        } else {
            let (_, dev) = DEVICES.get((ino as usize).wrapping_sub(2)).ok_or(ENOENT)?;
            let perm = if *dev == Device::Console { 0o600 } else { 0o666 };
            (FileType::CharDevice(*dev), perm, 0, 0)
        };
        Ok(Meta { ino, kind, size: 0, perm, nlink: 1, mtime: 0, uid, gid })
    }

    // Device data never goes through the filesystem (see vfs.rs).
    fn read(&mut self, _ino: Ino, _offset: u64, _buf: &mut [u8]) -> FsResult<usize> {
        Err(EACCES)
    }

    fn write(&mut self, _ino: Ino, _offset: u64, _data: &[u8]) -> FsResult<usize> {
        Err(EACCES)
    }

    fn truncate(&mut self, _ino: Ino, _len: u64) -> FsResult<()> {
        Ok(()) // `> /dev/null` truncates: nothing to do
    }

    fn readdir(&mut self, dir: Ino) -> FsResult<Vec<(String, Ino, FileType)>> {
        match dir {
            ROOT => {
                let mut v: Vec<_> = DEVICES
                    .iter()
                    .enumerate()
                    .map(|(i, (n, d))| (n.to_string(), i as Ino + 2, FileType::CharDevice(*d)))
                    .collect();
                v.push(("pts".to_string(), PTS_DIR, FileType::Dir));
                v.push(("shm".to_string(), SHM_DIR, FileType::Dir));
                Ok(v)
            }
            SHM_DIR => Ok(Vec::new()),
            PTS_DIR => Ok(crate::pty::list()
                .into_iter()
                .map(|n| (format!("{}", n), PTS_BASE + n as Ino, FileType::CharDevice(Device::Pts(n))))
                .collect()),
            _ => Err(ENOTDIR),
        }
    }

    fn create(&mut self, _dir: Ino, _name: &str, _kind: FileType, _perm: u32) -> FsResult<Ino> {
        Err(EPERM)
    }

    fn remove(&mut self, _dir: Ino, _name: &str, _is_dir: bool) -> FsResult<()> {
        Err(EPERM)
    }

    /// A pseudo-terminal's owner and mode can change (grantpt, and servers
    /// giving a user their terminal).
    fn chmod(&mut self, ino: Ino, perm: u32) -> FsResult<()> {
        let n = pts_number(ino).ok_or(EPERM)?;
        crate::pty::set_owner(n, None, None, Some(perm)).then_some(()).ok_or(ENOENT)
    }

    fn chown(&mut self, ino: Ino, uid: u32, gid: u32) -> FsResult<()> {
        let n = pts_number(ino).ok_or(EPERM)?;
        let keep = u32::MAX; // chown(-1) leaves it
        let ok = crate::pty::set_owner(n, (uid != keep).then_some(uid), (gid != keep).then_some(gid), None);
        ok.then_some(()).ok_or(ENOENT)
    }
}

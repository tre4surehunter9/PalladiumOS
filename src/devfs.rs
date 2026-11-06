// Copyright (c) 2026 tre4surehunter9
//! /dev: device files. Opening one gives a device instead of a file:
//! /dev/null (discards writes, reads are end-of-file), /dev/zero (reads
//! zeros), /dev/random and /dev/urandom (random bytes, as getrandom), and
//! /dev/tty and /dev/console (the console).

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::mounts::{Device, FileSystem, FileType, FsResult, Ino, Meta};
use crate::syscall::{EACCES, ENOENT, ENOTDIR, EPERM};

const ROOT: Ino = 1;
const DEVICES: &[(&str, Device)] = &[
    ("console", Device::Tty),
    ("null", Device::Null),
    ("random", Device::Random),
    ("tty", Device::Tty),
    ("urandom", Device::Random),
    ("zero", Device::Zero),
];

pub struct DevFs;

impl FileSystem for DevFs {
    fn name(&self) -> &'static str {
        "devfs"
    }

    fn root(&self) -> Ino {
        ROOT
    }

    fn lookup(&mut self, dir: Ino, name: &str) -> FsResult<Ino> {
        if dir != ROOT {
            return Err(ENOTDIR);
        }
        DEVICES.iter().position(|(n, _)| *n == name).map(|i| i as Ino + 2).ok_or(ENOENT)
    }

    fn meta(&mut self, ino: Ino) -> FsResult<Meta> {
        let kind = if ino == ROOT {
            FileType::Dir
        } else {
            let (_, dev) = DEVICES.get((ino as usize).wrapping_sub(2)).ok_or(ENOENT)?;
            FileType::CharDevice(*dev)
        };
        let perm = if kind == FileType::Dir { 0o755 } else { 0o666 };
        Ok(Meta { ino, kind, size: 0, perm, nlink: 1, mtime: 0, uid: 0, gid: 0 })
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
        if dir != ROOT {
            return Err(ENOTDIR);
        }
        Ok(DEVICES.iter().enumerate().map(|(i, (n, d))| (n.to_string(), i as Ino + 2, FileType::CharDevice(*d))).collect())
    }

    fn create(&mut self, _dir: Ino, _name: &str, _kind: FileType, _perm: u32) -> FsResult<Ino> {
        Err(EPERM)
    }

    fn remove(&mut self, _dir: Ino, _name: &str, _is_dir: bool) -> FsResult<()> {
        Err(EPERM)
    }
}

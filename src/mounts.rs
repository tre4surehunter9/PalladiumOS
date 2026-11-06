// Copyright (c) 2026 tre4surehunter9
//! The VFS layer: the `FileSystem` interface and the mount table.
//!
//! Every filesystem (ext2 on disk, the in-memory filesystem, /dev) works
//! with inode numbers; this module turns absolute paths into
//! (mount, inode) pairs. A path belongs to the mount with the longest
//! matching prefix, e.g. with `/` and `/bin` mounted, `/bin/ls` is `ls` in
//! the `/bin` filesystem.

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use spin::Mutex;

use crate::syscall::{EBUSY, EEXIST, EINVAL, EISDIR, ENOTDIR};

pub type Ino = u64;
/// Errors are positive errno values.
pub type FsResult<T> = Result<T, i64>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Device {
    Null,
    Zero,
    Tty,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileType {
    File,
    Dir,
    CharDevice(Device),
}

#[derive(Clone, Copy, Debug)]
pub struct Meta {
    pub ino: Ino,
    pub kind: FileType,
    pub size: u64,
    /// Permission bits (e.g. 0o644).
    pub perm: u32,
    pub nlink: u32,
    /// Modification time (seconds since 1970).
    pub mtime: u64,
}

/// What every filesystem implements. Directory entries exclude "." and "..".
pub trait FileSystem: Send {
    fn name(&self) -> &'static str;
    fn root(&self) -> Ino;
    fn lookup(&mut self, dir: Ino, name: &str) -> FsResult<Ino>;
    fn meta(&mut self, ino: Ino) -> FsResult<Meta>;
    fn read(&mut self, ino: Ino, offset: u64, buf: &mut [u8]) -> FsResult<usize>;
    fn write(&mut self, ino: Ino, offset: u64, data: &[u8]) -> FsResult<usize>;
    fn truncate(&mut self, ino: Ino, len: u64) -> FsResult<()>;
    fn readdir(&mut self, dir: Ino) -> FsResult<Vec<(String, Ino, FileType)>>;
    /// Create a file or directory `name` in `dir`.
    fn create(&mut self, dir: Ino, name: &str, kind: FileType) -> FsResult<Ino>;
    /// unlink (`is_dir` false) or rmdir (`is_dir` true) `name` in `dir`.
    fn remove(&mut self, dir: Ino, name: &str, is_dir: bool) -> FsResult<()>;
    /// An open file now refers to `ino` / no longer does. A file removed
    /// while open is only freed when it's no longer open (POSIX).
    fn open_inode(&mut self, _ino: Ino) {}
    fn close_inode(&mut self, _ino: Ino) {}
    /// Write everything cached to the disk.
    fn sync(&mut self) -> FsResult<()> {
        Ok(())
    }
}

struct Mount {
    path: String,
    fs: Box<dyn FileSystem>,
}

static MOUNTS: Mutex<Vec<Mount>> = Mutex::new(Vec::new());

/// Mount `fs` at `path` (an absolute, normalized path).
pub fn mount(path: &str, fs: Box<dyn FileSystem>) {
    swap(path, fs);
}

/// Replace the filesystem mounted at `path` (keeping its slot), returning
/// the old one. Used by tests to put a different filesystem at "/".
pub fn swap(path: &str, fs: Box<dyn FileSystem>) -> Option<Box<dyn FileSystem>> {
    ensure_root();
    let mut mounts = MOUNTS.lock();
    match mounts.iter_mut().find(|m| m.path == path) {
        Some(m) => Some(core::mem::replace(&mut m.fs, fs)),
        None => {
            mounts.push(Mount { path: path.to_string(), fs });
            None
        }
    }
}

/// Remove the mount at `path` (not "/"). Its slot is kept but unused, so
/// other mounts keep their numbers.
pub fn unmount(path: &str) -> FsResult<Box<dyn FileSystem>> {
    if path == "/" {
        return Err(EBUSY);
    }
    let mut mounts = MOUNTS.lock();
    let m = mounts.iter_mut().find(|m| m.path == path).ok_or(EINVAL)?;
    m.path = alloc::format!("\0unmounted:{}", path); // never matches a path
    Ok(core::mem::replace(&mut m.fs, Box::new(crate::fdfs::Fdfs::new())))
}

/// (mount path, filesystem name) for each mount, like /proc/mounts.
pub fn list() -> Vec<(String, &'static str)> {
    ensure_root();
    MOUNTS.lock().iter().filter(|m| m.path.starts_with('/')).map(|m| (m.path.clone(), m.fs.name())).collect()
}

/// If nothing is mounted at "/" yet, use an empty in-memory filesystem
/// (the test kernels don't set anything up).
fn ensure_root() {
    let mut mounts = MOUNTS.lock();
    if !mounts.iter().any(|m| m.path == "/") {
        mounts.push(Mount { path: "/".to_string(), fs: Box::new(crate::fdfs::Fdfs::new()) });
    }
}

/// Which mount `abs` is in, and the path inside it ("" for its root).
fn find_mount<'a>(mounts: &[Mount], abs: &'a str) -> (usize, &'a str) {
    let mut best: Option<(usize, usize)> = None;
    for (i, m) in mounts.iter().enumerate() {
        if !m.path.starts_with('/') {
            continue; // unmounted
        }
        let inside = if m.path == "/" {
            Some(abs.trim_start_matches('/'))
        } else if abs == m.path {
            Some("")
        } else {
            abs.strip_prefix(m.path.as_str()).and_then(|r| r.strip_prefix('/'))
        };
        if inside.is_some() && best.is_none_or(|(_, len)| m.path.len() > len) {
            best = Some((i, m.path.len()));
        }
    }
    let (i, _) = best.expect("no root filesystem mounted");
    let m = &mounts[i];
    let rest = if m.path == "/" { abs.trim_start_matches('/') } else { abs[m.path.len()..].trim_start_matches('/') };
    (i, rest)
}

/// A filesystem location: mount index + inode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Loc {
    pub mount: usize,
    pub ino: Ino,
}

fn walk(fs: &mut dyn FileSystem, rel: &str) -> FsResult<Ino> {
    let mut ino = fs.root();
    for comp in rel.split('/').filter(|c| !c.is_empty()) {
        if fs.meta(ino)?.kind != FileType::Dir {
            return Err(ENOTDIR);
        }
        ino = fs.lookup(ino, comp)?;
    }
    Ok(ino)
}

/// Run `f` with the filesystem holding `loc`.
pub fn with_fs<T>(mount: usize, f: impl FnOnce(&mut dyn FileSystem) -> T) -> T {
    let mut mounts = MOUNTS.lock();
    f(mounts[mount].fs.as_mut())
}

/// Resolve an absolute, normalized path.
pub fn resolve(abs: &str) -> FsResult<Loc> {
    ensure_root();
    let mut mounts = MOUNTS.lock();
    let (i, rel) = find_mount(&mounts, abs);
    let ino = walk(mounts[i].fs.as_mut(), rel)?;
    Ok(Loc { mount: i, ino })
}

/// Resolve and stat.
pub fn stat(abs: &str) -> FsResult<(Loc, Meta)> {
    let loc = resolve(abs)?;
    let meta = with_fs(loc.mount, |fs| fs.meta(loc.ino))?;
    Ok((loc, meta))
}

/// Split "/a/b/c" into the parent directory's location and "c". Fails for
/// "/" and for mount points (which can't be created or removed).
fn parent_of(abs: &str) -> FsResult<(Loc, String)> {
    ensure_root();
    if abs == "/" {
        return Err(EBUSY);
    }
    let (parent, name) = match abs.rfind('/') {
        Some(0) => ("/", &abs[1..]),
        Some(i) => (&abs[..i], &abs[i + 1..]),
        None => return Err(EINVAL),
    };
    if name.is_empty() || name == "." || name == ".." {
        return Err(EINVAL);
    }
    {
        let mounts = MOUNTS.lock();
        if mounts.iter().any(|m| m.path == abs) {
            return Err(EBUSY);
        }
    }
    let loc = resolve(parent)?;
    let kind = with_fs(loc.mount, |fs| fs.meta(loc.ino))?.kind;
    if kind != FileType::Dir {
        return Err(ENOTDIR);
    }
    Ok((loc, name.to_string()))
}

/// Create a file or directory at `abs`.
pub fn create(abs: &str, kind: FileType) -> FsResult<Loc> {
    if resolve(abs).is_ok() {
        return Err(EEXIST);
    }
    let (dir, name) = parent_of(abs).map_err(|e| if e == EBUSY { EEXIST } else { e })?;
    if name.len() > 255 {
        return Err(crate::syscall::ENAMETOOLONG);
    }
    let ino = with_fs(dir.mount, |fs| fs.create(dir.ino, &name, kind))?;
    Ok(Loc { mount: dir.mount, ino })
}

/// unlink or rmdir `abs`.
pub fn remove(abs: &str, is_dir: bool) -> FsResult<()> {
    let (dir, name) = match parent_of(abs) {
        Err(EBUSY) => return Err(if is_dir { EBUSY } else { EISDIR }),
        r => r?,
    };
    with_fs(dir.mount, |fs| {
        let ino = fs.lookup(dir.ino, &name)?;
        let kind = fs.meta(ino)?.kind;
        match (kind == FileType::Dir, is_dir) {
            (true, false) => Err(EISDIR),
            (false, true) => Err(ENOTDIR),
            _ => fs.remove(dir.ino, &name, is_dir),
        }
    })
}

/// Flush every filesystem (sync(2), and before reboot/power off).
pub fn sync_all() {
    let mut mounts = MOUNTS.lock();
    for m in mounts.iter_mut() {
        if let Err(e) = m.fs.sync() {
            crate::println!("sync {}: error {}", m.path, e);
        }
    }
}

/// Is there a directory at `abs`?
pub fn is_dir(abs: &str) -> bool {
    stat(abs).is_ok_and(|(_, m)| m.kind == FileType::Dir)
}

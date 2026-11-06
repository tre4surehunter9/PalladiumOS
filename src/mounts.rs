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

use crate::perm;
use crate::syscall::{EBUSY, EEXIST, EINVAL, EISDIR, ENOENT, ENOTDIR, EPERM};

pub const EXDEV: i64 = 18;
pub const ELOOP: i64 = 40;
/// How many symlinks one path lookup may follow (like Linux).
const MAX_SYMLINKS: usize = 40;

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
    Symlink,
    CharDevice(Device),
}

/// Filesystem usage, for statfs/df.
#[derive(Clone, Copy, Debug, Default)]
pub struct FsStats {
    pub block_size: u64,
    pub blocks: u64,
    pub free_blocks: u64,
    pub files: u64,
    pub free_files: u64,
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
    /// Owner and group.
    pub uid: u32,
    pub gid: u32,
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
    /// Create a file or directory `name` in `dir` with permissions `perm`.
    fn create(&mut self, dir: Ino, name: &str, kind: FileType, perm: u32) -> FsResult<Ino>;
    /// unlink (`is_dir` false) or rmdir (`is_dir` true) `name` in `dir`.
    fn remove(&mut self, dir: Ino, name: &str, is_dir: bool) -> FsResult<()>;
    /// Move `old_dir/old_name` to `new_dir/new_name`, replacing a file (or
    /// empty directory) already there. The VFS has checked it's not a
    /// directory moving into itself.
    fn rename(&mut self, _old_dir: Ino, _old_name: &str, _new_dir: Ino, _new_name: &str) -> FsResult<()> {
        Err(EPERM)
    }
    /// A hard link: another name for the file `target`.
    fn link(&mut self, _dir: Ino, _name: &str, _target: Ino) -> FsResult<()> {
        Err(EPERM)
    }
    fn symlink(&mut self, _dir: Ino, _name: &str, _target: &str) -> FsResult<Ino> {
        Err(EPERM)
    }
    fn readlink(&mut self, _ino: Ino) -> FsResult<String> {
        Err(EINVAL)
    }
    fn chmod(&mut self, _ino: Ino, _perm: u32) -> FsResult<()> {
        Err(EPERM)
    }
    fn chown(&mut self, _ino: Ino, _uid: u32, _gid: u32) -> FsResult<()> {
        Err(EPERM)
    }
    /// Set the modification time (seconds since 1970).
    fn set_mtime(&mut self, _ino: Ino, _mtime: u64) -> FsResult<()> {
        Ok(())
    }
    fn statfs(&mut self) -> FsStats {
        FsStats::default()
    }
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
/// (path, filesystem name) of each mount, kept separately so code running
/// inside a filesystem (like /proc/mounts) can read it without the
/// MOUNTS lock, which is held during every filesystem call.
static MOUNT_LIST: Mutex<Vec<(String, &'static str)>> = Mutex::new(Vec::new());

fn update_list(mounts: &[Mount]) {
    *MOUNT_LIST.lock() =
        mounts.iter().filter(|m| m.path.starts_with('/')).map(|m| (m.path.clone(), m.fs.name())).collect();
}

/// Mount `fs` at `path` (an absolute, normalized path).
pub fn mount(path: &str, fs: Box<dyn FileSystem>) {
    swap(path, fs);
}

/// Replace the filesystem mounted at `path` (keeping its slot), returning
/// the old one. Used by tests to put a different filesystem at "/".
pub fn swap(path: &str, fs: Box<dyn FileSystem>) -> Option<Box<dyn FileSystem>> {
    ensure_root();
    let mut mounts = MOUNTS.lock();
    let old = match mounts.iter_mut().find(|m| m.path == path) {
        Some(m) => Some(core::mem::replace(&mut m.fs, fs)),
        None => {
            mounts.push(Mount { path: path.to_string(), fs });
            None
        }
    };
    update_list(&mounts);
    old
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
    let old = core::mem::replace(&mut m.fs, Box::new(crate::fdfs::Fdfs::new()));
    update_list(&mounts);
    Ok(old)
}

/// (mount path, filesystem name) for each mount, like /proc/mounts.
pub fn list() -> Vec<(String, &'static str)> {
    ensure_root();
    MOUNT_LIST.lock().clone()
}

/// The mount list without touching the MOUNTS lock (for /proc/mounts,
/// which is generated while it's held).
pub fn list_unlocked() -> Vec<(String, &'static str)> {
    MOUNT_LIST.lock().clone()
}

/// If nothing is mounted at "/" yet, use an empty in-memory filesystem
/// (the test kernels don't set anything up).
fn ensure_root() {
    let mut mounts = MOUNTS.lock();
    if !mounts.iter().any(|m| m.path == "/") {
        mounts.push(Mount { path: "/".to_string(), fs: Box::new(crate::fdfs::Fdfs::new()) });
        update_list(&mounts);
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

/// Look up a path that contains no symlinks.
fn resolve_plain(abs: &str) -> FsResult<(Loc, Meta)> {
    let mut mounts = MOUNTS.lock();
    let (i, rel) = find_mount(&mounts, abs);
    let fs = mounts[i].fs.as_mut();
    let ino = walk(fs, rel)?;
    let meta = fs.meta(ino)?;
    Ok((Loc { mount: i, ino }, meta))
}

fn join(dir: &str, name: &str) -> String {
    if dir == "/" { alloc::format!("/{}", name) } else { alloc::format!("{}/{}", dir, name) }
}

/// Resolve `abs`, following symlinks in every component (and in the last
/// one too if `follow_last`). Returns the location, its metadata and the
/// path with all symlinks replaced (the "real" path).
pub fn resolve_full(abs: &str, follow_last: bool) -> FsResult<(Loc, Meta, String)> {
    ensure_root();
    let mut path = String::from(abs);
    let mut followed = 0;
    'restart: loop {
        let comps: Vec<String> = path.split('/').filter(|c| !c.is_empty()).map(String::from).collect();
        let mut prefix = String::from("/");
        let (mut loc, mut meta) = resolve_plain("/")?;
        let cred = crate::process::current_cred();
        for (i, comp) in comps.iter().enumerate() {
            if meta.kind != FileType::Dir {
                return Err(ENOTDIR);
            }
            // Using a directory in a path needs search (x) permission.
            perm::check(&meta, &cred, perm::EXEC)?;
            let candidate = join(&prefix, comp);
            let (l, m) = resolve_plain(&candidate)?;
            let last = i + 1 == comps.len();
            if m.kind == FileType::Symlink && (!last || follow_last) {
                followed += 1;
                if followed > MAX_SYMLINKS {
                    return Err(ELOOP);
                }
                let target = with_fs(l.mount, |fs| fs.readlink(l.ino))?;
                let rest = comps[i + 1..].join("/");
                let base = if target.starts_with('/') { target } else { join(&prefix, &target) };
                let joined = if rest.is_empty() { base } else { join(&base, &rest) };
                path = crate::filesystem::normalize("/", &joined);
                continue 'restart;
            }
            prefix = candidate;
            loc = l;
            meta = m;
        }
        return Ok((loc, meta, prefix));
    }
}

/// Resolve an absolute, normalized path, following symlinks.
pub fn resolve(abs: &str) -> FsResult<Loc> {
    resolve_full(abs, true).map(|(loc, _, _)| loc)
}

/// Resolve and stat, following symlinks (stat).
pub fn stat(abs: &str) -> FsResult<(Loc, Meta)> {
    resolve_full(abs, true).map(|(loc, meta, _)| (loc, meta))
}

/// Resolve and stat without following a final symlink (lstat).
pub fn lstat(abs: &str) -> FsResult<(Loc, Meta)> {
    resolve_full(abs, false).map(|(loc, meta, _)| (loc, meta))
}

/// Split "/a/b/c" into the parent directory (symlinks followed) and "c".
/// Fails for "/" and for mount points (which can't be created or removed).
fn parent_of(abs: &str) -> FsResult<(Loc, String, String)> {
    parent_full(abs).map(|(loc, _, name, real)| (loc, name, real))
}

/// parent_of, also returning the parent directory's metadata.
fn parent_full(abs: &str) -> FsResult<(Loc, Meta, String, String)> {
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
    let (loc, meta, real_parent) = resolve_full(parent, true)?;
    if meta.kind != FileType::Dir {
        return Err(ENOTDIR);
    }
    let real = join(&real_parent, name);
    {
        let mounts = MOUNTS.lock();
        if mounts.iter().any(|m| m.path == real) {
            return Err(EBUSY);
        }
    }
    Ok((loc, meta, name.to_string(), real))
}

/// Changing a directory's entries needs write and search permission on it.
fn check_dir_writable(dir: &Meta) -> FsResult<()> {
    perm::check(dir, &crate::process::current_cred(), perm::WRITE | perm::EXEC)
}

/// Give a new file to its creator: the effective uid, and the effective
/// gid (or the directory's group, if the directory has the setgid bit).
fn set_new_owner(loc: Loc, dir: &Meta) {
    let cred = crate::process::current_cred();
    let gid = if dir.perm & perm::S_ISGID != 0 { dir.gid } else { cred.egid };
    if cred.euid != 0 || gid != 0 {
        let _ = with_fs(loc.mount, |fs| fs.chown(loc.ino, cred.euid, gid));
    }
}

/// Create a file or directory at `abs` with permissions `perm`.
pub fn create(abs: &str, kind: FileType, perm: u32) -> FsResult<Loc> {
    if lstat(abs).is_ok() {
        return Err(EEXIST);
    }
    let (dir, dir_meta, name, _) = parent_full(abs).map_err(|e| if e == EBUSY { EEXIST } else { e })?;
    if name.len() > 255 {
        return Err(crate::syscall::ENAMETOOLONG);
    }
    check_dir_writable(&dir_meta)?;
    // New directories inherit a setgid bit (so files in them keep the group).
    let perm = if kind == FileType::Dir { perm | (dir_meta.perm & perm::S_ISGID) } else { perm };
    let ino = with_fs(dir.mount, |fs| fs.create(dir.ino, &name, kind, perm))?;
    let loc = Loc { mount: dir.mount, ino };
    set_new_owner(loc, &dir_meta);
    Ok(loc)
}

/// symlink(target, abs)
pub fn symlink(target: &str, abs: &str) -> FsResult<()> {
    if target.is_empty() {
        return Err(ENOENT);
    }
    if lstat(abs).is_ok() {
        return Err(EEXIST);
    }
    let (dir, dir_meta, name, _) = parent_full(abs).map_err(|e| if e == EBUSY { EEXIST } else { e })?;
    check_dir_writable(&dir_meta)?;
    let ino = with_fs(dir.mount, |fs| fs.symlink(dir.ino, &name, target))?;
    set_new_owner(Loc { mount: dir.mount, ino }, &dir_meta);
    Ok(())
}

/// link(existing, new): another name for a file (same filesystem only).
pub fn link(existing: &str, new: &str) -> FsResult<()> {
    let (src, meta) = lstat(existing)?;
    if meta.kind == FileType::Dir {
        return Err(EPERM); // no hard links to directories
    }
    if lstat(new).is_ok() {
        return Err(EEXIST);
    }
    let (dir, dir_meta, name, _) = parent_full(new).map_err(|e| if e == EBUSY { EEXIST } else { e })?;
    if dir.mount != src.mount {
        return Err(EXDEV);
    }
    check_dir_writable(&dir_meta)?;
    with_fs(dir.mount, |fs| fs.link(dir.ino, &name, src.ino))
}

/// rename(old, new)
pub fn rename(old: &str, new: &str) -> FsResult<()> {
    let (src_dir, src_dir_meta, src_name, src_real) = parent_full(old)?;
    let (src, src_meta) = lstat(old)?;
    let (dst_dir, dst_dir_meta, dst_name, dst_real) = parent_full(new)?;
    let cred = crate::process::current_cred();
    check_dir_writable(&src_dir_meta)?;
    check_dir_writable(&dst_dir_meta)?;
    perm::check_sticky(&src_dir_meta, &src_meta, &cred)?;
    if let Ok((_, existing)) = lstat(new) {
        perm::check_sticky(&dst_dir_meta, &existing, &cred)?;
    }
    if src_dir.mount != dst_dir.mount {
        return Err(EXDEV);
    }
    if src_real == dst_real {
        return Ok(());
    }
    if src_meta.kind == FileType::Dir && dst_real.starts_with(&alloc::format!("{}/", src_real)) {
        return Err(EINVAL); // a directory can't move into itself
    }
    if let Ok((dst, dst_meta)) = lstat(new) {
        if dst.ino == src.ino {
            return Ok(()); // two names for the same file: nothing to do
        }
        match (src_meta.kind == FileType::Dir, dst_meta.kind == FileType::Dir) {
            (true, false) => return Err(ENOTDIR),
            (false, true) => return Err(EISDIR),
            _ => {}
        }
    }
    with_fs(src_dir.mount, |fs| fs.rename(src_dir.ino, &src_name, dst_dir.ino, &dst_name))
}

/// unlink or rmdir `abs`.
pub fn remove(abs: &str, is_dir: bool) -> FsResult<()> {
    let (dir, dir_meta, name, _) = match parent_full(abs) {
        Err(EBUSY) => return Err(if is_dir { EBUSY } else { EISDIR }),
        r => r?,
    };
    check_dir_writable(&dir_meta)?;
    let cred = crate::process::current_cred();
    with_fs(dir.mount, |fs| {
        if let Ok(target) = fs.lookup(dir.ino, &name).and_then(|i| fs.meta(i)) {
            perm::check_sticky(&dir_meta, &target, &cred)?;
        }
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

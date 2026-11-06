// Copyright (c) 2026 tre4surehunter9
//! File API: thin wrappers over the file syscalls, plus helpers.
//! Functions return the raw result: >= 0 on success, -errno on failure.

use crate::sys::{self, syscall3};

// open() flags
pub const O_RDONLY: u64 = 0o0;
pub const O_WRONLY: u64 = 0o1;
pub const O_RDWR: u64 = 0o2;
pub const O_CREAT: u64 = 0o100;
pub const O_EXCL: u64 = 0o200;
pub const O_TRUNC: u64 = 0o1000;
pub const O_APPEND: u64 = 0o2000;
pub const O_DIRECTORY: u64 = 0o200000;

pub const AT_FDCWD: i32 = -100;

pub const SEEK_SET: u64 = 0;
pub const SEEK_CUR: u64 = 1;
pub const SEEK_END: u64 = 2;

// errno values
pub const ENOENT: isize = 2;
pub const EIO: isize = 5;
pub const EBADF: isize = 9;
pub const EFAULT: isize = 14;
pub const EBUSY: isize = 16;
pub const EEXIST: isize = 17;
pub const ENOTDIR: isize = 20;
pub const EISDIR: isize = 21;
pub const EINVAL: isize = 22;
pub const EMFILE: isize = 24;
pub const EFBIG: isize = 27;
pub const ESPIPE: isize = 29;
pub const ERANGE: isize = 34;
pub const ENAMETOOLONG: isize = 36;
pub const ENOSYS: isize = 38;
pub const ENOTEMPTY: isize = 39;

/// Human-readable message for a (positive) errno, like strerror().
pub fn strerror(errno: isize) -> &'static str {
    match errno {
        ENOENT => "No such file or directory",
        EIO => "Input/output error",
        EBADF => "Bad file descriptor",
        EFAULT => "Bad address",
        EBUSY => "Device or resource busy",
        EEXIST => "File exists",
        ENOTDIR => "Not a directory",
        EISDIR => "Is a directory",
        EINVAL => "Invalid argument",
        EMFILE => "Too many open files",
        EFBIG => "File too large",
        ESPIPE => "Illegal seek",
        ERANGE => "Result too large",
        ENAMETOOLONG => "File name too long",
        ENOSYS => "Function not implemented",
        ENOTEMPTY => "Directory not empty",
        _ => "Unknown error",
    }
}

/// Run `f` with `path` as a NUL-terminated string (no allocator needed).
fn with_cstr(path: &str, f: impl FnOnce(u64) -> i64) -> isize {
    let mut buf = [0u8; 4097];
    if path.len() >= buf.len() {
        return -ENAMETOOLONG;
    }
    if path.as_bytes().contains(&0) {
        return -EINVAL;
    }
    buf[..path.len()].copy_from_slice(path.as_bytes());
    f(buf.as_ptr() as u64) as isize
}

pub fn open(path: &str, flags: u64) -> isize {
    with_cstr(path, |p| unsafe { syscall3(sys::OPEN, p, flags, 0o644) })
}

pub fn openat(dirfd: i32, path: &str, flags: u64) -> isize {
    with_cstr(path, |p| unsafe { syscall3(sys::OPENAT, dirfd as i64 as u64, p, flags) })
}

pub fn close(fd: i32) -> isize {
    unsafe { syscall3(sys::CLOSE, fd as u64, 0, 0) as isize }
}

pub fn lseek(fd: i32, offset: i64, whence: u64) -> isize {
    unsafe { syscall3(sys::LSEEK, fd as u64, offset as u64, whence) as isize }
}

pub fn mkdir(path: &str) -> isize {
    with_cstr(path, |p| unsafe { syscall3(sys::MKDIR, p, 0o755, 0) })
}

pub fn rmdir(path: &str) -> isize {
    with_cstr(path, |p| unsafe { syscall3(sys::RMDIR, p, 0, 0) })
}

pub fn unlink(path: &str) -> isize {
    with_cstr(path, |p| unsafe { syscall3(sys::UNLINK, p, 0, 0) })
}

pub fn chdir(path: &str) -> isize {
    with_cstr(path, |p| unsafe { syscall3(sys::CHDIR, p, 0, 0) })
}

/// getcwd into `buf`; returns the path as &str, or -errno.
pub fn getcwd(buf: &mut [u8]) -> Result<&str, isize> {
    let r = unsafe { syscall3(sys::GETCWD, buf.as_mut_ptr() as u64, buf.len() as u64, 0) } as isize;
    if r < 0 {
        return Err(r);
    }
    core::str::from_utf8(&buf[..r as usize - 1]).map_err(|_| -EINVAL)
}

// ---- stat ----

pub const S_IFMT: u32 = 0o170000;
pub const S_IFCHR: u32 = 0o020000;
pub const S_IFDIR: u32 = 0o040000;
pub const S_IFREG: u32 = 0o100000;

/// struct stat (Linux x86_64 layout).
#[repr(C)]
#[derive(Default)]
pub struct Stat {
    pub st_dev: u64,
    pub st_ino: u64,
    pub st_nlink: u64,
    pub st_mode: u32,
    pub st_uid: u32,
    pub st_gid: u32,
    _pad0: u32,
    pub st_rdev: u64,
    pub st_size: i64,
    pub st_blksize: i64,
    pub st_blocks: i64,
    pub st_atime: i64,
    pub st_atime_nsec: i64,
    pub st_mtime: i64,
    pub st_mtime_nsec: i64,
    pub st_ctime: i64,
    pub st_ctime_nsec: i64,
    _reserved: [i64; 3],
}

impl Stat {
    pub fn is_dir(&self) -> bool {
        self.st_mode & S_IFMT == S_IFDIR
    }
    pub fn is_file(&self) -> bool {
        self.st_mode & S_IFMT == S_IFREG
    }
}

pub fn stat(path: &str) -> Result<Stat, isize> {
    let mut st = Stat::default();
    let r = with_cstr(path, |p| unsafe { syscall3(sys::STAT, p, &mut st as *mut Stat as u64, 0) });
    if r < 0 { Err(r) } else { Ok(st) }
}

pub fn fstat(fd: i32) -> Result<Stat, isize> {
    let mut st = Stat::default();
    let r = unsafe { syscall3(sys::FSTAT, fd as u64, &mut st as *mut Stat as u64, 0) } as isize;
    if r < 0 { Err(r) } else { Ok(st) }
}

// ---- directories ----

pub const DT_DIR: u8 = 4;
pub const DT_REG: u8 = 8;

pub fn getdents64(fd: i32, buf: &mut [u8]) -> isize {
    unsafe { syscall3(sys::GETDENTS64, fd as u64, buf.as_mut_ptr() as u64, buf.len() as u64) as isize }
}

pub struct DirEntry<'a> {
    pub ino: u64,
    pub d_type: u8,
    pub name: &'a str,
}

/// Iterate over the records in a getdents64 buffer.
pub fn dirents(buf: &[u8]) -> impl Iterator<Item = DirEntry<'_>> {
    let mut off = 0usize;
    core::iter::from_fn(move || {
        if off + 19 > buf.len() {
            return None;
        }
        let rec = &buf[off..];
        let ino = u64::from_le_bytes(rec[0..8].try_into().unwrap());
        let reclen = u16::from_le_bytes([rec[16], rec[17]]) as usize;
        if reclen < 20 || off + reclen > buf.len() {
            return None;
        }
        let name_bytes = &rec[19..reclen];
        let len = name_bytes.iter().position(|&b| b == 0).unwrap_or(name_bytes.len());
        off += reclen;
        Some(DirEntry { ino, d_type: rec[18], name: core::str::from_utf8(&name_bytes[..len]).unwrap_or("?") })
    })
}

/// Call `f` for every entry in directory `fd` (including "." and "..").
pub fn for_each_entry(fd: i32, mut f: impl FnMut(&DirEntry)) -> Result<(), isize> {
    let mut buf = [0u8; 1024];
    loop {
        let n = getdents64(fd, &mut buf);
        if n < 0 {
            return Err(n);
        }
        if n == 0 {
            return Ok(());
        }
        for entry in dirents(&buf[..n as usize]) {
            f(&entry);
        }
    }
}

/// Write all of `data` to `fd`, retrying short writes.
pub fn write_all(fd: i32, mut data: &[u8]) -> Result<(), isize> {
    while !data.is_empty() {
        let n = crate::write(fd, data);
        if n < 0 {
            return Err(n);
        }
        if n == 0 {
            return Err(-EIO);
        }
        data = &data[n as usize..];
    }
    Ok(())
}

// Copyright (c) 2026 tre4surehunter9
//! File syscalls: the file descriptor table and the POSIX file API on top of
//! the in-memory filesystem (Fdfs).
//!
//! Each process has its own `Files`: the fd table (0/1/2 start on the
//! console) and working directory. A child gets a copy at fork, so a
//! program's chdir doesn't affect its parent, and everything a process
//! leaves open is closed when it exits.

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use spin::Mutex;

use crate::fdfs::Node;
use crate::filesystem::{self, FS};
use crate::syscall::*;

pub const MAX_FDS: usize = 64;
/// Largest file a program may create (the kernel heap is small).
pub const MAX_FILE_SIZE: usize = 512 * 1024;
pub const PATH_MAX: usize = 4096;

// open() flags (Linux values)
pub const O_ACCMODE: u64 = 0o3;
pub const O_RDONLY: u64 = 0o0;
pub const O_WRONLY: u64 = 0o1;
pub const O_RDWR: u64 = 0o2;
pub const O_CREAT: u64 = 0o100;
pub const O_EXCL: u64 = 0o200;
pub const O_TRUNC: u64 = 0o1000;
pub const O_APPEND: u64 = 0o2000;
pub const O_DIRECTORY: u64 = 0o200000;

pub const AT_FDCWD: i64 = -100;

pub const SEEK_SET: u64 = 0;
pub const SEEK_CUR: u64 = 1;
pub const SEEK_END: u64 = 2;

// st_mode bits
const S_IFCHR: u32 = 0o020000;
const S_IFDIR: u32 = 0o040000;
const S_IFREG: u32 = 0o100000;

// d_type values for getdents64
const DT_CHR: u8 = 2;
const DT_DIR: u8 = 4;
const DT_REG: u8 = 8;

#[derive(Clone)]
enum OpenFile {
    /// The console: reads come from the TTY, writes go to the screen.
    Console,
    File { node: usize, offset: usize, readable: bool, writable: bool, append: bool },
    /// `pos` counts entries already returned by getdents64 ("." and ".." first).
    Dir { node: usize, path: String, pos: usize },
}

/// A process's open files and working directory. Copied by fork, kept
/// across execve, and dropped (closing everything) when the process exits.
#[derive(Clone)]
pub struct Files {
    fds: Vec<Option<OpenFile>>,
    pub cwd: String,
}

impl Files {
    /// fds 0, 1 and 2 on the console, and the given working directory.
    pub fn new(cwd: &str) -> Self {
        let mut fds = vec![None; MAX_FDS];
        fds[0] = Some(OpenFile::Console);
        fds[1] = Some(OpenFile::Console);
        fds[2] = Some(OpenFile::Console);
        Files { fds, cwd: cwd.to_string() }
    }
}

fn with_process<T>(f: impl FnOnce(&mut Files) -> T) -> T {
    crate::process::with_current_files(f)
}

/// Read a whole file into memory (for execve). Directories are EACCES.
pub fn read_whole_file(path: &str) -> Result<Vec<u8>, i64> {
    let abs = absolute(path);
    let fs = FS.lock();
    let idx = fs.resolve(&abs).ok_or(ENOENT)?;
    let size = fs.file_size(idx).ok_or(EACCES)?;
    let mut data = vec![0u8; size];
    fs.read_at(idx, 0, &mut data).map_err(errno_for)?;
    Ok(data)
}

/// Read a NUL-terminated string from user memory (paths, argv entries).
pub fn user_string(ptr: u64) -> Result<String, i64> {
    user_cstr(ptr)
}

/// Map Fdfs error messages to errno values.
pub fn errno_for(msg: &str) -> i64 {
    match msg {
        "Not found" | "File not found" | "Directory not found" | "Parent not found"
        | "Parent directory not found" | "Invalid path" => ENOENT,
        "Not a directory" | "Parent is not a directory" => ENOTDIR,
        "Is a directory" => EISDIR,
        "Already exists" => EEXIST,
        "Directory is not empty" => ENOTEMPTY,
        "Path ends with /" => EINVAL,
        "File too large" => EFBIG,
        _ => EIO,
    }
}

/// Read a NUL-terminated path from user memory, validating every page.
fn user_path(ptr: u64) -> Result<String, i64> {
    match user_cstr(ptr)? {
        p if p.is_empty() => Err(ENOENT), // POSIX: empty path
        p => Ok(p),
    }
}

fn user_cstr(ptr: u64) -> Result<String, i64> {
    let mut bytes = Vec::new();
    let mut addr = ptr;
    loop {
        if !crate::memory::is_user_range(addr, 1, false) {
            return Err(EFAULT);
        }
        // Scan to the end of this page without re-checking every byte.
        let page_end = (addr | 0xfff) + 1;
        while addr < page_end {
            let b = unsafe { *(addr as *const u8) };
            if b == 0 {
                return String::from_utf8(bytes).map_err(|_| EINVAL);
            }
            bytes.push(b);
            if bytes.len() >= PATH_MAX {
                return Err(ENAMETOOLONG);
            }
            addr += 1;
        }
    }
}

/// Resolve a user path to an absolute path, relative to the process cwd.
fn absolute(path: &str) -> String {
    let cwd = with_process(|p| p.cwd.clone());
    filesystem::normalize(&cwd, path)
}

fn alloc_fd(p: &mut Files, file: OpenFile) -> Result<usize, i64> {
    let fd = p.fds.iter().position(|f| f.is_none()).ok_or(EMFILE)?;
    p.fds[fd] = Some(file);
    Ok(fd)
}

fn open_path(abs: &str, flags: u64) -> i64 {
    let access = flags & O_ACCMODE;
    let readable = access == O_RDONLY || access == O_RDWR;
    let writable = access == O_WRONLY || access == O_RDWR;
    if access == O_ACCMODE {
        return -EINVAL;
    }

    let mut fs = FS.lock();
    let node = match fs.resolve(abs) {
        Some(idx) => {
            if flags & O_CREAT != 0 && flags & O_EXCL != 0 {
                return -EEXIST;
            }
            idx
        }
        None if flags & O_CREAT != 0 => match fs.create_file(abs) {
            Ok(idx) => idx,
            Err(e) => return -errno_for(e),
        },
        None => return -ENOENT,
    };

    let file = if fs.node(node).is_some_and(Node::is_dir) {
        if writable {
            return -EISDIR;
        }
        OpenFile::Dir { node, path: abs.to_string(), pos: 0 }
    } else {
        if flags & O_DIRECTORY != 0 {
            return -ENOTDIR;
        }
        if flags & O_TRUNC != 0 && writable {
            let _ = fs.truncate(node, 0);
        }
        OpenFile::File { node, offset: 0, readable, writable, append: flags & O_APPEND != 0 }
    };
    drop(fs);

    match with_process(|p| alloc_fd(p, file)) {
        Ok(fd) => fd as i64,
        Err(e) => -e,
    }
}

// ---- syscalls ----

/// open(path, flags, mode)
pub fn sys_open(path: u64, flags: u64) -> i64 {
    match user_path(path) {
        Ok(path) => open_path(&absolute(&path), flags),
        Err(e) => -e,
    }
}

/// openat(dirfd, path, flags, mode): relative paths resolve against dirfd.
pub fn sys_openat(dirfd: u64, path: u64, flags: u64) -> i64 {
    let path = match user_path(path) {
        Ok(p) => p,
        Err(e) => return -e,
    };
    if path.starts_with('/') || dirfd as i64 == AT_FDCWD {
        return open_path(&absolute(&path), flags);
    }
    let base = with_process(|p| match p.fds.get(dirfd as usize) {
        Some(Some(OpenFile::Dir { path, .. })) => Ok(path.clone()),
        Some(Some(_)) => Err(ENOTDIR),
        _ => Err(EBADF),
    });
    match base {
        Ok(base) => open_path(&filesystem::normalize(&base, &path), flags),
        Err(e) => -e,
    }
}

/// Print to the screen, handling the few ANSI escape sequences programs use:
/// ESC[2J / ESC[J clears the screen, ESC[H moves to the top left. Other
/// sequences are dropped instead of being printed as garbage.
fn console_write(bytes: &[u8]) {
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'[') {
            let mut j = i + 2;
            while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b';') {
                j += 1;
            }
            if j < bytes.len() {
                match bytes[j] {
                    b'J' => crate::vga_buffer::clear_screen(),
                    b'H' => crate::vga_buffer::WRITER.lock().set_cursor_pos(0, 0),
                    _ => {}
                }
                i = j + 1;
                continue;
            }
        }
        crate::print!("{}", bytes[i] as char);
        i += 1;
    }
}

/// dup(fd): copy an fd to the lowest free slot.
pub fn sys_dup(fd: u64) -> i64 {
    match get_fd(fd) {
        Ok(file) => match with_process(|p| alloc_fd(p, file)) {
            Ok(new) => new as i64,
            Err(e) => -e,
        },
        Err(e) => -e,
    }
}

/// dup2(oldfd, newfd): make newfd refer to the same file as oldfd,
/// closing whatever newfd was. Used for redirections like `> file`.
pub fn sys_dup2(old: u64, new: u64) -> i64 {
    let file = match get_fd(old) {
        Ok(f) => f,
        Err(e) => return -e,
    };
    if new as usize >= MAX_FDS {
        return -EBADF;
    }
    with_process(|p| p.fds[new as usize] = Some(file));
    new as i64
}

/// close(fd)
pub fn sys_close(fd: u64) -> i64 {
    with_process(|p| match p.fds.get_mut(fd as usize) {
        Some(slot @ Some(_)) => {
            *slot = None;
            0
        }
        _ => -EBADF,
    })
}

fn get_fd(fd: u64) -> Result<OpenFile, i64> {
    with_process(|p| p.fds.get(fd as usize).cloned().flatten().ok_or(EBADF))
}

fn set_offset(fd: u64, new_offset: usize) {
    with_process(|p| {
        if let Some(Some(OpenFile::File { offset, .. })) = p.fds.get_mut(fd as usize) {
            *offset = new_offset;
        }
    });
}

/// read(fd, buf, count)
pub fn sys_read(fd: u64, buf: &mut [u8]) -> i64 {
    match get_fd(fd) {
        Ok(OpenFile::Console) => crate::tty::read(buf) as i64,
        Ok(OpenFile::File { node, offset, readable, .. }) => {
            if !readable {
                return -EBADF;
            }
            match FS.lock().read_at(node, offset, buf) {
                Ok(n) => {
                    set_offset(fd, offset + n);
                    n as i64
                }
                Err(e) => -errno_for(e),
            }
        }
        Ok(OpenFile::Dir { .. }) => -EISDIR,
        Err(e) => -e,
    }
}

/// write(fd, buf, count)
pub fn sys_write(fd: u64, bytes: &[u8]) -> i64 {
    match get_fd(fd) {
        Ok(OpenFile::Console) => {
            console_write(bytes);
            bytes.len() as i64
        }
        Ok(OpenFile::File { node, offset, writable, append, .. }) => {
            if !writable {
                return -EBADF;
            }
            let mut fs = FS.lock();
            let offset = if append { fs.file_size(node).unwrap_or(0) } else { offset };
            if offset.saturating_add(bytes.len()) > MAX_FILE_SIZE {
                return -EFBIG;
            }
            match fs.write_at(node, offset, bytes) {
                Ok(n) => {
                    drop(fs);
                    set_offset(fd, offset + n);
                    n as i64
                }
                Err(e) => -errno_for(e),
            }
        }
        Ok(OpenFile::Dir { .. }) => -EISDIR,
        Err(e) => -e,
    }
}

/// lseek(fd, offset, whence)
pub fn sys_lseek(fd: u64, delta: u64, whence: u64) -> i64 {
    let delta = delta as i64;
    match get_fd(fd) {
        Ok(OpenFile::File { node, offset, .. }) => {
            let size = FS.lock().file_size(node).unwrap_or(0) as i64;
            let base = match whence {
                SEEK_SET => 0,
                SEEK_CUR => offset as i64,
                SEEK_END => size,
                _ => return -EINVAL,
            };
            match base.checked_add(delta) {
                Some(pos) if pos >= 0 && pos as usize <= MAX_FILE_SIZE => {
                    set_offset(fd, pos as usize);
                    pos
                }
                _ => -EINVAL,
            }
        }
        Ok(OpenFile::Dir { .. }) => {
            // Only rewinding a directory is supported.
            if whence == SEEK_SET && delta == 0 {
                with_process(|p| {
                    if let Some(Some(OpenFile::Dir { pos, .. })) = p.fds.get_mut(fd as usize) {
                        *pos = 0;
                    }
                });
                0
            } else {
                -EINVAL
            }
        }
        Ok(OpenFile::Console) => -ESPIPE,
        Err(e) => -e,
    }
}

/// Fill a Linux x86_64 `struct stat` (144 bytes).
fn fill_stat(out: &mut [u8], ino: u64, mode: u32, size: u64) {
    out.fill(0);
    let put64 = |out: &mut [u8], off: usize, v: u64| out[off..off + 8].copy_from_slice(&v.to_le_bytes());
    put64(out, 8, ino); // st_ino
    put64(out, 16, 1); // st_nlink
    out[24..28].copy_from_slice(&mode.to_le_bytes()); // st_mode
    put64(out, 48, size); // st_size
    put64(out, 56, 4096); // st_blksize
    put64(out, 64, size.div_ceil(512)); // st_blocks
}

pub const STAT_SIZE: u64 = 144;

fn stat_node(node: usize, out: &mut [u8]) -> i64 {
    let fs = FS.lock();
    match fs.node(node) {
        Some(Node::File { data, .. }) => fill_stat(out, node as u64 + 1, S_IFREG | 0o644, data.len() as u64),
        Some(Node::Dir { .. }) => fill_stat(out, node as u64 + 1, S_IFDIR | 0o755, 0),
        None => return -ENOENT,
    }
    0
}

/// fstat(fd, statbuf)
pub fn sys_fstat(fd: u64, out: &mut [u8]) -> i64 {
    match get_fd(fd) {
        Ok(OpenFile::Console) => {
            fill_stat(out, 0, S_IFCHR | 0o620, 0);
            0
        }
        Ok(OpenFile::File { node, .. }) | Ok(OpenFile::Dir { node, .. }) => stat_node(node, out),
        Err(e) => -e,
    }
}

/// stat(path, statbuf)
pub fn sys_stat(path: u64, out: &mut [u8]) -> i64 {
    let abs = match user_path(path) {
        Ok(p) => absolute(&p),
        Err(e) => return -e,
    };
    let node = FS.lock().resolve(&abs);
    match node {
        Some(node) => stat_node(node, out),
        None => -ENOENT,
    }
}

/// getdents64(fd, dirp, count): fill `out` with linux_dirent64 records.
pub fn sys_getdents64(fd: u64, out: &mut [u8]) -> i64 {
    let (node, path, pos) = match get_fd(fd) {
        Ok(OpenFile::Dir { node, path, pos }) => (node, path, pos),
        Ok(_) => return -ENOTDIR,
        Err(e) => return -e,
    };

    let fs = FS.lock();
    let parent = fs.resolve(&filesystem::normalize(&path, "..")).unwrap_or(node);
    let mut entries: Vec<(String, usize, u8)> = vec![(".".into(), node, DT_DIR), ("..".into(), parent, DT_DIR)];
    match fs.dir_entries(node) {
        Ok(children) => entries.extend(
            children.into_iter().map(|(name, idx, is_dir)| (name, idx, if is_dir { DT_DIR } else { DT_REG })),
        ),
        Err(e) => return -errno_for(e),
    }
    drop(fs);

    // struct linux_dirent64 { u64 d_ino; i64 d_off; u16 d_reclen; u8 d_type; char d_name[]; }
    let mut written = 0usize;
    let mut next = pos;
    for (name, idx, d_type) in entries.iter().skip(pos) {
        let reclen = (19 + name.len() + 1).next_multiple_of(8);
        if written + reclen > out.len() {
            if written == 0 {
                return -EINVAL; // buffer too small for even one entry
            }
            break;
        }
        let rec = &mut out[written..written + reclen];
        rec.fill(0);
        rec[0..8].copy_from_slice(&(*idx as u64 + 1).to_le_bytes());
        rec[8..16].copy_from_slice(&((next + 1) as i64).to_le_bytes());
        rec[16..18].copy_from_slice(&(reclen as u16).to_le_bytes());
        rec[18] = *d_type;
        rec[19..19 + name.len()].copy_from_slice(name.as_bytes());
        written += reclen;
        next += 1;
    }

    with_process(|p| {
        if let Some(Some(OpenFile::Dir { pos, .. })) = p.fds.get_mut(fd as usize) {
            *pos = next;
        }
    });
    written as i64
}

/// getcwd(buf, size): returns the length including the NUL.
pub fn sys_getcwd(out: &mut [u8]) -> i64 {
    let cwd = with_process(|p| p.cwd.clone());
    let needed = cwd.len() + 1;
    if out.len() < needed {
        return -ERANGE;
    }
    out[..cwd.len()].copy_from_slice(cwd.as_bytes());
    out[cwd.len()] = 0;
    needed as i64
}

/// chdir(path)
pub fn sys_chdir(path: u64) -> i64 {
    let abs = match user_path(path) {
        Ok(p) => absolute(&p),
        Err(e) => return -e,
    };
    let is_dir = {
        let fs = FS.lock();
        fs.resolve(&abs).and_then(|i| fs.node(i)).map(Node::is_dir)
    };
    match is_dir {
        Some(true) => {
            with_process(|p| p.cwd = abs);
            0
        }
        Some(false) => -ENOTDIR,
        None => -ENOENT,
    }
}

/// mkdir(path, mode)
pub fn sys_mkdir(path: u64) -> i64 {
    let abs = match user_path(path) {
        Ok(p) => absolute(&p),
        Err(e) => return -e,
    };
    if abs == "/" {
        return -EEXIST;
    }
    match FS.lock().make_dir(&abs) {
        Ok(()) => 0,
        Err(e) => -errno_for(e),
    }
}

/// unlink(path) for files, rmdir(path) for directories.
pub fn sys_remove(path: u64, want_dir: bool) -> i64 {
    let abs = match user_path(path) {
        Ok(p) => absolute(&p),
        Err(e) => return -e,
    };
    if abs == "/" {
        return if want_dir { -EBUSY } else { -EISDIR };
    }
    let mut fs = FS.lock();
    let is_dir = match fs.resolve(&abs).and_then(|i| fs.node(i)) {
        Some(n) => n.is_dir(),
        None => return -ENOENT,
    };
    if want_dir && !is_dir {
        return -ENOTDIR;
    }
    if !want_dir && is_dir {
        return -EISDIR;
    }
    match fs.remove(&abs) {
        Ok(()) => 0,
        Err(e) => -errno_for(e),
    }
}

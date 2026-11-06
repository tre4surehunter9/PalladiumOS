// Copyright (c) 2026 tre4surehunter9
//! File syscalls: the file descriptor table and the POSIX file API on top of
//! the in-memory filesystem (Fdfs).
//!
//! Each process has its own `Files`: the fd table and working directory.
//! An fd points to an *open file description* (`Handle`): the open file
//! and its offset. Like POSIX, descriptions are shared: fork gives the
//! child the same descriptions, and dup/dup2 make two fds share one, so
//! they share the file offset. (That's what makes `(echo a; echo b) > f`
//! and pipes work.) Each fd also has its own close-on-exec flag.
//!
//! A child gets a copy of the table at fork, so a program's chdir doesn't
//! affect its parent; everything a process leaves open is closed when it
//! exits.

use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use spin::Mutex;

use crate::fdfs::Node;
use crate::filesystem::{self, FS};
use crate::pipe::{self, PipeReader, PipeWriter};
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
pub const O_CLOEXEC: u64 = 0o2000000;

pub const AT_FDCWD: i64 = -100;

pub const SEEK_SET: u64 = 0;
pub const SEEK_CUR: u64 = 1;
pub const SEEK_END: u64 = 2;

// fcntl commands
const F_DUPFD: u64 = 0;
const F_GETFD: u64 = 1;
const F_SETFD: u64 = 2;
const F_GETFL: u64 = 3;
const F_SETFL: u64 = 4;
const F_DUPFD_CLOEXEC: u64 = 1030;
const FD_CLOEXEC: u64 = 1;

// st_mode bits
const S_IFIFO: u32 = 0o010000;
const S_IFCHR: u32 = 0o020000;
const S_IFDIR: u32 = 0o040000;
const S_IFREG: u32 = 0o100000;

// d_type values for getdents64
const DT_DIR: u8 = 4;
const DT_REG: u8 = 8;

enum Kind {
    /// The console: reads come from the TTY, writes go to the screen.
    Console,
    File { node: usize, readable: bool, writable: bool, append: bool },
    Dir { node: usize, path: String },
    PipeRead(PipeReader),
    PipeWrite(PipeWriter),
}

/// An open file description.
struct Description {
    kind: Kind,
    /// Byte offset for files; for directories, the number of entries
    /// getdents64 has returned ("." and ".." first).
    offset: usize,
}

type Handle = Arc<Mutex<Description>>;

fn handle(kind: Kind) -> Handle {
    Arc::new(Mutex::new(Description { kind, offset: 0 }))
}

#[derive(Clone)]
struct Slot {
    handle: Handle,
    cloexec: bool,
}

/// A process's open files and working directory. Copied by fork (sharing
/// the descriptions), kept across execve except close-on-exec fds, and
/// dropped (closing everything) when the process exits.
#[derive(Clone)]
pub struct Files {
    fds: Vec<Option<Slot>>,
    pub cwd: String,
}

impl Files {
    /// fds 0, 1 and 2 on the console, and the given working directory.
    pub fn new(cwd: &str) -> Self {
        let mut fds = vec![None; MAX_FDS];
        let console = handle(Kind::Console);
        for slot in &mut fds[..3] {
            *slot = Some(Slot { handle: console.clone(), cloexec: false });
        }
        Files { fds, cwd: cwd.to_string() }
    }

    /// A table with nothing open (for exiting processes).
    pub fn empty() -> Self {
        Files { fds: vec![None; MAX_FDS], cwd: "/".to_string() }
    }

    /// Close the close-on-exec fds (done by execve).
    pub fn close_on_exec(&mut self) {
        for slot in &mut self.fds {
            if slot.as_ref().is_some_and(|s| s.cloexec) {
                *slot = None;
            }
        }
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

/// Put `handle` in the lowest free fd at or above `min`.
fn alloc_fd_from(p: &mut Files, handle: Handle, cloexec: bool, min: usize) -> Result<usize, i64> {
    let fd = (min..MAX_FDS).find(|&i| p.fds[i].is_none()).ok_or(EMFILE)?;
    p.fds[fd] = Some(Slot { handle, cloexec });
    Ok(fd)
}

fn alloc_fd(p: &mut Files, handle: Handle, cloexec: bool) -> Result<usize, i64> {
    alloc_fd_from(p, handle, cloexec, 0)
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

    let kind = if fs.node(node).is_some_and(Node::is_dir) {
        if writable {
            return -EISDIR;
        }
        Kind::Dir { node, path: abs.to_string() }
    } else {
        if flags & O_DIRECTORY != 0 {
            return -ENOTDIR;
        }
        if flags & O_TRUNC != 0 && writable {
            let _ = fs.truncate(node, 0);
        }
        Kind::File { node, readable, writable, append: flags & O_APPEND != 0 }
    };
    drop(fs);

    match with_process(|p| alloc_fd(p, handle(kind), flags & O_CLOEXEC != 0)) {
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
    let base = get(dirfd).and_then(|h| match &h.lock().kind {
        Kind::Dir { path, .. } => Ok(path.clone()),
        _ => Err(ENOTDIR),
    });
    match base {
        Ok(base) => open_path(&filesystem::normalize(&base, &path), flags),
        Err(e) => -e,
    }
}

/// Print program output, interpreting ANSI escape sequences.
fn console_write(bytes: &[u8]) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        crate::vga_buffer::WRITER.lock().write_ansi(bytes);
    });
}

// ioctl requests (Linux values)
const TCGETS: u64 = 0x5401;
const TCSETS: u64 = 0x5402;
const TCSETSW: u64 = 0x5403;
const TCSETSF: u64 = 0x5404;
const TIOCGWINSZ: u64 = 0x5413;
const TIOCGPGRP: u64 = 0x540F;
const TIOCSPGRP: u64 = 0x5410;
/// sizeof(struct termios) for the TCGETS/TCSETS ioctls: four u32 flag
/// words, c_line, and 19 control characters.
const TERMIOS_SIZE: u64 = 36;

/// ioctl(fd, request, arg): terminal settings for the console.
/// Anything that isn't the console is ENOTTY (how isatty() works).
pub fn sys_ioctl(fd: u64, request: u64, arg: u64) -> i64 {
    use crate::syscall::{user_slice, user_slice_mut};
    match get(fd) {
        Ok(h) if matches!(h.lock().kind, Kind::Console) => {}
        Ok(_) => return -ENOTTY,
        Err(e) => return -e,
    }
    match request {
        TCGETS => {
            let Some(out) = (unsafe { user_slice_mut(arg, TERMIOS_SIZE) }) else { return -EFAULT };
            let mode = crate::tty::mode();
            out.fill(0);
            out[0..4].copy_from_slice(&mode.iflag.to_le_bytes()); // c_iflag
            out[4..8].copy_from_slice(&1u32.to_le_bytes()); // c_oflag: OPOST
            out[8..12].copy_from_slice(&0o277u32.to_le_bytes()); // c_cflag: CS8|CREAD|B38400
            out[12..16].copy_from_slice(&mode.lflag.to_le_bytes()); // c_lflag
            // c_cc: VINTR ^C, VERASE DEL, VKILL ^U, VEOF ^D, VMIN 1
            out[17] = 3;
            out[19] = 0x7f;
            out[20] = 0x15;
            out[21] = 4;
            out[23] = 1;
            0
        }
        TCSETS | TCSETSW | TCSETSF => {
            let Some(input) = (unsafe { user_slice(arg, TERMIOS_SIZE) }) else { return -EFAULT };
            let word = |i: usize| u32::from_le_bytes(input[i..i + 4].try_into().unwrap());
            crate::tty::set_mode(crate::tty::Mode { iflag: word(0), lflag: word(12) });
            0
        }
        TIOCGPGRP => {
            let Some(out) = (unsafe { user_slice_mut(arg, 4) }) else { return -EFAULT };
            out.copy_from_slice(&(crate::process::foreground() as i32).to_le_bytes());
            0
        }
        TIOCSPGRP => {
            let Some(input) = (unsafe { user_slice(arg, 4) }) else { return -EFAULT };
            let pgid = i32::from_le_bytes(input.try_into().unwrap());
            if pgid <= 0 {
                return -EINVAL;
            }
            crate::process::set_foreground(pgid as u64);
            0
        }
        TIOCGWINSZ => {
            let Some(out) = (unsafe { user_slice_mut(arg, 8) }) else { return -EFAULT };
            out.fill(0);
            out[0..2].copy_from_slice(&25u16.to_le_bytes()); // ws_row
            out[2..4].copy_from_slice(&80u16.to_le_bytes()); // ws_col
            0
        }
        _ => -EINVAL,
    }
}

/// The open file description behind `fd`.
fn get(fd: u64) -> Result<Handle, i64> {
    with_process(|p| p.fds.get(fd as usize).cloned().flatten().map(|s| s.handle).ok_or(EBADF))
}

/// dup(fd): another fd for the same open file (sharing the offset).
pub fn sys_dup(fd: u64) -> i64 {
    match get(fd) {
        Ok(h) => match with_process(|p| alloc_fd(p, h, false)) {
            Ok(new) => new as i64,
            Err(e) => -e,
        },
        Err(e) => -e,
    }
}

/// dup2(oldfd, newfd) / dup3(oldfd, newfd, flags): make newfd refer to the
/// same open file as oldfd, closing whatever newfd was. Used for
/// redirections like `> file` and for pipes.
pub fn sys_dup3(old: u64, new: u64, flags: u64, is_dup3: bool) -> i64 {
    if flags & !O_CLOEXEC != 0 || (is_dup3 && old == new) {
        return -EINVAL;
    }
    let h = match get(old) {
        Ok(h) => h,
        Err(e) => return -e,
    };
    if new as usize >= MAX_FDS {
        return -EBADF;
    }
    if old != new {
        with_process(|p| p.fds[new as usize] = Some(Slot { handle: h, cloexec: flags & O_CLOEXEC != 0 }));
    }
    new as i64
}

/// close(fd)
pub fn sys_close(fd: u64) -> i64 {
    // The description is dropped after the process table is unlocked (it
    // may be the last end of a pipe, which wakes other processes).
    let slot = with_process(|p| p.fds.get_mut(fd as usize).and_then(Option::take));
    if slot.is_some() { 0 } else { -EBADF }
}

/// pipe2(fds, flags): fds[0] is the read end, fds[1] the write end.
pub fn sys_pipe2(fds_out: u64, flags: u64) -> i64 {
    if flags & !O_CLOEXEC != 0 {
        return -EINVAL; // O_NONBLOCK and O_DIRECT aren't supported
    }
    let Some(out) = (unsafe { user_slice_mut(fds_out, 8) }) else { return -EFAULT };
    let cloexec = flags & O_CLOEXEC != 0;
    let (reader, writer) = pipe::new_pipe();
    let result = with_process(|p| {
        let r = alloc_fd(p, handle(Kind::PipeRead(reader)), cloexec)?;
        match alloc_fd(p, handle(Kind::PipeWrite(writer)), cloexec) {
            Ok(w) => Ok((r, w)),
            Err(e) => {
                p.fds[r] = None;
                Err(e)
            }
        }
    });
    match result {
        Ok((r, w)) => {
            out[0..4].copy_from_slice(&(r as i32).to_le_bytes());
            out[4..8].copy_from_slice(&(w as i32).to_le_bytes());
            0
        }
        Err(e) => -e,
    }
}

/// fcntl(fd, cmd, arg): duplicate fds, and get/set FD_CLOEXEC and the
/// status flags.
pub fn sys_fcntl(fd: u64, cmd: u64, arg: u64) -> i64 {
    let fd = fd as usize;
    with_process(|p| {
        let Some(Some(slot)) = p.fds.get(fd).cloned() else { return -EBADF };
        match cmd {
            F_DUPFD | F_DUPFD_CLOEXEC => {
                if arg as usize >= MAX_FDS {
                    return -EINVAL;
                }
                match alloc_fd_from(p, slot.handle, cmd == F_DUPFD_CLOEXEC, arg as usize) {
                    Ok(new) => new as i64,
                    Err(e) => -e,
                }
            }
            F_GETFD => if slot.cloexec { FD_CLOEXEC as i64 } else { 0 },
            F_SETFD => {
                p.fds[fd].as_mut().unwrap().cloexec = arg & FD_CLOEXEC != 0;
                0
            }
            F_GETFL => match &slot.handle.lock().kind {
                Kind::File { readable, writable, append, .. } => {
                    let mode = match (readable, writable) {
                        (true, true) => O_RDWR,
                        (false, true) => O_WRONLY,
                        _ => O_RDONLY,
                    };
                    (mode | if *append { O_APPEND } else { 0 }) as i64
                }
                Kind::Console => O_RDWR as i64,
                Kind::PipeWrite(_) => O_WRONLY as i64,
                Kind::Dir { .. } | Kind::PipeRead(_) => O_RDONLY as i64,
            },
            F_SETFL => 0, // O_APPEND/O_NONBLOCK changes aren't supported; accepted
            _ => -EINVAL,
        }
    })
}

/// read(fd, buf, count)
pub fn sys_read(fd: u64, buf: &mut [u8]) -> i64 {
    let h = match get(fd) {
        Ok(h) => h,
        Err(e) => return -e,
    };
    let mut desc = h.lock();
    match &desc.kind {
        Kind::Console => {
            drop(desc); // don't hold it while waiting for a key
            crate::tty::read(buf)
        }
        Kind::PipeRead(r) => {
            let pipe = r.pipe();
            drop(desc); // or while waiting for data
            pipe::read(&pipe, buf)
        }
        Kind::File { node, readable, .. } => {
            if !readable {
                return -EBADF;
            }
            let node = *node;
            match FS.lock().read_at(node, desc.offset, buf) {
                Ok(n) => {
                    desc.offset += n;
                    n as i64
                }
                Err(e) => -errno_for(e),
            }
        }
        Kind::Dir { .. } => -EISDIR,
        Kind::PipeWrite(_) => -EBADF,
    }
}

/// write(fd, buf, count)
pub fn sys_write(fd: u64, bytes: &[u8]) -> i64 {
    let h = match get(fd) {
        Ok(h) => h,
        Err(e) => return -e,
    };
    let mut desc = h.lock();
    match &desc.kind {
        Kind::Console => {
            drop(desc);
            console_write(bytes);
            bytes.len() as i64
        }
        Kind::PipeWrite(w) => {
            let pipe = w.pipe();
            drop(desc);
            pipe::write(&pipe, bytes)
        }
        Kind::File { node, writable, append, .. } => {
            if !writable {
                return -EBADF;
            }
            let (node, append) = (*node, *append);
            let mut fs = FS.lock();
            let offset = if append { fs.file_size(node).unwrap_or(0) } else { desc.offset };
            if offset.saturating_add(bytes.len()) > MAX_FILE_SIZE {
                return -EFBIG;
            }
            match fs.write_at(node, offset, bytes) {
                Ok(n) => {
                    desc.offset = offset + n;
                    n as i64
                }
                Err(e) => -errno_for(e),
            }
        }
        Kind::Dir { .. } => -EISDIR,
        Kind::PipeRead(_) => -EBADF,
    }
}

/// lseek(fd, offset, whence)
pub fn sys_lseek(fd: u64, delta: u64, whence: u64) -> i64 {
    let delta = delta as i64;
    let h = match get(fd) {
        Ok(h) => h,
        Err(e) => return -e,
    };
    let mut desc = h.lock();
    match &desc.kind {
        Kind::File { node, .. } => {
            let size = FS.lock().file_size(*node).unwrap_or(0) as i64;
            let base = match whence {
                SEEK_SET => 0,
                SEEK_CUR => desc.offset as i64,
                SEEK_END => size,
                _ => return -EINVAL,
            };
            match base.checked_add(delta) {
                Some(pos) if pos >= 0 && pos as usize <= MAX_FILE_SIZE => {
                    desc.offset = pos as usize;
                    pos
                }
                _ => -EINVAL,
            }
        }
        // Only rewinding a directory is supported.
        Kind::Dir { .. } if whence == SEEK_SET && delta == 0 => {
            desc.offset = 0;
            0
        }
        Kind::Dir { .. } => -EINVAL,
        Kind::Console | Kind::PipeRead(_) | Kind::PipeWrite(_) => -ESPIPE,
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
    let h = match get(fd) {
        Ok(h) => h,
        Err(e) => return -e,
    };
    let desc = h.lock();
    match &desc.kind {
        Kind::Console => {
            fill_stat(out, 0, S_IFCHR | 0o620, 0);
            0
        }
        Kind::PipeRead(_) | Kind::PipeWrite(_) => {
            fill_stat(out, 0, S_IFIFO | 0o600, 0);
            0
        }
        Kind::File { node, .. } | Kind::Dir { node, .. } => stat_node(*node, out),
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
    let h = match get(fd) {
        Ok(h) => h,
        Err(e) => return -e,
    };
    let mut desc = h.lock();
    let (node, path) = match &desc.kind {
        Kind::Dir { node, path } => (*node, path.clone()),
        _ => return -ENOTDIR,
    };
    let pos = desc.offset;

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
    desc.offset = next;
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

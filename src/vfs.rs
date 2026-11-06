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

use crate::filesystem;
use crate::perm;
use crate::mounts::{self, Device, FileType, Loc};
use crate::pipe::{self, PipeReader, PipeWriter};
use crate::syscall::*;

pub const MAX_FDS: usize = 64;
/// Largest file a program may create in an in-memory filesystem (/tmp),
/// which lives in the kernel heap. (Big enough for a compiled tcc.)
pub const MAX_FILE_SIZE: usize = 4 * 1024 * 1024;
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
pub const O_NOFOLLOW: u64 = 0o400000;

const AT_SYMLINK_NOFOLLOW: u64 = 0x100;
const AT_REMOVEDIR: u64 = 0x200;
const AT_SYMLINK_FOLLOW: u64 = 0x400;
const ELOOP: i64 = 40;

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
const S_IFLNK: u32 = 0o120000;

// d_type values for getdents64
const DT_CHR: u8 = 2;
const DT_DIR: u8 = 4;
const DT_REG: u8 = 8;
const DT_LNK: u8 = 10;

enum Kind {
    /// The console: reads come from the TTY, writes go to the screen.
    Console,
    File { loc: Loc, path: String, readable: bool, writable: bool, append: bool },
    Dir { loc: Loc, path: String },
    /// /dev/null: reads are end-of-file, writes are discarded.
    Null,
    /// /dev/zero: reads are zeros, writes are discarded.
    Zero,
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

impl Drop for Description {
    fn drop(&mut self) {
        // The last fd for this open file is gone: tell the filesystem (a
        // file deleted while open is freed now).
        if let Kind::File { loc, .. } | Kind::Dir { loc, .. } = &self.kind {
            mounts::with_fs(loc.mount, |fs| fs.close_inode(loc.ino));
        }
    }
}

/// An open file: its description, plus a label that never changes (what
/// /proc/PID/fd shows). The label is outside the lock so /proc can read it
/// even while a syscall on this very file holds the lock.
struct OpenFile {
    label: String,
    desc: Mutex<Description>,
}

impl OpenFile {
    fn lock(&self) -> spin::MutexGuard<'_, Description> {
        self.desc.lock()
    }
}

type Handle = Arc<OpenFile>;

fn handle(kind: Kind) -> Handle {
    let label = match &kind {
        Kind::Console => "/dev/tty".to_string(),
        Kind::Null => "/dev/null".to_string(),
        Kind::Zero => "/dev/zero".to_string(),
        Kind::File { path, .. } | Kind::Dir { path, .. } => path.clone(),
        Kind::PipeRead(r) => alloc::format!("pipe:[{}]", pipe::id(&r.pipe())),
        Kind::PipeWrite(w) => alloc::format!("pipe:[{}]", pipe::id(&w.pipe())),
    };
    Arc::new(OpenFile { label, desc: Mutex::new(Description { kind, offset: 0 }) })
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
    /// File creation mask: these permission bits are removed from new files.
    pub umask: u32,
}

impl Files {
    /// fds 0, 1 and 2 on the console, and the given working directory.
    pub fn new(cwd: &str) -> Self {
        let mut fds = vec![None; MAX_FDS];
        let console = handle(Kind::Console);
        for slot in &mut fds[..3] {
            *slot = Some(Slot { handle: console.clone(), cloexec: false });
        }
        Files { fds, cwd: cwd.to_string(), umask: 0o022 }
    }

    /// A table with nothing open (for exiting processes).
    pub fn empty() -> Self {
        Files { fds: vec![None; MAX_FDS], cwd: "/".to_string(), umask: 0o022 }
    }

    /// (fd, what it is) for each open fd: a path, /dev/tty, or pipe:[id].
    pub fn describe(&self) -> Vec<(usize, String)> {
        self.fds
            .iter()
            .enumerate()
            .filter_map(|(fd, slot)| Some((fd, slot.as_ref()?.handle.label.clone())))
            .collect()
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
    let (loc, meta) = mounts::stat(&absolute(path))?;
    if meta.kind != FileType::File {
        return Err(EACCES);
    }
    let mut data = vec![0u8; meta.size as usize];
    let n = mounts::with_fs(loc.mount, |fs| fs.read(loc.ino, 0, &mut data))?;
    data.truncate(n);
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

fn open_path(abs: &str, flags: u64, mode: u64) -> i64 {
    let access = flags & O_ACCMODE;
    let readable = access == O_RDONLY || access == O_RDWR;
    let writable = access == O_WRONLY || access == O_RDWR;
    if access == O_ACCMODE {
        return -EINVAL;
    }

    let mut created = false;
    let looked_up = if flags & O_NOFOLLOW != 0 { mounts::lstat(abs) } else { mounts::stat(abs) };
    let (loc, meta) = match looked_up {
        Ok((_, meta)) if meta.kind == FileType::Symlink => return -ELOOP, // O_NOFOLLOW
        Ok(found) => {
            if flags & O_CREAT != 0 && flags & O_EXCL != 0 {
                return -EEXIST;
            }
            found
        }
        Err(ENOENT) if flags & O_CREAT != 0 => {
            let perm = mode as u32 & 0o7777 & !with_process(|p| p.umask);
            let made = mounts::create(abs, FileType::File, perm)
                .and_then(|loc| Ok((loc, mounts::with_fs(loc.mount, |fs| fs.meta(loc.ino))?)));
            match made {
                Ok(found) => {
                    created = true;
                    found
                }
                Err(e) => return -e,
            }
        }
        Err(e) => return -e,
    };

    // Permission to open it the way asked (a just-created file is ours).
    let cred = crate::process::current_cred();
    let want = if readable { perm::READ } else { 0 } | if writable || flags & O_TRUNC != 0 { perm::WRITE } else { 0 };
    // (The open that creates a file may use it whatever its new mode says.)
    if !created {
        if let Err(e) = perm::check(&meta, &cred, want) {
            return -e;
        }
    }
    let kind = match meta.kind {
        FileType::Dir => {
            if writable {
                return -EISDIR;
            }
            Kind::Dir { loc, path: abs.to_string() }
        }
        FileType::CharDevice(dev) => match dev {
            Device::Null => Kind::Null,
            Device::Zero => Kind::Zero,
            Device::Tty => Kind::Console,
        },
        FileType::Symlink => return -ELOOP, // a loop of symlinks
        FileType::File => {
            if flags & O_DIRECTORY != 0 {
                return -ENOTDIR;
            }
            if flags & O_TRUNC != 0 && writable {
                if let Err(e) = mounts::with_fs(loc.mount, |fs| fs.truncate(loc.ino, 0)) {
                    return -e;
                }
            }
            Kind::File { loc, path: abs.to_string(), readable, writable, append: flags & O_APPEND != 0 }
        }
    };
    if let Kind::File { loc, .. } | Kind::Dir { loc, .. } = &kind {
        mounts::with_fs(loc.mount, |fs| fs.open_inode(loc.ino));
    }
    // (If there's no free fd, the description is dropped here, closing it.)
    let h = handle(kind);
    match with_process(|p| alloc_fd(p, h, flags & O_CLOEXEC != 0)) {
        Ok(fd) => fd as i64,
        Err(e) => -e,
    }
}

// ---- syscalls ----

/// open(path, flags, mode)
pub fn sys_open(path: u64, flags: u64, mode: u64) -> i64 {
    match user_path(path) {
        Ok(path) => open_path(&absolute(&path), flags, mode),
        Err(e) => -e,
    }
}

/// The absolute path for a `*at` syscall: `path` relative to the directory
/// `dirfd` (or the working directory for AT_FDCWD).
fn at_path(dirfd: u64, path: &str) -> Result<String, i64> {
    if path.starts_with('/') || dirfd as i64 as i32 as i64 == AT_FDCWD {
        return Ok(absolute(path));
    }
    let base = get(dirfd).and_then(|h| match &h.lock().kind {
        Kind::Dir { path, .. } => Ok(path.clone()),
        _ => Err(ENOTDIR),
    })?;
    Ok(filesystem::normalize(&base, path))
}

fn user_at_path(dirfd: u64, path: u64) -> Result<String, i64> {
    at_path(dirfd, &user_path(path)?)
}

/// openat(dirfd, path, flags, mode)
pub fn sys_openat(dirfd: u64, path: u64, flags: u64, mode: u64) -> i64 {
    match user_at_path(dirfd, path) {
        Ok(abs) => open_path(&abs, flags, mode),
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
            // c_cc (from byte 17): VINTR ^C, VERASE DEL, VKILL ^U, VEOF ^D, VTIME, VMIN
            out[17] = 3;
            out[19] = 0x7f;
            out[20] = 0x15;
            out[21] = 4;
            out[22] = mode.vtime;
            out[23] = mode.vmin;
            0
        }
        TCSETS | TCSETSW | TCSETSF => {
            let Some(input) = (unsafe { user_slice(arg, TERMIOS_SIZE) }) else { return -EFAULT };
            let word = |i: usize| u32::from_le_bytes(input[i..i + 4].try_into().unwrap());
            crate::tty::set_mode(crate::tty::Mode { iflag: word(0), lflag: word(12), vtime: input[22], vmin: input[23] });
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
                Kind::Console | Kind::Null | Kind::Zero => O_RDWR as i64,
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
        Kind::File { loc, readable, .. } => {
            if !readable {
                return -EBADF;
            }
            let loc = *loc;
            let offset = desc.offset as u64;
            match mounts::with_fs(loc.mount, |fs| fs.read(loc.ino, offset, buf)) {
                Ok(n) => {
                    desc.offset += n;
                    n as i64
                }
                Err(e) => -e,
            }
        }
        Kind::Null => 0,
        Kind::Zero => {
            buf.fill(0);
            buf.len() as i64
        }
        Kind::Dir { .. } => -EISDIR,
        Kind::PipeWrite(_) => -EBADF,
    }
}

/// pread64(fd, buf, count, offset): read at `offset` without moving the
/// file position (regular files only).
pub fn sys_pread(fd: u64, buf: &mut [u8], offset: u64) -> i64 {
    let h = match get(fd) {
        Ok(h) => h,
        Err(e) => return -e,
    };
    let desc = h.lock();
    match &desc.kind {
        Kind::File { loc, readable, .. } => {
            if !readable {
                return -EBADF;
            }
            let loc = *loc;
            match mounts::with_fs(loc.mount, |fs| fs.read(loc.ino, offset, buf)) {
                Ok(n) => n as i64,
                Err(e) => -e,
            }
        }
        Kind::Dir { .. } => -EISDIR,
        Kind::Zero => {
            buf.fill(0);
            buf.len() as i64
        }
        Kind::Null => 0,
        _ => -ESPIPE,
    }
}

/// pwrite64(fd, buf, count, offset): write at `offset` without moving the
/// file position.
pub fn sys_pwrite(fd: u64, bytes: &[u8], offset: u64) -> i64 {
    let h = match get(fd) {
        Ok(h) => h,
        Err(e) => return -e,
    };
    let (loc, writable) = match &h.lock().kind {
        Kind::File { loc, writable, .. } => (*loc, *writable),
        Kind::Null => return bytes.len() as i64,
        Kind::Dir { .. } => return -EISDIR,
        _ => return -ESPIPE,
    };
    if !writable {
        return -EBADF;
    }
    match mounts::with_fs(loc.mount, |fs| fs.write(loc.ino, offset, bytes)) {
        Ok(n) => n as i64,
        Err(e) => -e,
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
        Kind::File { loc, writable, append, .. } => {
            if !writable {
                return -EBADF;
            }
            let (loc, append) = (*loc, *append);
            let cur = desc.offset as u64;
            let result = mounts::with_fs(loc.mount, |fs| {
                let offset = if append { fs.meta(loc.ino)?.size } else { cur };
                // The in-memory filesystem lives in the (small) kernel heap.
                if fs.name() == "memfs" && offset as usize + bytes.len() > MAX_FILE_SIZE {
                    return Err(EFBIG);
                }
                fs.write(loc.ino, offset, bytes).map(|n| (offset, n))
            });
            match result {
                Ok((offset, n)) => {
                    desc.offset = offset as usize + n;
                    n as i64
                }
                Err(e) => -e,
            }
        }
        Kind::Null | Kind::Zero => bytes.len() as i64,
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
        Kind::File { loc, .. } => {
            let loc = *loc;
            let size = match mounts::with_fs(loc.mount, |fs| fs.meta(loc.ino)) {
                Ok(m) => m.size as i64,
                Err(e) => return -e,
            };
            let base = match whence {
                SEEK_SET => 0,
                SEEK_CUR => desc.offset as i64,
                SEEK_END => size,
                _ => return -EINVAL,
            };
            match base.checked_add(delta) {
                Some(pos) if pos >= 0 => {
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
        Kind::Null | Kind::Zero => 0,
        Kind::Console | Kind::PipeRead(_) | Kind::PipeWrite(_) => -ESPIPE,
    }
}

/// Fill a Linux x86_64 `struct stat` (144 bytes).
fn fill_stat(out: &mut [u8], dev: u64, ino: u64, mode: u32, nlink: u64, size: u64, mtime: u64) {
    fill_stat_owned(out, dev, ino, mode, nlink, size, mtime, 0, 0);
}

#[allow(clippy::too_many_arguments)]
fn fill_stat_owned(out: &mut [u8], dev: u64, ino: u64, mode: u32, nlink: u64, size: u64, mtime: u64, uid: u32, gid: u32) {
    out.fill(0);
    out[28..32].copy_from_slice(&uid.to_le_bytes()); // st_uid
    out[32..36].copy_from_slice(&gid.to_le_bytes()); // st_gid
    let put64 = |out: &mut [u8], off: usize, v: u64| out[off..off + 8].copy_from_slice(&v.to_le_bytes());
    put64(out, 0, dev); // st_dev
    put64(out, 8, ino); // st_ino
    put64(out, 16, nlink); // st_nlink
    out[24..28].copy_from_slice(&mode.to_le_bytes()); // st_mode
    put64(out, 48, size); // st_size
    put64(out, 56, 4096); // st_blksize
    put64(out, 64, size.div_ceil(512)); // st_blocks
    put64(out, 72, mtime); // st_atime
    put64(out, 88, mtime); // st_mtime
    put64(out, 104, mtime); // st_ctime
}

pub const STAT_SIZE: u64 = 144;

fn stat_loc(loc: Loc, out: &mut [u8]) -> i64 {
    let meta = match mounts::with_fs(loc.mount, |fs| fs.meta(loc.ino)) {
        Ok(m) => m,
        Err(e) => return -e,
    };
    let kind_bits = match meta.kind {
        FileType::File => S_IFREG,
        FileType::Dir => S_IFDIR,
        FileType::Symlink => S_IFLNK,
        FileType::CharDevice(_) => S_IFCHR,
    };
    fill_stat_owned(
        out,
        loc.mount as u64 + 1,
        meta.ino,
        kind_bits | meta.perm,
        meta.nlink as u64,
        meta.size,
        meta.mtime,
        meta.uid,
        meta.gid,
    );
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
        Kind::Console | Kind::Null | Kind::Zero => {
            fill_stat(out, 0, 0, S_IFCHR | 0o666, 1, 0, 0);
            0
        }
        Kind::PipeRead(_) | Kind::PipeWrite(_) => {
            fill_stat(out, 0, 0, S_IFIFO | 0o600, 1, 0, 0);
            0
        }
        Kind::File { loc, .. } | Kind::Dir { loc, .. } => stat_loc(*loc, out),
    }
}

/// stat(path, statbuf) and lstat (`follow` false: a symlink itself).
pub fn sys_stat(path: u64, out: &mut [u8], follow: bool) -> i64 {
    let abs = match user_path(path) {
        Ok(p) => absolute(&p),
        Err(e) => return -e,
    };
    let found = if follow { mounts::stat(&abs) } else { mounts::lstat(&abs) };
    match found {
        Ok((loc, _)) => stat_loc(loc, out),
        Err(e) => -e,
    }
}

/// newfstatat(dirfd, path, statbuf, flags)
pub fn sys_fstatat(dirfd: u64, path: u64, out: &mut [u8], flags: u64) -> i64 {
    const AT_EMPTY_PATH: u64 = 0x1000;
    // fstatat(fd, "", buf, AT_EMPTY_PATH) is fstat(fd).
    if flags & AT_EMPTY_PATH != 0 && user_string(path).is_ok_and(|p| p.is_empty()) {
        return sys_fstat(dirfd, out);
    }
    let abs = match user_at_path(dirfd, path) {
        Ok(p) => p,
        Err(e) => return -e,
    };
    let found = if flags & AT_SYMLINK_NOFOLLOW != 0 { mounts::lstat(&abs) } else { mounts::stat(&abs) };
    match found {
        Ok((loc, _)) => stat_loc(loc, out),
        Err(e) => -e,
    }
}

/// getdents64(fd, dirp, count): fill `out` with linux_dirent64 records.
pub fn sys_getdents64(fd: u64, out: &mut [u8]) -> i64 {
    let h = match get(fd) {
        Ok(h) => h,
        Err(e) => return -e,
    };
    let mut desc = h.lock();
    let (loc, path) = match &desc.kind {
        Kind::Dir { loc, path } => (*loc, path.clone()),
        _ => return -ENOTDIR,
    };
    let pos = desc.offset;

    let parent = mounts::resolve(&filesystem::normalize(&path, "..")).map(|l| l.ino).unwrap_or(loc.ino);
    let mut entries: Vec<(String, u64, u8)> = vec![(".".into(), loc.ino, DT_DIR), ("..".into(), parent, DT_DIR)];
    match mounts::with_fs(loc.mount, |fs| fs.readdir(loc.ino)) {
        Ok(children) => entries.extend(children.into_iter().map(|(name, ino, kind)| {
            let t = match kind {
                FileType::Dir => DT_DIR,
                FileType::File => DT_REG,
                FileType::Symlink => DT_LNK,
                FileType::CharDevice(_) => DT_CHR,
            };
            (name, ino, t)
        })),
        Err(e) => return -e,
    }

    // struct linux_dirent64 { u64 d_ino; i64 d_off; u16 d_reclen; u8 d_type; char d_name[]; }
    let mut written = 0usize;
    let mut next = pos;
    for (name, ino, d_type) in entries.iter().skip(pos) {
        let reclen = (19 + name.len() + 1).next_multiple_of(8);
        if written + reclen > out.len() {
            if written == 0 {
                return -EINVAL; // buffer too small for even one entry
            }
            break;
        }
        let rec = &mut out[written..written + reclen];
        rec.fill(0);
        rec[0..8].copy_from_slice(&ino.to_le_bytes());
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
    match mounts::stat(&abs) {
        Ok((_, meta)) if meta.kind == FileType::Dir => {
            if let Err(e) = perm::check(&meta, &crate::process::current_cred(), perm::EXEC) {
                return -e;
            }
            with_process(|p| p.cwd = abs);
            0
        }
        Ok(_) => -ENOTDIR,
        Err(e) => -e,
    }
}

/// mkdir(path, mode) / mkdirat(dirfd, path, mode)
pub fn sys_mkdirat(dirfd: u64, path: u64, mode: u64) -> i64 {
    let abs = match user_at_path(dirfd, path) {
        Ok(p) => p,
        Err(e) => return -e,
    };
    let perm = mode as u32 & 0o7777 & !with_process(|p| p.umask);
    match mounts::create(&abs, FileType::Dir, perm) {
        Ok(_) => 0,
        Err(e) => -e,
    }
}

/// unlink(path) for files, rmdir(path) for directories.
pub fn sys_remove(path: u64, want_dir: bool) -> i64 {
    sys_unlinkat(AT_FDCWD as u64, path, if want_dir { AT_REMOVEDIR } else { 0 })
}

/// unlinkat(dirfd, path, flags): AT_REMOVEDIR means rmdir.
pub fn sys_unlinkat(dirfd: u64, path: u64, flags: u64) -> i64 {
    let abs = match user_at_path(dirfd, path) {
        Ok(p) => p,
        Err(e) => return -e,
    };
    match mounts::remove(&abs, flags & AT_REMOVEDIR != 0) {
        Ok(()) => 0,
        Err(e) => -e,
    }
}

/// renameat(olddirfd, old, newdirfd, new) (rename and renameat2 too)
pub fn sys_renameat(olddirfd: u64, old: u64, newdirfd: u64, new: u64, flags: u64) -> i64 {
    if flags != 0 {
        return -EINVAL; // RENAME_NOREPLACE etc. aren't supported
    }
    let (old, new) = match (user_at_path(olddirfd, old), user_at_path(newdirfd, new)) {
        (Ok(o), Ok(n)) => (o, n),
        (Err(e), _) | (_, Err(e)) => return -e,
    };
    match mounts::rename(&old, &new) {
        Ok(()) => 0,
        Err(e) => -e,
    }
}

/// linkat(olddirfd, old, newdirfd, new, flags) (and link)
pub fn sys_linkat(olddirfd: u64, old: u64, newdirfd: u64, new: u64, flags: u64) -> i64 {
    let (mut old, new) = match (user_at_path(olddirfd, old), user_at_path(newdirfd, new)) {
        (Ok(o), Ok(n)) => (o, n),
        (Err(e), _) | (_, Err(e)) => return -e,
    };
    if flags & AT_SYMLINK_FOLLOW != 0 {
        match mounts::resolve_full(&old, true) {
            Ok((_, _, real)) => old = real,
            Err(e) => return -e,
        }
    }
    match mounts::link(&old, &new) {
        Ok(()) => 0,
        Err(e) => -e,
    }
}

/// symlinkat(target, newdirfd, linkpath) (and symlink)
pub fn sys_symlinkat(target: u64, newdirfd: u64, linkpath: u64) -> i64 {
    let target = match user_cstr(target) {
        Ok(t) => t,
        Err(e) => return -e,
    };
    let abs = match user_at_path(newdirfd, linkpath) {
        Ok(p) => p,
        Err(e) => return -e,
    };
    match mounts::symlink(&target, &abs) {
        Ok(()) => 0,
        Err(e) => -e,
    }
}

/// readlinkat(dirfd, path, buf, size) (and readlink): the target, not
/// NUL-terminated, cut to fit.
pub fn sys_readlinkat(dirfd: u64, path: u64, out: &mut [u8]) -> i64 {
    let abs = match user_at_path(dirfd, path) {
        Ok(p) => p,
        Err(e) => return -e,
    };
    let (loc, meta) = match mounts::lstat(&abs) {
        Ok(found) => found,
        Err(e) => return -e,
    };
    if meta.kind != FileType::Symlink {
        return -EINVAL;
    }
    match mounts::with_fs(loc.mount, |fs| fs.readlink(loc.ino)) {
        Ok(target) => {
            let n = target.len().min(out.len());
            out[..n].copy_from_slice(&target.as_bytes()[..n]);
            n as i64
        }
        Err(e) => -e,
    }
}

/// fchmodat(dirfd, path, mode) (and chmod)
pub fn sys_fchmodat(dirfd: u64, path: u64, mode: u64) -> i64 {
    let abs = match user_at_path(dirfd, path) {
        Ok(p) => p,
        Err(e) => return -e,
    };
    match mounts::stat(&abs) {
        Ok((loc, meta)) => chmod_loc(loc, &meta, mode),
        Err(e) => -e,
    }
}

/// chmod with the permission rules: only the owner (or root) may change
/// the mode, and only root may keep setuid/setgid bits on others' groups.
fn chmod_loc(loc: Loc, meta: &mounts::Meta, mode: u64) -> i64 {
    let cred = crate::process::current_cred();
    if let Err(e) = perm::check_owner(meta, &cred) {
        return -e;
    }
    let mut mode = mode as u32 & 0o7777;
    if !cred.is_root() && meta.kind != FileType::Dir && !cred.in_group(meta.gid) {
        mode &= !perm::S_ISGID;
    }
    match mounts::with_fs(loc.mount, |fs| fs.chmod(loc.ino, mode)) {
        Ok(()) => 0,
        Err(e) => -e,
    }
}

/// The file an fd refers to (for fchmod, futimens, fstatfs).
fn fd_loc(fd: u64) -> Result<Option<Loc>, i64> {
    let h = get(fd)?;
    let desc = h.lock();
    Ok(match &desc.kind {
        Kind::File { loc, .. } | Kind::Dir { loc, .. } => Some(*loc),
        _ => None,
    })
}

/// fchmod(fd, mode)
pub fn sys_fchmod(fd: u64, mode: u64) -> i64 {
    match fd_loc(fd) {
        Ok(Some(loc)) => match mounts::with_fs(loc.mount, |fs| fs.meta(loc.ino)) {
            Ok(meta) => chmod_loc(loc, &meta, mode),
            Err(e) => -e,
        },
        Ok(None) => -EINVAL,
        Err(e) => -e,
    }
}

/// faccessat(dirfd, path, mode) (and access): may the real user read,
/// write or execute the file?
pub fn sys_faccessat(dirfd: u64, path: u64, mode: u64) -> i64 {
    const X_OK: u64 = 1;
    if mode & !7 != 0 {
        return -EINVAL;
    }
    let abs = match user_at_path(dirfd, path) {
        Ok(p) => p,
        Err(e) => return -e,
    };
    match mounts::stat(&abs) {
        Ok((_, meta)) => {
            // access() checks with the *real* IDs (who started the program),
            // so a setuid program can ask "may my user do this?".
            let mut cred = crate::process::current_cred();
            cred.euid = cred.ruid;
            cred.egid = cred.rgid;
            let want = (mode & 7) as u32 | if mode & X_OK != 0 { perm::EXEC } else { 0 };
            match perm::check(&meta, &cred, want) {
                Ok(()) => 0,
                Err(e) => -e,
            }
        }
        Err(e) => -e,
    }
}

/// umask(mask): set the creation mask, returning the old one.
pub fn sys_umask(mask: u64) -> i64 {
    with_process(|p| core::mem::replace(&mut p.umask, mask as u32 & 0o777)) as i64
}

/// statfs(path, buf) / fstatfs(fd, buf): struct statfs (120 bytes).
pub fn sys_statfs(path: Option<u64>, fd: u64, out: &mut [u8]) -> i64 {
    let loc = match path {
        Some(p) => match user_path(p).and_then(|p| mounts::resolve(&absolute(&p))) {
            Ok(l) => l,
            Err(e) => return -e,
        },
        None => match fd_loc(fd) {
            Ok(Some(l)) => l,
            Ok(None) => return -EINVAL,
            Err(e) => return -e,
        },
    };
    let (stats, name) = mounts::with_fs(loc.mount, |fs| (fs.statfs(), fs.name()));
    let fs_type: u64 = match name {
        "ext2" => 0xEF53,
        "memfs" => 0x01021994, // tmpfs
        _ => 0x1373,           // devfs
    };
    out.fill(0);
    let put = |out: &mut [u8], off: usize, v: u64| out[off..off + 8].copy_from_slice(&v.to_le_bytes());
    put(out, 0, fs_type);
    put(out, 8, stats.block_size);
    put(out, 16, stats.blocks);
    put(out, 24, stats.free_blocks);
    put(out, 32, stats.free_blocks); // available to unprivileged users
    put(out, 40, stats.files);
    put(out, 48, stats.free_files);
    put(out, 56, loc.mount as u64); // f_fsid
    put(out, 64, 255); // f_namelen
    put(out, 72, stats.block_size); // f_frsize
    0
}

pub const STATFS_SIZE: u64 = 120;

/// utimensat(dirfd, path, times, flags): set the modification time (to
/// now if `times` is NULL). With a NULL path it applies to dirfd itself.
pub fn sys_utimensat(dirfd: u64, path: u64, times: u64, flags: u64) -> i64 {
    const UTIME_NOW: i64 = (1 << 30) - 1;
    const UTIME_OMIT: i64 = (1 << 30) - 2;
    let mtime = if times == 0 {
        Some(crate::rtc::now())
    } else {
        let Some(t) = (unsafe { crate::syscall::user_slice(times, 32) }) else { return -EFAULT };
        let sec = i64::from_le_bytes(t[16..24].try_into().unwrap());
        let nsec = i64::from_le_bytes(t[24..32].try_into().unwrap());
        match nsec {
            UTIME_OMIT => None,
            UTIME_NOW => Some(crate::rtc::now()),
            _ if sec < 0 => return -EINVAL,
            _ => Some(sec as u64),
        }
    };
    let explicit = times != 0;
    let loc = if path == 0 {
        match fd_loc(dirfd) {
            Ok(Some(l)) => l,
            Ok(None) => return -EINVAL,
            Err(e) => return -e,
        }
    } else {
        let abs = match user_at_path(dirfd, path) {
            Ok(p) => p,
            Err(e) => return -e,
        };
        let found = if flags & AT_SYMLINK_NOFOLLOW != 0 { mounts::lstat(&abs) } else { mounts::stat(&abs) };
        match found {
            Ok((l, _)) => l,
            Err(e) => return -e,
        }
    };
    // Setting a time needs ownership; setting it to "now" is also allowed
    // with write permission (like Linux).
    let meta = match mounts::with_fs(loc.mount, |fs| fs.meta(loc.ino)) {
        Ok(m) => m,
        Err(e) => return -e,
    };
    let cred = crate::process::current_cred();
    if perm::check_owner(&meta, &cred).is_err() && (explicit || !perm::allowed(&meta, &cred, perm::WRITE)) {
        return if explicit { -EPERM } else { -EACCES };
    }
    match mtime {
        Some(t) => match mounts::with_fs(loc.mount, |fs| fs.set_mtime(loc.ino, t)) {
            Ok(()) => 0,
            Err(e) => -e,
        },
        None => 0,
    }
}

/// chown rules: only root may give a file to someone else; the owner may
/// change its group to one of their own groups. A non-root chown clears
/// the setuid/setgid bits (so you can't hand someone a setuid program).
fn chown_loc(loc: Loc, uid: u32, gid: u32) -> i64 {
    const KEEP: u32 = u32::MAX;
    let meta = match mounts::with_fs(loc.mount, |fs| fs.meta(loc.ino)) {
        Ok(m) => m,
        Err(e) => return -e,
    };
    let cred = crate::process::current_cred();
    let new_uid = if uid == KEEP { meta.uid } else { uid };
    let new_gid = if gid == KEEP { meta.gid } else { gid };
    if !cred.is_root() {
        if cred.euid != meta.uid || new_uid != meta.uid || (new_gid != meta.gid && !cred.in_group(new_gid)) {
            return -EPERM;
        }
    }
    if let Err(e) = mounts::with_fs(loc.mount, |fs| fs.chown(loc.ino, new_uid, new_gid)) {
        return -e;
    }
    if !cred.is_root() && meta.kind == FileType::File && meta.perm & (perm::S_ISUID | perm::S_ISGID) != 0 {
        let _ = mounts::with_fs(loc.mount, |fs| fs.chmod(loc.ino, meta.perm & !(perm::S_ISUID | perm::S_ISGID)));
    }
    0
}

/// fchownat(dirfd, path, uid, gid, flags) (and chown, lchown)
pub fn sys_fchownat(dirfd: u64, path: u64, uid: u64, gid: u64, flags: u64) -> i64 {
    let abs = match user_at_path(dirfd, path) {
        Ok(p) => p,
        Err(e) => return -e,
    };
    let found = if flags & AT_SYMLINK_NOFOLLOW != 0 { mounts::lstat(&abs) } else { mounts::stat(&abs) };
    match found {
        Ok((loc, _)) => chown_loc(loc, uid as u32, gid as u32),
        Err(e) => -e,
    }
}

/// fchown(fd, uid, gid)
pub fn sys_fchown(fd: u64, uid: u64, gid: u64) -> i64 {
    match fd_loc(fd) {
        Ok(Some(loc)) => chown_loc(loc, uid as u32, gid as u32),
        Ok(None) => -EINVAL,
        Err(e) => -e,
    }
}

/// sync(): write everything cached to the disks.
pub fn sys_sync() -> i64 {
    mounts::sync_all();
    0
}

pub const POLLIN: i16 = 0x1;
pub const POLLOUT: i16 = 0x4;
pub const POLLERR: i16 = 0x8;
pub const POLLHUP: i16 = 0x10;
pub const POLLNVAL: i16 = 0x20;

/// What `fd` is ready for, out of `events` (plus POLLERR/POLLHUP/POLLNVAL,
/// which are always reported).
pub fn poll_ready(fd: i32, events: i16) -> i16 {
    if fd < 0 {
        return 0;
    }
    let Ok(h) = get(fd as u64) else { return POLLNVAL };
    let kind_ready = match &h.lock().kind {
        Kind::Console => {
            let input = events & POLLIN != 0 && crate::tty::input_ready();
            (if input { POLLIN } else { 0 }) | POLLOUT
        }
        Kind::PipeRead(r) => match pipe::read_status(&r.pipe()) {
            (true, gone) => POLLIN | if gone { POLLHUP } else { 0 },
            (false, true) => POLLHUP,
            (false, false) => 0,
        },
        Kind::PipeWrite(w) => match pipe::write_status(&w.pipe()) {
            (_, true) => POLLERR,
            (room, false) => if room { POLLOUT } else { 0 },
        },
        // Files and devices never block.
        _ => POLLIN | POLLOUT,
    };
    kind_ready & (events | POLLERR | POLLHUP | POLLNVAL)
}

/// ftruncate(fd, length)
pub fn sys_ftruncate(fd: u64, len: u64) -> i64 {
    let h = match get(fd) {
        Ok(h) => h,
        Err(e) => return -e,
    };
    let (loc, writable) = match &h.lock().kind {
        Kind::File { loc, writable, .. } => (*loc, *writable),
        _ => return -EINVAL,
    };
    if !writable {
        return -EINVAL;
    }
    match mounts::with_fs(loc.mount, |fs| fs.truncate(loc.ino, len)) {
        Ok(()) => 0,
        Err(e) => -e,
    }
}

/// truncate(path, length)
pub fn sys_truncate(path: u64, len: u64) -> i64 {
    let fd = sys_open(path, O_WRONLY as u64, 0);
    if fd < 0 {
        return fd;
    }
    let r = sys_ftruncate(fd as u64, len);
    sys_close(fd as u64);
    r
}

/// statx(dirfd, path, flags, mask, buf): stat with Linux's newer, larger
/// struct (filled from the same information as stat).
pub fn sys_statx(dirfd: u64, path: u64, flags: u64, _mask: u64, buf: u64) -> i64 {
    let mut st = [0u8; STAT_SIZE as usize];
    let r = sys_fstatat(dirfd, path, &mut st, flags);
    if r < 0 {
        return r;
    }
    let Some(out) = (unsafe { crate::syscall::user_slice_mut(buf, 256) }) else { return -crate::syscall::EFAULT };
    let u64_at = |o: usize| u64::from_le_bytes(st[o..o + 8].try_into().unwrap());
    let u32_at = |o: usize| u32::from_le_bytes(st[o..o + 4].try_into().unwrap());
    out.fill(0);
    let put32 = |out: &mut [u8], o: usize, v: u32| out[o..o + 4].copy_from_slice(&v.to_le_bytes());
    let put64 = |out: &mut [u8], o: usize, v: u64| out[o..o + 8].copy_from_slice(&v.to_le_bytes());
    put32(out, 0, 0x7ff); // stx_mask: STATX_BASIC_STATS
    put32(out, 4, u64_at(56) as u32); // blksize
    put32(out, 16, u64_at(16) as u32); // nlink
    put32(out, 20, u32_at(28)); // uid
    put32(out, 24, u32_at(32)); // gid
    out[28..30].copy_from_slice(&(u32_at(24) as u16).to_le_bytes()); // mode
    put64(out, 32, u64_at(8)); // ino
    put64(out, 40, u64_at(48)); // size
    put64(out, 48, u64_at(64)); // blocks
    for (stx, st_off) in [(64, 72), (96, 104), (112, 88)] {
        // atime, ctime, mtime: { i64 sec, u32 nsec }
        put64(out, stx, u64_at(st_off));
        put32(out, stx + 8, u64_at(st_off + 8) as u32);
    }
    let dev = u64_at(0);
    put32(out, 136, (dev >> 8) as u32); // dev major
    put32(out, 140, (dev & 0xff) as u32); // dev minor
    0
}

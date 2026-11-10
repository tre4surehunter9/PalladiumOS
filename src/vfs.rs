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
use crate::net::socket::{self as sock, Socket};
use crate::net::netlink::NetlinkSocket;
use crate::net::unix::{self, UnixSocket};
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
pub const O_NONBLOCK: u64 = 0o4000;
pub const O_DIRECTORY: u64 = 0o200000;
pub const O_CLOEXEC: u64 = 0o2000000;
pub const O_NOFOLLOW: u64 = 0o400000;
pub const O_NOCTTY: u64 = 0o400;

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
    /// /dev/random, /dev/urandom: reads are random bytes, writes discarded.
    Random,
    PipeRead(PipeReader),
    PipeWrite(PipeWriter),
    /// A socket: internet (net::socket) or Unix-domain (net::unix).
    Socket(AnySock),
    /// A pseudo-terminal end: /dev/ptmx (master) or /dev/pts/N (slave).
    /// `loc` is its /dev entry (for fstat).
    Pty { end: Arc<crate::pty::End>, loc: Loc },
    /// eventfd: a counter that wakes readers when it's not zero.
    EventFd(Arc<Mutex<EventFd>>),
    /// epoll: a list of fds to watch.
    Epoll(Arc<Mutex<Vec<EpollEntry>>>),
    /// timerfd: readable when its timer has expired.
    TimerFd(Arc<Mutex<TimerFd>>),
}

struct EventFd {
    count: u64,
    /// EFD_SEMAPHORE: each read takes 1 instead of everything.
    semaphore: bool,
}

#[derive(Clone, Copy)]
struct EpollEntry {
    fd: i32,
    events: u32,
    data: u64,
}

/// An open file description.
struct Description {
    kind: Kind,
    /// Byte offset for files; for directories, the number of entries
    /// getdents64 has returned ("." and ".." first).
    offset: usize,
    /// O_NONBLOCK, for pipes (sockets and terminals keep their own).
    nonblocking: bool,
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
        Kind::Random => "/dev/urandom".to_string(),
        Kind::Socket(s) => alloc::format!("socket:[{}]", s.id()),
        Kind::Pty { end, .. } if end.master => "/dev/ptmx".to_string(),
        Kind::Pty { end, .. } => alloc::format!("/dev/pts/{}", end.pty.index),
        Kind::File { path, .. } | Kind::Dir { path, .. } => path.clone(),
        Kind::PipeRead(r) => alloc::format!("pipe:[{}]", pipe::id(&r.pipe())),
        Kind::PipeWrite(w) => alloc::format!("pipe:[{}]", pipe::id(&w.pipe())),
        Kind::EventFd(_) => "anon_inode:[eventfd]".to_string(),
        Kind::Epoll(_) => "anon_inode:[eventpoll]".to_string(),
        Kind::TimerFd(_) => "anon_inode:[timerfd]".to_string(),
    };
    Arc::new(OpenFile { label, desc: Mutex::new(Description { kind, offset: 0, nonblocking: false }) })
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
    /// The root directory (chroot), as a real path: absolute paths start
    /// here. "/" normally.
    pub root: String,
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
        Files { fds, cwd: cwd.to_string(), root: "/".to_string(), umask: 0o022 }
    }

    /// A table with nothing open (for exiting processes).
    pub fn empty() -> Self {
        Files { fds: vec![None; MAX_FDS], cwd: "/".to_string(), root: "/".to_string(), umask: 0o022 }
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

/// Resolve a user path to a real absolute path: relative to the process's
/// working directory, inside its root directory (chroot; ".." stops there).
pub fn absolute(path: &str) -> String {
    let (cwd, root) = with_process(|p| (p.cwd.clone(), p.root.clone()));
    if root == "/" {
        return filesystem::normalize(&cwd, path);
    }
    // (A working directory outside the root counts as the root.)
    let cwd_inside = filesystem::strip_root(&root, &cwd).unwrap_or_else(|| "/".to_string());
    filesystem::with_root(&root, &filesystem::normalize(&cwd_inside, path))
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
        FileType::CharDevice(dev) => {
            let nonblocking = flags & O_NONBLOCK != 0;
            match dev {
                Device::Null => Kind::Null,
                Device::Zero => Kind::Zero,
                Device::Random => Kind::Random,
                Device::Console => Kind::Console,
                Device::Ptmx => match crate::pty::open_master(nonblocking) {
                    // (its /dev/pts/N entry, for fstat and ttyname)
                    Ok(end) => {
                        let loc = mounts::resolve(&alloc::format!("/dev/pts/{}", end.pty.index)).unwrap_or(loc);
                        Kind::Pty { end: Arc::new(end), loc }
                    }
                    Err(e) => return -e,
                },
                Device::Pts(n) => match crate::pty::open_slave(n, nonblocking, flags & O_NOCTTY != 0) {
                    Ok(end) => Kind::Pty { end: Arc::new(end), loc },
                    Err(e) => return -e,
                },
                // /dev/tty: the caller's controlling terminal.
                Device::Tty => match crate::process::current_session().2 {
                    crate::process::CTTY_CONSOLE => Kind::Console,
                    crate::process::CTTY_NONE => return -crate::syscall::ENXIO,
                    ctty => {
                        let Some(pty) = crate::pty::get(ctty - 1) else { return -crate::syscall::ENXIO };
                        let end = crate::pty::reopen_slave(&pty);
                        end.set_nonblocking(nonblocking);
                        let loc = mounts::resolve(&alloc::format!("/dev/pts/{}", pty.index)).unwrap_or(loc);
                        Kind::Pty { end: Arc::new(end), loc }
                    }
                },
            }
        }
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
    // The request is an unsigned int: libc may pass it sign-extended
    // (TIOCGPTN is 0x80045430), so only the low 32 bits count, as on Linux.
    let request = request & 0xffff_ffff;
    const FIONREAD: u64 = 0x541B;
    const FIONBIO: u64 = 0x5421;
    match get(fd) {
        Ok(h) if matches!(h.lock().kind, Kind::Console) => {}
        Ok(h) if matches!(h.lock().kind, Kind::Pty { .. }) => {
            // (The description isn't held while it runs: TIOCSWINSZ signals.)
            let Kind::Pty { end, .. } = &h.lock().kind else { unreachable!() };
            let end = end.clone();
            return crate::pty::ioctl(&end, request, arg);
        }
        Ok(h) => {
            // Pipes: non-blocking mode, and bytes waiting.
            {
                let mut desc = h.lock();
                let pipe = match &desc.kind {
                    Kind::PipeRead(r) => Some(r.pipe()),
                    Kind::PipeWrite(w) => Some(w.pipe()),
                    _ => None,
                };
                if let Some(pipe) = pipe {
                    return match request {
                        FIONBIO => match unsafe { user_slice(arg, 4) } {
                            Some(b) => {
                                desc.nonblocking = u32::from_le_bytes(b.try_into().unwrap()) != 0;
                                0
                            }
                            None => -EFAULT,
                        },
                        FIONREAD => match unsafe { user_slice_mut(arg, 4) } {
                            Some(out) => {
                                out.copy_from_slice(&(pipe::buffered(&pipe) as u32).to_le_bytes());
                                0
                            }
                            None => -EFAULT,
                        },
                        _ => -ENOTTY,
                    };
                }
            }
            // Sockets: non-blocking mode, bytes waiting, and the
            // interfaces (name, index, address, MTU, MAC...).
            let Kind::Socket(s) = &h.lock().kind else { return -ENOTTY };
            if let Some(req) = unsafe { user_slice_mut(arg, 40) } {
                let mut ifreq = [0u8; 40];
                ifreq.copy_from_slice(req);
                if let Some(r) = crate::net::netlink::interface_ioctl(request, &mut ifreq) {
                    return match r {
                        Ok(()) => {
                            req.copy_from_slice(&ifreq);
                            0
                        }
                        Err(e) => -e,
                    };
                }
            }
            return match request {
                FIONBIO => match unsafe { user_slice(arg, 4) } {
                    Some(b) => {
                        s.set_nonblocking(u32::from_le_bytes(b.try_into().unwrap()) != 0);
                        0
                    }
                    None => -EFAULT,
                },
                FIONREAD => match unsafe { user_slice_mut(arg, 4) } {
                    Some(out) => {
                        out.copy_from_slice(&(s.pending_bytes() as u32).to_le_bytes());
                        0
                    }
                    None => -EFAULT,
                },
                _ => -ENOTTY,
            };
        }
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
            let (rows, cols) = crate::vga_buffer::size();
            out[0..2].copy_from_slice(&(rows as u16).to_le_bytes()); // ws_row
            out[2..4].copy_from_slice(&(cols as u16).to_le_bytes()); // ws_col
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

/// close_range(first, last, flags): close fds first..=last, or with
/// CLOSE_RANGE_CLOEXEC mark them close-on-exec. (CLOSE_RANGE_UNSHARE: the
/// fd table is already this process's own.)
pub fn sys_close_range(first: u64, last: u64, flags: u64) -> i64 {
    const CLOSE_RANGE_UNSHARE: u64 = 2;
    const CLOSE_RANGE_CLOEXEC: u64 = 4;
    if flags & !(CLOSE_RANGE_UNSHARE | CLOSE_RANGE_CLOEXEC) != 0 || first > last {
        return -EINVAL;
    }
    let end = last.min(MAX_FDS as u64 - 1);
    if first > end {
        return 0;
    }
    // Dropped after the process table is unlocked (see sys_close).
    let closed: Vec<Slot> = with_process(|p| {
        let mut out = Vec::new();
        for fd in first as usize..=end as usize {
            if flags & CLOSE_RANGE_CLOEXEC != 0 {
                if let Some(s) = p.fds[fd].as_mut() {
                    s.cloexec = true;
                }
            } else if let Some(s) = p.fds[fd].take() {
                out.push(s);
            }
        }
        out
    });
    drop(closed);
    0
}

/// fallocate(fd, mode, offset, len): make room for [offset, offset + len)
/// in a file (mode 0 grows it; FALLOC_FL_KEEP_SIZE only reserves, which
/// ext2 here doesn't need).
pub fn sys_fallocate(fd: u64, mode: u64, offset: u64, len: u64) -> i64 {
    const FALLOC_FL_KEEP_SIZE: u64 = 1;
    if mode & !FALLOC_FL_KEEP_SIZE != 0 {
        return -EOPNOTSUPP; // punching holes, zeroing ranges...
    }
    if len == 0 {
        return -EINVAL;
    }
    let loc = match fd_loc(fd) {
        Ok(Some(loc)) => loc,
        Ok(None) => return -ENODEV,
        Err(e) => return -e,
    };
    let size = match mounts::with_fs(loc.mount, |fs| fs.meta(loc.ino)) {
        Ok(m) => m.size,
        Err(e) => return -e,
    };
    let Some(end) = offset.checked_add(len) else { return -EFBIG };
    if mode & FALLOC_FL_KEEP_SIZE == 0 && end > size {
        return sys_ftruncate(fd, end);
    }
    0
}

/// copy_file_range(in, off_in, out, off_out, len, flags): copy between
/// files in the kernel, 64 KiB at a time. A NULL offset means the fd's
/// own position (which moves); otherwise the offset is read and updated.
pub fn sys_copy_file_range(fd_in: u64, off_in: u64, fd_out: u64, off_out: u64, len: u64, flags: u64) -> i64 {
    if flags != 0 {
        return -EINVAL;
    }
    let read_off = |p: u64| -> Result<Option<u64>, i64> {
        if p == 0 {
            return Ok(None);
        }
        let b = unsafe { user_slice(p, 8) }.ok_or(EFAULT)?;
        Ok(Some(u64::from_le_bytes(b.try_into().unwrap())))
    };
    let (mut pos_in, mut pos_out) = match (read_off(off_in), read_off(off_out)) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => return -e,
    };
    let mut buf = vec![0u8; 64 * 1024];
    let mut total: u64 = 0;
    while total < len {
        let want = ((len - total) as usize).min(buf.len());
        let n = match pos_in {
            Some(p) => sys_pread(fd_in, &mut buf[..want], p),
            None => sys_read(fd_in, &mut buf[..want]),
        };
        if n < 0 {
            if total > 0 { break } else { return n }
        }
        if n == 0 {
            break; // end of the input
        }
        let mut written = 0usize;
        while written < n as usize {
            let w = match pos_out {
                Some(p) => sys_pwrite(fd_out, &buf[written..n as usize], p + written as u64),
                None => sys_write(fd_out, &buf[written..n as usize]),
            };
            if w <= 0 {
                if total + (written as u64) > 0 { break } else { return if w < 0 { w } else { -EIO } }
            }
            written += w as usize;
        }
        total += written as u64;
        if let Some(p) = pos_in.as_mut() {
            *p += written as u64;
        }
        if let Some(p) = pos_out.as_mut() {
            *p += written as u64;
        }
        if written < n as usize {
            break;
        }
    }
    for (ptr, pos) in [(off_in, pos_in), (off_out, pos_out)] {
        if let (true, Some(p)) = (ptr != 0, pos) {
            if let Some(b) = unsafe { user_slice_mut(ptr, 8) } {
                b.copy_from_slice(&p.to_le_bytes());
            }
        }
    }
    total as i64
}

/// memfd_create(name, flags): an anonymous file in memory. It's a file in
/// the in-memory /tmp, removed at once, so it lives as long as its fds.
/// (Limited to memory filesystems' 4 MiB per file.)
pub fn sys_memfd_create(name: u64, flags: u64) -> i64 {
    const MFD_CLOEXEC: u64 = 1;
    const MFD_ALLOW_SEALING: u64 = 2;
    use core::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    if flags & !(MFD_CLOEXEC | MFD_ALLOW_SEALING) != 0 {
        return -EINVAL;
    }
    let name = match user_path(name) {
        Ok(n) if n.len() <= 249 && !n.contains('/') => n,
        Ok(_) => return -EINVAL,
        Err(e) => return -e,
    };
    let path = alloc::format!("/tmp/memfd:{}.{}", name, NEXT.fetch_add(1, Ordering::Relaxed));
    let cloexec = if flags & MFD_CLOEXEC != 0 { O_CLOEXEC } else { 0 };
    let fd = open_path(&path, O_RDWR | O_CREAT | O_EXCL | cloexec, 0o600);
    if fd >= 0 {
        let _ = filesystem::remove(&path);
    }
    fd
}

/// pipe2(fds, flags): fds[0] is the read end, fds[1] the write end.
pub fn sys_pipe2(fds_out: u64, flags: u64) -> i64 {
    if flags & !(O_CLOEXEC | O_NONBLOCK) != 0 {
        return -EINVAL; // O_DIRECT (packet mode) isn't supported
    }
    let Some(out) = (unsafe { user_slice_mut(fds_out, 8) }) else { return -EFAULT };
    let cloexec = flags & O_CLOEXEC != 0;
    let (reader, writer) = pipe::new_pipe();
    let (rh, wh) = (handle(Kind::PipeRead(reader)), handle(Kind::PipeWrite(writer)));
    if flags & O_NONBLOCK != 0 {
        rh.lock().nonblocking = true;
        wh.lock().nonblocking = true;
    }
    let result = with_process(|p| {
        let r = alloc_fd(p, rh, cloexec)?;
        match alloc_fd(p, wh, cloexec) {
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
            F_GETFL => {
                let desc = slot.handle.lock();
                let nb = if desc.nonblocking { O_NONBLOCK } else { 0 };
                match &desc.kind {
                    Kind::File { readable, writable, append, .. } => {
                        let mode = match (readable, writable) {
                            (true, true) => O_RDWR,
                            (false, true) => O_WRONLY,
                            _ => O_RDONLY,
                        };
                        (mode | if *append { O_APPEND } else { 0 }) as i64
                    }
                    Kind::Console | Kind::Null | Kind::Zero | Kind::Random => O_RDWR as i64,
                    Kind::Socket(s) => (O_RDWR | if s.is_nonblocking() { O_NONBLOCK } else { 0 }) as i64,
                    Kind::Pty { end, .. } => (O_RDWR | if end.is_nonblocking() { O_NONBLOCK } else { 0 }) as i64,
                    Kind::PipeWrite(_) => (O_WRONLY | nb) as i64,
                    Kind::PipeRead(_) => (O_RDONLY | nb) as i64,
                    Kind::Dir { .. } => O_RDONLY as i64,
                    Kind::EventFd(_) | Kind::Epoll(_) | Kind::TimerFd(_) => (O_RDWR | nb) as i64,
                }
            }
            // Record locks: granted (see sys_flock). F_GETLK reports
            // "unlocked" (struct flock's l_type = F_UNLCK).
            5 | 36 => match unsafe { crate::syscall::user_slice_mut(arg, 2) } {
                Some(b) => {
                    b.copy_from_slice(&2i16.to_le_bytes());
                    0
                }
                None => -EFAULT,
            },
            6 | 7 | 37 | 38 => 0,
            F_SETFL => {
                // O_NONBLOCK works on pipes, sockets and terminals; other
                // changes are accepted.
                let mut desc = slot.handle.lock();
                match &desc.kind {
                    Kind::Socket(s) => s.set_nonblocking(arg & O_NONBLOCK != 0),
                    Kind::Pty { end, .. } => end.set_nonblocking(arg & O_NONBLOCK != 0),
                    _ => desc.nonblocking = arg & O_NONBLOCK != 0,
                }
                0
            }
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
        Kind::Pty { end, .. } => {
            let end = end.clone();
            drop(desc); // don't hold it while waiting
            if end.master { crate::pty::master_read(&end, buf) } else { crate::pty::slave_read(&end, buf) }
        }
        Kind::PipeRead(r) => {
            let pipe = r.pipe();
            let nonblocking = desc.nonblocking;
            drop(desc); // or while waiting for data
            if nonblocking && !buf.is_empty() && pipe::read_status(&pipe) == (false, false) {
                return -crate::syscall::EAGAIN; // empty, but writers remain
            }
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
        Kind::Random => {
            crate::random::fill(buf);
            buf.len() as i64
        }
        Kind::Socket(s) => {
            let s = s.clone();
            drop(desc); // don't hold it while waiting for data
            match s.recv(buf, 0) {
                Ok(n) => n as i64,
                Err(e) => -e,
            }
        }
        Kind::Dir { .. } => -EISDIR,
        Kind::PipeWrite(_) => -EBADF,
        Kind::EventFd(e) => {
            let e = e.clone();
            let nonblocking = desc.nonblocking;
            drop(desc);
            eventfd_read(&e, nonblocking, buf)
        }
        Kind::Epoll(_) => -EINVAL,
        Kind::TimerFd(t) => {
            let t = t.clone();
            let nonblocking = desc.nonblocking;
            drop(desc);
            timerfd_read(&t, nonblocking, buf)
        }
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
        Kind::Random => {
            crate::random::fill(buf);
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
        Kind::Pty { end, .. } => {
            let end = end.clone();
            drop(desc);
            if end.master { crate::pty::master_write(&end, bytes) } else { crate::pty::slave_write(&end, bytes) }
        }
        Kind::PipeWrite(w) => {
            let pipe = w.pipe();
            let nonblocking = desc.nonblocking;
            drop(desc);
            if nonblocking { pipe::try_write(&pipe, bytes) } else { pipe::write(&pipe, bytes) }
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
        Kind::Null | Kind::Zero | Kind::Random => bytes.len() as i64,
        Kind::Socket(s) => {
            let s = s.clone();
            drop(desc);
            match s.send(bytes, 0) {
                Ok(n) => n as i64,
                Err(e) => -e,
            }
        }
        Kind::Dir { .. } => -EISDIR,
        Kind::PipeRead(_) => -EBADF,
        Kind::EventFd(e) => {
            let e = e.clone();
            let nonblocking = desc.nonblocking;
            drop(desc);
            eventfd_write(&e, nonblocking, bytes)
        }
        Kind::Epoll(_) | Kind::TimerFd(_) => -EINVAL,
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
        Kind::Null | Kind::Zero | Kind::Random => 0,
        Kind::Console | Kind::PipeRead(_) | Kind::PipeWrite(_) | Kind::Socket(_) | Kind::Pty { .. } | Kind::EventFd(_) | Kind::Epoll(_) | Kind::TimerFd(_) => -ESPIPE,
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
        Kind::Console | Kind::Null | Kind::Zero | Kind::Random => {
            fill_stat(out, 0, 0, S_IFCHR | 0o666, 1, 0, 0);
            0
        }
        // The /dev/pts/N entry's stat, so ttyname() can match them up.
        Kind::Pty { loc, .. } => {
            let loc = *loc;
            drop(desc);
            stat_loc(loc, out)
        }
        Kind::Socket(s) => {
            fill_stat(out, 0, s.id(), 0o140000 | 0o777, 1, 0, 0); // S_IFSOCK
            0
        }
        Kind::PipeRead(_) | Kind::PipeWrite(_) => {
            fill_stat(out, 0, 0, S_IFIFO | 0o600, 1, 0, 0);
            0
        }
        Kind::EventFd(_) | Kind::Epoll(_) | Kind::TimerFd(_) => {
            fill_stat(out, 0, 0, 0o600, 1, 0, 0); // an anonymous inode
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
    // As seen from inside the root directory.
    let (cwd, root) = with_process(|p| (p.cwd.clone(), p.root.clone()));
    let cwd = filesystem::strip_root(&root, &cwd).unwrap_or_else(|| "/".to_string());
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

/// fchdir(fd): make the directory open as `fd` the working directory.
pub fn sys_fchdir(fd: u64) -> i64 {
    let path = match get(fd) {
        Ok(h) => match &h.lock().kind {
            Kind::Dir { path, .. } => path.clone(),
            _ => return -ENOTDIR,
        },
        Err(e) => return -e,
    };
    with_process(|p| p.cwd = path);
    0
}

/// chroot(path): absolute paths start at `path` from now on (root only).
/// The working directory doesn't change (programs chdir("/") next).
pub fn sys_chroot(path: u64) -> i64 {
    if crate::process::current_cred().euid != 0 {
        return -EPERM;
    }
    let abs = match user_path(path) {
        Ok(p) => absolute(&p),
        Err(e) => return -e,
    };
    match mounts::resolve_full(&abs, true) {
        Ok((_, meta, real)) if meta.kind == FileType::Dir => {
            if let Err(e) = perm::check(&meta, &crate::process::current_cred(), perm::EXEC) {
                return -e;
            }
            with_process(|p| p.root = real);
            0
        }
        Ok(_) => -ENOTDIR,
        Err(e) => -e,
    }
}

/// mount(source, target, fstype, flags, data): mount a new proc, devtmpfs
/// (PalladiumOS's /dev) or tmpfs filesystem at `target` (root only). Disks
/// are mounted by the kernel at boot.
pub fn sys_mount(_source: u64, target: u64, fstype: u64, flags: u64, _data: u64) -> i64 {
    const MS_REMOUNT: u64 = 32;
    const MS_BIND: u64 = 4096;
    if crate::process::current_cred().euid != 0 {
        return -EPERM;
    }
    let real = match user_path(target).map(|p| absolute(&p)).and_then(|abs| mounts::resolve_full(&abs, true)) {
        Ok((_, meta, real)) if meta.kind == FileType::Dir => real,
        Ok(_) => return -ENOTDIR,
        Err(e) => return -e,
    };
    if flags & MS_REMOUNT != 0 {
        return 0; // (options like read-only aren't changed)
    }
    if flags & MS_BIND != 0 {
        return -EINVAL;
    }
    let kind = match user_path(fstype) {
        Ok(k) => k,
        Err(e) => return -e,
    };
    let fs: alloc::boxed::Box<dyn mounts::FileSystem> = match kind.as_str() {
        "proc" => alloc::boxed::Box::new(crate::procfs::ProcFs),
        "devtmpfs" | "devfs" => alloc::boxed::Box::new(crate::devfs::DevFs),
        "tmpfs" => alloc::boxed::Box::new(crate::fdfs::Fdfs::new()),
        _ => return -ENODEV,
    };
    if mounts::list().iter().any(|(p, _)| *p == real) {
        return -EBUSY;
    }
    mounts::mount(&real, fs);
    0
}

/// umount2(target, flags) (root only).
pub fn sys_umount2(target: u64, _flags: u64) -> i64 {
    if crate::process::current_cred().euid != 0 {
        return -EPERM;
    }
    let real = match user_path(target).map(|p| absolute(&p)).and_then(|abs| mounts::resolve_full(&abs, true)) {
        Ok((_, _, real)) => real,
        Err(e) => return -e,
    };
    match mounts::unmount(&real) {
        Ok(fs) => {
            drop(fs);
            0
        }
        Err(e) => -e,
    }
}

/// flock(fd, op): always granted. With one user's programs on one CPU there
/// is nobody to wait for; programs (like apk) take the lock and carry on.
pub fn sys_flock(fd: u64, _op: u64) -> i64 {
    match get(fd) {
        Ok(_) => 0,
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
/// The regular file an fd refers to, and whether it's open for writing
/// (for shared file mappings).
pub fn fd_file(fd: u64) -> Result<(Loc, bool), i64> {
    match &get(fd)?.lock().kind {
        Kind::File { loc, writable, .. } => Ok((*loc, *writable)),
        _ => Err(ENODEV),
    }
}

fn fd_loc(fd: u64) -> Result<Option<Loc>, i64> {
    let h = get(fd)?;
    let desc = h.lock();
    Ok(match &desc.kind {
        Kind::File { loc, .. } | Kind::Dir { loc, .. } => Some(*loc),
        _ => None,
    })
}

/// fchmod(fd, mode)
/// fchmodat2(dirfd, path, mode, flags): fchmodat with flags.
/// AT_SYMLINK_NOFOLLOW: symlinks have no mode (EOPNOTSUPP, as on Linux);
/// AT_EMPTY_PATH: the fd itself.
pub fn sys_fchmodat2(dirfd: u64, path: u64, mode: u64, flags: u64) -> i64 {
    const AT_SYMLINK_NOFOLLOW: u64 = 0x100;
    const AT_EMPTY_PATH: u64 = 0x1000;
    if flags & !(AT_SYMLINK_NOFOLLOW | AT_EMPTY_PATH) != 0 {
        return -EINVAL;
    }
    if flags & AT_EMPTY_PATH != 0 && matches!(user_path(path), Ok(p) if p.is_empty()) {
        return sys_fchmod(dirfd, mode);
    }
    if flags & AT_SYMLINK_NOFOLLOW == 0 {
        return sys_fchmodat(dirfd, path, mode);
    }
    let abs = match user_at_path(dirfd, path) {
        Ok(p) => p,
        Err(e) => return -e,
    };
    match mounts::lstat(&abs) {
        Ok((_, meta)) if meta.kind == FileType::Symlink => -EOPNOTSUPP,
        Ok((loc, meta)) => chmod_loc(loc, &meta, mode),
        Err(e) => -e,
    }
}

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
    set_mtime_at(dirfd, path, mtime, times != 0, flags)
}

/// utimes(path, times) and utime(path, times): the older interfaces
/// (timevals; a utimbuf). Only the modification time is kept.
pub fn sys_utimes(path: u64, times: u64, utimbuf: bool) -> i64 {
    let mtime = if times == 0 {
        Some(crate::rtc::now())
    } else {
        // utimbuf: { actime, modtime }; timeval[2]: { sec, usec } x 2.
        let (off, len) = if utimbuf { (8, 16) } else { (16, 32) };
        let Some(t) = (unsafe { crate::syscall::user_slice(times, len) }) else { return -EFAULT };
        let sec = i64::from_le_bytes(t[off..off + 8].try_into().unwrap());
        if sec < 0 {
            return -EINVAL;
        }
        Some(sec as u64)
    };
    set_mtime_at(FDCWD_VALUE, path, mtime, times != 0, 0)
}

const FDCWD_VALUE: u64 = -100i64 as u64;

/// Set a file's modification time (None: leave it), with utimensat's
/// permission rules (`explicit`: a given time, not "now").
fn set_mtime_at(dirfd: u64, path: u64, mtime: Option<u64>, explicit: bool, flags: u64) -> i64 {
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
        Kind::Socket(s) => {
            let (r, w, hup) = s.readiness();
            (if r { POLLIN } else { 0 }) | (if w { POLLOUT } else { 0 }) | (if hup { POLLHUP } else { 0 })
        }
        Kind::Pty { end, .. } => {
            let (r, w, hup) = crate::pty::readiness(end);
            (if r { POLLIN } else { 0 }) | (if w { POLLOUT } else { 0 }) | (if hup { POLLHUP } else { 0 })
        }
        Kind::EventFd(e) => (if e.lock().count > 0 { POLLIN } else { 0 }) | POLLOUT,
        Kind::TimerFd(t) => {
            let mut t = t.lock();
            t.update(crate::process::ticks());
            if t.expired > 0 { POLLIN } else { 0 }
        }
        Kind::Epoll(list) => {
            // Readable when something it watches is ready (epolls inside
            // epolls aren't looked into).
            let list = list.lock().clone();
            let any = list.iter().any(|e| {
                e.events & EPOLL_EVENTS != 0 && get(e.fd as u64).is_ok_and(|h| !matches!(h.lock().kind, Kind::Epoll(_)))
                    && poll_ready(e.fd, (e.events & EPOLL_EVENTS) as i16) != 0
            });
            if any { POLLIN } else { 0 }
        }
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

// ---- sockets ----

/// A socket descriptor's socket: internet or Unix-domain.
#[derive(Clone)]
pub enum AnySock {
    Inet(Arc<Socket>),
    Unix(Arc<UnixSocket>),
    /// Netlink (read-only: interface and address lists)
    Netlink(Arc<NetlinkSocket>),
}

impl AnySock {
    fn id(&self) -> u64 {
        match self {
            AnySock::Inet(s) => s.id,
            AnySock::Unix(s) => s.id,
            AnySock::Netlink(s) => s.id,
        }
    }
    fn is_nonblocking(&self) -> bool {
        match self {
            AnySock::Inet(s) => s.is_nonblocking(),
            AnySock::Unix(s) => s.is_nonblocking(),
            AnySock::Netlink(s) => s.is_nonblocking(),
        }
    }
    fn set_nonblocking(&self, on: bool) {
        match self {
            AnySock::Inet(s) => s.set_nonblocking(on),
            AnySock::Unix(s) => s.set_nonblocking(on),
            AnySock::Netlink(s) => s.set_nonblocking(on),
        }
    }
    fn readiness(&self) -> (bool, bool, bool) {
        match self {
            AnySock::Inet(s) => s.readiness(),
            AnySock::Unix(s) => s.readiness(),
            AnySock::Netlink(s) => s.readiness(),
        }
    }
    fn pending_bytes(&self) -> usize {
        match self {
            AnySock::Inet(s) => s.pending_bytes(),
            AnySock::Unix(s) => s.pending_bytes(),
            AnySock::Netlink(s) => s.pending_bytes(),
        }
    }
    /// read()
    fn recv(&self, buf: &mut [u8], flags: u64) -> Result<usize, i64> {
        match self {
            AnySock::Inet(s) => s.recv(buf, flags).map(|(n, _)| n),
            AnySock::Unix(s) => s.recv(buf, flags).map(|(n, _)| n),
            AnySock::Netlink(s) => s.recv(buf, flags),
        }
    }
    /// write()
    fn send(&self, data: &[u8], flags: u64) -> Result<usize, i64> {
        match self {
            AnySock::Inet(s) => s.send(data, flags, None),
            AnySock::Unix(s) => s.send(data, flags, None),
            AnySock::Netlink(s) => s.send(data),
        }
    }
}

/// The socket behind `fd`.
fn any_socket(fd: u64) -> Result<AnySock, i64> {
    match &get(fd)?.lock().kind {
        Kind::Socket(s) => Ok(s.clone()),
        _ => Err(ENOTSOCK),
    }
}

/// struct sockaddr_in -> (address, port).
fn read_sockaddr(ptr: u64, len: u64) -> Result<(smoltcp::wire::Ipv4Address, u16), i64> {
    if len < 8 {
        return Err(EINVAL);
    }
    let b = unsafe { crate::syscall::user_slice(ptr, 8) }.ok_or(EFAULT)?;
    let family = u16::from_le_bytes([b[0], b[1]]) as u64;
    if family != sock::AF_INET {
        return Err(EAFNOSUPPORT);
    }
    let port = u16::from_be_bytes([b[2], b[3]]);
    Ok((smoltcp::wire::Ipv4Address::new(b[4], b[5], b[6], b[7]), port))
}

/// struct sockaddr_un -> a name: a path (up to its NUL), or an abstract
/// name (sun_path[0] == 0: the rest of the given length).
fn read_sockaddr_un(ptr: u64, len: u64) -> Result<unix::Name, i64> {
    if !(3..=110).contains(&len) {
        return Err(EINVAL);
    }
    let b = unsafe { crate::syscall::user_slice(ptr, len) }.ok_or(EFAULT)?;
    if u16::from_le_bytes([b[0], b[1]]) as u64 != unix::AF_UNIX {
        return Err(EAFNOSUPPORT);
    }
    let path = &b[2..];
    if path[0] == 0 {
        return Ok(unix::Name::Abstract(path[1..].to_vec()));
    }
    let end = path.iter().position(|&c| c == 0).unwrap_or(path.len());
    let given = String::from_utf8_lossy(&path[..end]).into_owned();
    let abs = absolute(&given);
    Ok(unix::Name::Path { given, abs })
}

/// Write a socket address to (ptr, *len_ptr) and set *len_ptr (NULL: skip).
fn write_addr(ptr: u64, len_ptr: u64, bytes: &[u8]) -> Result<(), i64> {
    if ptr == 0 || len_ptr == 0 {
        return Ok(());
    }
    let len_b = unsafe { crate::syscall::user_slice_mut(len_ptr, 4) }.ok_or(EFAULT)?;
    let room = u32::from_le_bytes(len_b[..4].try_into().unwrap()) as usize;
    len_b.copy_from_slice(&(bytes.len() as u32).to_le_bytes());
    let n = room.min(bytes.len());
    let out = unsafe { crate::syscall::user_slice_mut(ptr, n as u64) }.ok_or(EFAULT)?;
    out.copy_from_slice(&bytes[..n]);
    Ok(())
}

fn write_sockaddr(ptr: u64, len_ptr: u64, ep: smoltcp::wire::IpEndpoint) -> Result<(), i64> {
    let smoltcp::wire::IpAddress::Ipv4(a) = ep.addr;
    let mut sa = [0u8; 16];
    sa[0..2].copy_from_slice(&(sock::AF_INET as u16).to_le_bytes());
    sa[2..4].copy_from_slice(&ep.port.to_be_bytes());
    sa[4..8].copy_from_slice(&a.octets());
    write_addr(ptr, len_ptr, &sa)
}

/// A Unix socket's address (unnamed: just the family).
fn write_sockaddr_un(ptr: u64, len_ptr: u64, name: Option<&unix::Name>) -> Result<(), i64> {
    let mut sa = Vec::with_capacity(110);
    sa.extend_from_slice(&(unix::AF_UNIX as u16).to_le_bytes());
    match name {
        Some(unix::Name::Path { given, .. }) => {
            sa.extend_from_slice(given.as_bytes());
            sa.push(0);
        }
        Some(unix::Name::Abstract(b)) => {
            sa.push(0);
            sa.extend_from_slice(b);
        }
        None => {}
    }
    write_addr(ptr, len_ptr, &sa)
}

/// struct sockaddr_nl: family, port id (0: the kernel), no groups.
fn write_sockaddr_nl(ptr: u64, len_ptr: u64, port: u32) -> Result<(), i64> {
    let mut sa = [0u8; 12];
    sa[0..2].copy_from_slice(&(crate::net::netlink::AF_NETLINK as u16).to_le_bytes());
    sa[4..8].copy_from_slice(&port.to_le_bytes());
    write_addr(ptr, len_ptr, &sa)
}

fn result(r: Result<i64, i64>) -> i64 {
    r.unwrap_or_else(|e| -e)
}

fn new_socket_fd(s: AnySock, cloexec: bool) -> Result<i64, i64> {
    with_process(|p| alloc_fd(p, handle(Kind::Socket(s)), cloexec)).map(|fd| fd as i64)
}

/// socket(domain, type, protocol): AF_INET (TCP, UDP, raw) or AF_UNIX
/// (stream, datagram), with SOCK_NONBLOCK / SOCK_CLOEXEC.
pub fn sys_socket(domain: u64, ty: u64, protocol: u64) -> i64 {
    let cloexec = ty & sock::SOCK_CLOEXEC != 0;
    let nonblocking = ty & sock::SOCK_NONBLOCK != 0;
    result(match domain {
        sock::AF_INET => Socket::new(ty, protocol).and_then(|s| new_socket_fd(AnySock::Inet(Arc::new(s)), cloexec)),
        unix::AF_UNIX => match ty & 0xf {
            sock::SOCK_STREAM | sock::SOCK_DGRAM => {
                let s = UnixSocket::new(ty & 0xf == sock::SOCK_STREAM, nonblocking);
                new_socket_fd(AnySock::Unix(Arc::new(s)), cloexec)
            }
            _ => Err(crate::syscall::EPROTONOSUPPORT),
        },
        crate::net::netlink::AF_NETLINK => {
            if protocol != crate::net::netlink::NETLINK_ROUTE {
                Err(crate::syscall::EPROTONOSUPPORT)
            } else {
                new_socket_fd(AnySock::Netlink(Arc::new(NetlinkSocket::new(nonblocking))), cloexec)
            }
        }
        _ => Err(EAFNOSUPPORT), // no IPv6 yet
    })
}

/// socketpair(AF_UNIX, type, protocol, sv): two connected sockets.
pub fn sys_socketpair(domain: u64, ty: u64, _protocol: u64, sv: u64) -> i64 {
    result((|| {
        if domain != unix::AF_UNIX {
            return Err(crate::syscall::EOPNOTSUPP);
        }
        let stream = match ty & 0xf {
            sock::SOCK_STREAM => true,
            sock::SOCK_DGRAM => false,
            _ => return Err(crate::syscall::EPROTONOSUPPORT),
        };
        let out = unsafe { crate::syscall::user_slice_mut(sv, 8) }.ok_or(EFAULT)?;
        let (a, b) = UnixSocket::pair(stream, ty & sock::SOCK_NONBLOCK != 0);
        let cloexec = ty & sock::SOCK_CLOEXEC != 0;
        let fa = new_socket_fd(AnySock::Unix(a), cloexec)?;
        let fb = match new_socket_fd(AnySock::Unix(b), cloexec) {
            Ok(fd) => fd,
            Err(e) => {
                sys_close(fa as u64);
                return Err(e);
            }
        };
        out[0..4].copy_from_slice(&(fa as i32).to_le_bytes());
        out[4..8].copy_from_slice(&(fb as i32).to_le_bytes());
        Ok(0)
    })())
}

pub fn sys_bind(fd: u64, addr: u64, len: u64) -> i64 {
    result((|| match any_socket(fd)? {
        AnySock::Inet(s) => {
            let (a, port) = read_sockaddr(addr, len)?;
            s.bind(a, port).map(|_| 0)
        }
        AnySock::Unix(s) => s.bind(read_sockaddr_un(addr, len)?).map(|_| 0),
        AnySock::Netlink(_) => Ok(0), // any address is fine: replies come to us
    })())
}

pub fn sys_listen(fd: u64, backlog: u64) -> i64 {
    result((|| match any_socket(fd)? {
        AnySock::Inet(s) => s.listen(backlog as usize).map(|_| 0),
        AnySock::Unix(s) => s.listen().map(|_| 0),
        AnySock::Netlink(_) => Err(crate::syscall::EOPNOTSUPP),
    })())
}

pub fn sys_connect(fd: u64, addr: u64, len: u64) -> i64 {
    result((|| match any_socket(fd)? {
        AnySock::Inet(s) => {
            let (a, port) = read_sockaddr(addr, len)?;
            s.connect(a, port).map(|_| 0)
        }
        AnySock::Unix(s) => s.connect(read_sockaddr_un(addr, len)?).map(|_| 0),
        AnySock::Netlink(_) => Ok(0), // to the kernel: the only peer there is
    })())
}

/// accept4(fd, addr, addrlen, flags) (and accept)
pub fn sys_accept(fd: u64, addr: u64, len_ptr: u64, flags: u64) -> i64 {
    result((|| {
        let nonblocking = flags & sock::SOCK_NONBLOCK != 0;
        let cloexec = flags & sock::SOCK_CLOEXEC != 0;
        match any_socket(fd)? {
            AnySock::Inet(listener) => {
                let (conn, peer) = listener.accept(nonblocking)?;
                if let Some(peer) = peer {
                    write_sockaddr(addr, len_ptr, peer)?;
                }
                new_socket_fd(AnySock::Inet(Arc::new(conn)), cloexec)
            }
            AnySock::Unix(listener) => {
                let (conn, peer) = listener.accept(nonblocking)?;
                write_sockaddr_un(addr, len_ptr, peer.as_ref())?;
                new_socket_fd(AnySock::Unix(conn), cloexec)
            }
            AnySock::Netlink(_) => Err(crate::syscall::EOPNOTSUPP),
        }
    })())
}

/// sendto(fd, buf, len, flags, addr, addrlen) (and send)
pub fn sys_sendto(fd: u64, buf: u64, len: u64, flags: u64, addr: u64, alen: u64) -> i64 {
    result((|| {
        let data = unsafe { crate::syscall::user_slice(buf, len) }.ok_or(EFAULT)?;
        match any_socket(fd)? {
            AnySock::Inet(s) => {
                let to = if addr != 0 && !s.is_stream() {
                    // (raw sockets: the port in the address is ignored, as on Linux)
                    let (a, port) = read_sockaddr(addr, alen)?;
                    Some(smoltcp::wire::IpEndpoint::new(smoltcp::wire::IpAddress::Ipv4(a), port))
                } else {
                    None
                };
                s.send(data, flags, to).map(|n| n as i64)
            }
            AnySock::Unix(s) => {
                let to = if addr != 0 && !s.is_stream() { Some(read_sockaddr_un(addr, alen)?) } else { None };
                s.send(data, flags, to).map(|n| n as i64)
            }
            AnySock::Netlink(s) => s.send(data).map(|n| n as i64),
        }
    })())
}

/// recvfrom(fd, buf, len, flags, addr, addrlen) (and recv)
pub fn sys_recvfrom(fd: u64, buf: u64, len: u64, flags: u64, addr: u64, len_ptr: u64) -> i64 {
    result((|| {
        let out = unsafe { crate::syscall::user_slice_mut(buf, len) }.ok_or(EFAULT)?;
        match any_socket(fd)? {
            AnySock::Inet(s) => {
                let (n, from) = s.recv(out, flags)?;
                if let Some(from) = from {
                    write_sockaddr(addr, len_ptr, from)?;
                } else if s.is_stream() && addr != 0 {
                    write_sockaddr(addr, len_ptr, s.peer().unwrap_or(s.local()))?;
                }
                Ok(n as i64)
            }
            AnySock::Unix(s) => {
                let (n, from) = s.recv(out, flags)?;
                if addr != 0 {
                    write_sockaddr_un(addr, len_ptr, from.as_ref())?;
                }
                Ok(n as i64)
            }
            AnySock::Netlink(s) => {
                let n = s.recv(out, flags)?;
                write_sockaddr_nl(addr, len_ptr, 0)?; // from the kernel
                Ok(n as i64)
            }
        }
    })())
}

pub fn sys_shutdown(fd: u64, how: u64) -> i64 {
    result((|| match any_socket(fd)? {
        AnySock::Inet(s) => s.shutdown(how).map(|_| 0),
        AnySock::Unix(s) => s.shutdown(how).map(|_| 0),
        AnySock::Netlink(_) => Ok(0),
    })())
}

pub fn sys_getsockname(fd: u64, addr: u64, len_ptr: u64) -> i64 {
    result((|| match any_socket(fd)? {
        AnySock::Inet(s) => write_sockaddr(addr, len_ptr, s.local()).map(|_| 0),
        AnySock::Unix(s) => write_sockaddr_un(addr, len_ptr, s.local().as_ref()).map(|_| 0),
        AnySock::Netlink(s) => write_sockaddr_nl(addr, len_ptr, s.port).map(|_| 0),
    })())
}

pub fn sys_getpeername(fd: u64, addr: u64, len_ptr: u64) -> i64 {
    result((|| match any_socket(fd)? {
        AnySock::Inet(s) => s.peer().and_then(|p| write_sockaddr(addr, len_ptr, p)).map(|_| 0),
        AnySock::Unix(s) => s.peer().and_then(|p| write_sockaddr_un(addr, len_ptr, p.as_ref())).map(|_| 0),
        AnySock::Netlink(_) => write_sockaddr_nl(addr, len_ptr, 0).map(|_| 0),
    })())
}

/// setsockopt: IP_HDRINCL and IP_TTL for raw sockets; the rest
/// (SO_REUSEADDR, TCP_NODELAY, timeouts...) is accepted and ignored: the
/// stack's defaults apply.
pub fn sys_setsockopt(fd: u64, level: u64, name: u64, val: u64, len: u64) -> i64 {
    const IPPROTO_IP: u64 = 0;
    result((|| {
        if let AnySock::Inet(s) = any_socket(fd)? {
            if level == IPPROTO_IP && val != 0 && len >= 4 {
                let b = unsafe { crate::syscall::user_slice(val, 4) }.ok_or(EFAULT)?;
                s.set_ip_option(name, u32::from_le_bytes(b.try_into().unwrap()));
            }
        }
        Ok(0)
    })())
}

/// getsockopt: SO_ERROR (a failed non-blocking connect), SO_TYPE and, for
/// Unix sockets, SO_PEERCRED; anything else reads as 0.
pub fn sys_getsockopt(fd: u64, level: u64, name: u64, val: u64, len_ptr: u64) -> i64 {
    const SOL_SOCKET: u64 = 1;
    const SO_TYPE: u64 = 3;
    const SO_ERROR: u64 = 4;
    const SO_PEERCRED: u64 = 17;
    result((|| {
        let s = any_socket(fd)?;
        let bytes: Vec<u8> = match (level, name, &s) {
            (SOL_SOCKET, SO_PEERCRED, AnySock::Unix(u)) => {
                let c = u.peer_creds();
                [c.pid, c.uid, c.gid].iter().flat_map(|v| v.to_le_bytes()).collect()
            }
            (SOL_SOCKET, SO_ERROR, AnySock::Inet(i)) => (i.take_error() as u32).to_le_bytes().to_vec(),
            (SOL_SOCKET, SO_TYPE, _) => {
                let stream = match &s {
                    AnySock::Inet(i) => i.is_stream(),
                    AnySock::Unix(u) => u.is_stream(),
                    AnySock::Netlink(_) => false,
                };
                (if stream { 1u32 } else { 2 }).to_le_bytes().to_vec()
            }
            _ => 0u32.to_le_bytes().to_vec(),
        };
        if val != 0 && len_ptr != 0 {
            write_addr(val, len_ptr, &bytes)?;
        }
        Ok(0)
    })())
}

/// struct msghdr: (name, namelen, iov, iovlen) of sendmsg/recvmsg.
fn read_msghdr(msg: u64) -> Result<(u64, u64, Vec<(u64, u64)>), i64> {
    let m = unsafe { crate::syscall::user_slice(msg, 56) }.ok_or(EFAULT)?;
    let word = |o: usize| u64::from_le_bytes(m[o..o + 8].try_into().unwrap());
    let (name, namelen) = (word(0), word(8) & 0xffff_ffff);
    let (iov, iovlen) = (word(16), word(24));
    if iovlen > 64 {
        return Err(EINVAL);
    }
    let list = unsafe { crate::syscall::user_slice(iov, iovlen * 16) }.ok_or(EFAULT)?;
    let iovs = list
        .chunks(16)
        .map(|c| (u64::from_le_bytes(c[0..8].try_into().unwrap()), u64::from_le_bytes(c[8..16].try_into().unwrap())))
        .collect();
    Ok((name, namelen, iovs))
}

/// sendmsg(fd, msg, flags): the buffers, sent as one message (no control data).
pub fn sys_sendmsg(fd: u64, msg: u64, flags: u64) -> i64 {
    result((|| {
        let (name, namelen, iovs) = read_msghdr(msg)?;
        let mut data = Vec::new();
        for (base, len) in iovs {
            data.extend_from_slice(unsafe { crate::syscall::user_slice(base, len) }.ok_or(EFAULT)?);
        }
        match any_socket(fd)? {
            AnySock::Inet(s) => {
                let to = if name != 0 && !s.is_stream() {
                    let (a, port) = read_sockaddr(name, namelen)?;
                    Some(smoltcp::wire::IpEndpoint::new(smoltcp::wire::IpAddress::Ipv4(a), port))
                } else {
                    None
                };
                s.send(&data, flags, to).map(|n| n as i64)
            }
            AnySock::Unix(s) => {
                let to = if name != 0 && !s.is_stream() { Some(read_sockaddr_un(name, namelen)?) } else { None };
                s.send(&data, flags, to).map(|n| n as i64)
            }
            AnySock::Netlink(s) => s.send(&data).map(|n| n as i64),
        }
    })())
}

/// recvmsg(fd, msg, flags): one message, spread over the buffers; the
/// sender's address in msg_name (no control data).
pub fn sys_recvmsg(fd: u64, msg: u64, flags: u64) -> i64 {
    result((|| {
        let s = any_socket(fd)?;
        let (name, _, iovs) = read_msghdr(msg)?;
        let total: u64 = iovs.iter().map(|(_, l)| *l).sum();
        let mut buf = alloc::vec![0u8; total.min(1 << 20) as usize];
        enum From {
            Inet(Option<smoltcp::wire::IpEndpoint>),
            Unix(Option<unix::Name>),
            Netlink,
        }
        let (n, from) = match &s {
            AnySock::Inet(i) => i.recv(&mut buf, flags).map(|(n, f)| (n, From::Inet(f)))?,
            AnySock::Unix(u) => u.recv(&mut buf, flags).map(|(n, f)| (n, From::Unix(f)))?,
            AnySock::Netlink(l) => (l.recv(&mut buf, flags)?, From::Netlink),
        };
        let mut done = 0usize;
        for (base, len) in iovs {
            if done >= n {
                break;
            }
            let k = (len as usize).min(n - done);
            unsafe { crate::syscall::user_slice_mut(base, k as u64) }.ok_or(EFAULT)?.copy_from_slice(&buf[done..done + k]);
            done += k;
        }
        let m = unsafe { crate::syscall::user_slice_mut(msg, 56) }.ok_or(EFAULT)?;
        m[40..48].copy_from_slice(&0u64.to_le_bytes()); // msg_controllen: no control data
        m[48..52].copy_from_slice(&0u32.to_le_bytes()); // msg_flags
        if name != 0 {
            let namelen_ptr = msg + 8;
            match from {
                From::Inet(Some(ep)) => write_sockaddr(name, namelen_ptr, ep)?,
                From::Unix(n) => write_sockaddr_un(name, namelen_ptr, n.as_ref())?,
                From::Netlink => write_sockaddr_nl(name, namelen_ptr, 0)?,
                From::Inet(None) => {}
            }
        }
        Ok(n as i64)
    })())
}

// ---------------------------------------------------------------- eventfd

const EFD_SEMAPHORE: u64 = 1;

/// eventfd2(initial value, flags: EFD_CLOEXEC, EFD_NONBLOCK, EFD_SEMAPHORE)
pub fn sys_eventfd2(initval: u64, flags: u64) -> i64 {
    if flags & !(O_CLOEXEC | O_NONBLOCK | EFD_SEMAPHORE) != 0 {
        return -EINVAL;
    }
    let h = handle(Kind::EventFd(Arc::new(Mutex::new(EventFd { count: initval & 0xFFFF_FFFF, semaphore: flags & EFD_SEMAPHORE != 0 }))));
    h.lock().nonblocking = flags & O_NONBLOCK != 0;
    match with_process(|p| alloc_fd(p, h, flags & O_CLOEXEC != 0)) {
        Ok(fd) => fd as i64,
        Err(e) => -e,
    }
}

/// The eventfd's key for sleeping on (its address).
fn eventfd_key(e: &Arc<Mutex<EventFd>>) -> u64 {
    0x7E7E_0000_0000_0000 ^ (Arc::as_ptr(e) as u64)
}

fn eventfd_read(e: &Arc<Mutex<EventFd>>, nonblocking: bool, buf: &mut [u8]) -> i64 {
    if buf.len() < 8 {
        return -EINVAL;
    }
    loop {
        {
            let mut ev = e.lock();
            if ev.count > 0 {
                let value = if ev.semaphore { 1 } else { ev.count };
                ev.count -= value;
                drop(ev);
                buf[..8].copy_from_slice(&value.to_le_bytes());
                crate::process::wake(eventfd_key(e)); // writers waiting for room
                return 8;
            }
        }
        if nonblocking {
            return -EAGAIN;
        }
        if crate::process::signal_pending() {
            return -EINTR;
        }
        crate::process::block_on(eventfd_key(e));
    }
}

fn eventfd_write(e: &Arc<Mutex<EventFd>>, nonblocking: bool, data: &[u8]) -> i64 {
    if data.len() < 8 {
        return -EINVAL;
    }
    let value = u64::from_le_bytes(data[..8].try_into().unwrap());
    if value == u64::MAX {
        return -EINVAL;
    }
    loop {
        {
            let mut ev = e.lock();
            if ev.count.checked_add(value).is_some_and(|c| c < u64::MAX) {
                ev.count += value;
                drop(ev);
                crate::process::wake(eventfd_key(e));
                return 8;
            }
        }
        if nonblocking {
            return -EAGAIN;
        }
        if crate::process::signal_pending() {
            return -EINTR;
        }
        crate::process::block_on(eventfd_key(e));
    }
}

// ---------------------------------------------------------------- epoll

const EPOLL_CTL_ADD: u64 = 1;
const EPOLL_CTL_DEL: u64 = 2;
const EPOLL_CTL_MOD: u64 = 3;
/// The events poll() also knows (EPOLLIN, PRI, OUT, ERR, HUP, ...).
const EPOLL_EVENTS: u32 = 0x3F;
const EPOLLONESHOT: u32 = 1 << 30;

/// epoll_create1(flags: EPOLL_CLOEXEC), and epoll_create(size)
pub fn sys_epoll_create(flags: u64) -> i64 {
    if flags & !O_CLOEXEC != 0 {
        return -EINVAL;
    }
    let h = handle(Kind::Epoll(Arc::new(Mutex::new(Vec::new()))));
    match with_process(|p| alloc_fd(p, h, flags & O_CLOEXEC != 0)) {
        Ok(fd) => fd as i64,
        Err(e) => -e,
    }
}

fn epoll_list(epfd: u64) -> Result<Arc<Mutex<Vec<EpollEntry>>>, i64> {
    match &get(epfd)?.lock().kind {
        Kind::Epoll(list) => Ok(list.clone()),
        _ => Err(EINVAL),
    }
}

/// epoll_ctl(epfd, op, fd, event)
pub fn sys_epoll_ctl(epfd: u64, op: u64, fd: u64, event: u64) -> i64 {
    let list = match epoll_list(epfd) {
        Ok(l) => l,
        Err(e) => return -e,
    };
    let target = match get(fd) {
        Ok(h) => h,
        Err(e) => return -e,
    };
    if fd == epfd {
        return -EINVAL;
    }
    if matches!(target.lock().kind, Kind::File { .. } | Kind::Dir { .. }) {
        return -EPERM; // regular files are always ready: Linux refuses them
    }
    let ev = if op == EPOLL_CTL_DEL {
        None
    } else {
        let Some(b) = (unsafe { user_slice(event, 12) }) else { return -EFAULT };
        Some((u32::from_le_bytes(b[0..4].try_into().unwrap()), u64::from_le_bytes(b[4..12].try_into().unwrap())))
    };
    let mut list = list.lock();
    let pos = list.iter().position(|e| e.fd == fd as i32);
    match (op, pos, ev) {
        (EPOLL_CTL_ADD, Some(_), _) => -EEXIST,
        (EPOLL_CTL_ADD, None, Some((events, data))) => {
            list.push(EpollEntry { fd: fd as i32, events, data });
            0
        }
        (EPOLL_CTL_MOD, Some(i), Some((events, data))) => {
            list[i] = EpollEntry { fd: fd as i32, events, data };
            0
        }
        (EPOLL_CTL_DEL, Some(i), _) => {
            list.remove(i);
            0
        }
        (EPOLL_CTL_MOD | EPOLL_CTL_DEL, None, _) => -ENOENT,
        _ => -EINVAL,
    }
}

/// epoll_wait / epoll_pwait / epoll_pwait2: up to `max` ready entries,
/// waiting until `deadline` (in ticks; None: forever, Some(0): not at all).
pub fn sys_epoll_wait(epfd: u64, out: u64, max: u64, deadline: Option<u64>) -> i64 {
    if max == 0 || max > 4096 {
        return -EINVAL;
    }
    let list = match epoll_list(epfd) {
        Ok(l) => l,
        Err(e) => return -e,
    };
    let Some(out) = (unsafe { user_slice_mut(out, max * 12) }) else { return -EFAULT };
    loop {
        crate::net::poll_if_due();
        let mut ready = 0usize;
        {
            let mut entries = list.lock();
            // Fds closed since they were added are gone from the list.
            entries.retain(|e| get(e.fd as u64).is_ok());
            for e in entries.iter_mut() {
                if ready as u64 == max {
                    break;
                }
                let wanted = (e.events & EPOLL_EVENTS) as i16 | POLLERR | POLLHUP;
                if e.events & EPOLL_EVENTS == 0 && e.events & EPOLLONESHOT != 0 {
                    continue; // a one-shot entry that already fired
                }
                let revents = poll_ready(e.fd, wanted);
                if revents == 0 {
                    continue;
                }
                let slot = &mut out[ready * 12..ready * 12 + 12];
                slot[0..4].copy_from_slice(&(revents as u16 as u32).to_le_bytes());
                slot[4..12].copy_from_slice(&e.data.to_le_bytes());
                ready += 1;
                if e.events & EPOLLONESHOT != 0 {
                    e.events &= !EPOLL_EVENTS;
                }
            }
        }
        if ready > 0 || deadline == Some(0) {
            return ready as i64;
        }
        if deadline.is_some_and(|d| crate::process::ticks() >= d) {
            return 0;
        }
        if crate::process::signal_pending() {
            return -EINTR;
        }
        match timer_capped(deadline) {
            Some(d) => crate::process::block_on_until(crate::process::POLL_KEY, d),
            None => crate::process::block_on(crate::process::POLL_KEY),
        }
    }
}

// ---------------------------------------------------------------- timerfd

struct TimerFd {
    realtime: bool,
    /// When it next expires (ticks), if armed.
    next: Option<u64>,
    /// Repeat every this many ticks (0: once).
    interval: u64,
    /// Expirations not read yet.
    expired: u64,
}

impl TimerFd {
    /// Count the expirations up to `now`.
    fn update(&mut self, now: u64) {
        if let Some(next) = self.next.filter(|&n| now >= n) {
            if self.interval > 0 {
                let k = (now - next) / self.interval + 1;
                self.expired += k;
                self.next = Some(next + k * self.interval);
            } else {
                self.expired += 1;
                self.next = None;
            }
        }
    }
}

/// Every timerfd, so poll and epoll can sleep until the next expiry.
static TIMERS: Mutex<Vec<alloc::sync::Weak<Mutex<TimerFd>>>> = Mutex::new(Vec::new());

/// `deadline`, or sooner if a timerfd expires sooner (poll and epoll wake
/// up then and check their fds again).
pub fn timer_capped(deadline: Option<u64>) -> Option<u64> {
    let now = crate::process::ticks();
    let mut timers = TIMERS.lock();
    timers.retain(|w| w.strong_count() > 0);
    // Each timer brought up to date first: an expiry that has passed (on a
    // timer nobody is polling) would otherwise be a deadline in the past,
    // and the caller would never sleep. After update, `next` is later than
    // now (or None).
    let next = timers
        .iter()
        .filter_map(|w| w.upgrade())
        .filter_map(|t| {
            let mut t = t.lock();
            t.update(now);
            t.next
        })
        .min();
    match (deadline, next) {
        (Some(d), Some(n)) => Some(d.min(n)),
        (d, n) => d.or(n),
    }
}

const TFD_TIMER_ABSTIME: u64 = 1;
const CLOCK_REALTIME: u64 = 0;
const CLOCK_MONOTONIC: u64 = 1;
const CLOCK_BOOTTIME: u64 = 7;

/// timerfd_create(clock, flags: TFD_CLOEXEC, TFD_NONBLOCK)
pub fn sys_timerfd_create(clock: u64, flags: u64) -> i64 {
    if !matches!(clock, CLOCK_REALTIME | CLOCK_MONOTONIC | CLOCK_BOOTTIME) {
        return -EINVAL;
    }
    if flags & !(O_CLOEXEC | O_NONBLOCK) != 0 {
        return -EINVAL;
    }
    let t = Arc::new(Mutex::new(TimerFd { realtime: clock == CLOCK_REALTIME, next: None, interval: 0, expired: 0 }));
    TIMERS.lock().push(Arc::downgrade(&t));
    let h = handle(Kind::TimerFd(t));
    h.lock().nonblocking = flags & O_NONBLOCK != 0;
    match with_process(|p| alloc_fd(p, h, flags & O_CLOEXEC != 0)) {
        Ok(fd) => fd as i64,
        Err(e) => -e,
    }
}

fn timerfd_of(fd: u64) -> Result<Arc<Mutex<TimerFd>>, i64> {
    match &get(fd)?.lock().kind {
        Kind::TimerFd(t) => Ok(t.clone()),
        _ => Err(EINVAL),
    }
}

fn ns_to_ticks(ns: u64) -> u64 {
    ns.div_ceil(crate::process::TICK_NS)
}

/// The itimerspec at `ptr`: (interval ns, value ns).
fn read_itimerspec(ptr: u64) -> Result<(u64, u64), i64> {
    let b = unsafe { user_slice(ptr, 32) }.ok_or(EFAULT)?;
    let f = |o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap());
    if f(8) >= 1_000_000_000 || f(24) >= 1_000_000_000 {
        return Err(EINVAL);
    }
    Ok((f(0).saturating_mul(1_000_000_000).saturating_add(f(8)), f(16).saturating_mul(1_000_000_000).saturating_add(f(24))))
}

/// The current setting as an itimerspec: (interval, time left).
fn write_itimerspec(ptr: u64, t: &TimerFd, now: u64) -> Result<(), i64> {
    let b = unsafe { user_slice_mut(ptr, 32) }.ok_or(EFAULT)?;
    let tick = crate::process::TICK_NS;
    let interval = t.interval * tick;
    let left = t.next.map_or(0, |n| n.saturating_sub(now).max(1) * tick);
    for (o, ns) in [(0, interval), (16, left)] {
        b[o..o + 8].copy_from_slice(&(ns / 1_000_000_000).to_le_bytes());
        b[o + 8..o + 16].copy_from_slice(&(ns % 1_000_000_000).to_le_bytes());
    }
    Ok(())
}

/// timerfd_settime(fd, flags: TFD_TIMER_ABSTIME, new, old)
pub fn sys_timerfd_settime(fd: u64, flags: u64, new: u64, old: u64) -> i64 {
    if flags & !(TFD_TIMER_ABSTIME | 2 /* TFD_TIMER_CANCEL_ON_SET */) != 0 {
        return -EINVAL;
    }
    let t = match timerfd_of(fd) {
        Ok(t) => t,
        Err(e) => return -e,
    };
    let (interval, value) = match read_itimerspec(new) {
        Ok(v) => v,
        Err(e) => return -e,
    };
    let now = crate::process::ticks();
    let mut t = t.lock();
    t.update(now);
    if old != 0 {
        if let Err(e) = write_itimerspec(old, &t, now) {
            return -e;
        }
    }
    t.expired = 0;
    t.interval = ns_to_ticks(interval);
    t.next = if value == 0 {
        None // disarm
    } else if flags & TFD_TIMER_ABSTIME != 0 {
        // An absolute time on the timer's clock.
        let now_ns = if t.realtime { crate::rtc::now_ns() } else { now * crate::process::TICK_NS };
        Some(now + ns_to_ticks(value.saturating_sub(now_ns)))
    } else {
        Some(now + ns_to_ticks(value).max(1))
    };
    0
}

/// timerfd_gettime(fd, cur)
pub fn sys_timerfd_gettime(fd: u64, cur: u64) -> i64 {
    let t = match timerfd_of(fd) {
        Ok(t) => t,
        Err(e) => return -e,
    };
    let now = crate::process::ticks();
    let mut t = t.lock();
    t.update(now);
    match write_itimerspec(cur, &t, now) {
        Ok(()) => 0,
        Err(e) => -e,
    }
}

/// read: the number of expirations since the last read (waits for one).
fn timerfd_read(t: &Arc<Mutex<TimerFd>>, nonblocking: bool, buf: &mut [u8]) -> i64 {
    if buf.len() < 8 {
        return -EINVAL;
    }
    loop {
        let next = {
            let mut tf = t.lock();
            tf.update(crate::process::ticks());
            if tf.expired > 0 {
                buf[..8].copy_from_slice(&tf.expired.to_le_bytes());
                tf.expired = 0;
                return 8;
            }
            tf.next
        };
        if nonblocking {
            return -EAGAIN;
        }
        if crate::process::signal_pending() {
            return -EINTR;
        }
        match next {
            Some(d) => crate::process::block_on_until(crate::process::POLL_KEY, d),
            None => crate::process::block_on(crate::process::POLL_KEY), // disarmed: until settime
        }
    }
}

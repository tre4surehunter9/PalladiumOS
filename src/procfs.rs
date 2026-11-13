// Copyright (c) 2026 tre4surehunter9
//! /proc: information about processes and the system, as files.
//!
//! Nothing is stored: each file's text is generated when it's read, so
//! it's always current. Per process (like Linux):
//!   /proc/PID/status   Name, State, Pid, PPid, memory, ...
//!   /proc/PID/stat     the one-line format ps and top read
//!   /proc/PID/cmdline  the arguments, separated by NUL bytes
//!   /proc/PID/cwd      symlink to the working directory
//!   /proc/PID/exe      symlink to the program
//!   /proc/PID/fd/N     symlink to what fd N is (a path, /dev/tty, pipe:[id])
//!   /proc/self         symlink to the reading process's directory
//! System: meminfo, uptime, loadavg, mounts, version, cpuinfo, and
//! /proc/net/if_inet6 (the IPv6 addresses, for ifconfig and ip), /proc/net/dev
//! (each interface's traffic, for ifconfig).

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::mounts::{FileSystem, FileType, FsResult, Ino, Meta};
use crate::process::{self, ProcInfo};
use crate::syscall::{EACCES, EINVAL, ENOENT, ENOTDIR, EPERM};

const ROOT: Ino = 1;
const SELF: Ino = 2;
/// /proc/net and its files: (name, inode).
const NET_DIR: Ino = 13;
const NET_FILES: &[(&str, Ino)] = &[("if_inet6", 14), ("dev", 15)];
/// System files: (name, inode).
const SYSTEM: &[(&str, Ino)] =
    &[("cmdline", 11), ("cpuinfo", 3), ("kmsg", 9), ("loadavg", 4), ("meminfo", 5), ("mounts", 6), ("sound", 12), ("stat", 10), ("uptime", 7), ("version", 8)];

// Files in /proc/PID: (name, number).
const P_DIR: u64 = 0;
const P_FILES: &[(&str, u64)] =
    &[("cmdline", 1), ("cwd", 2), ("exe", 3), ("fd", 4), ("stat", 5), ("statm", 7), ("status", 6), ("task", 8)];
const P_FD_DIR: u64 = 4;
/// /proc/PID/task: the process's threads, each entry the same as /proc/TID
/// (htop reads processes through it).
const P_TASK_DIR: u64 = 8;
const P_FD_BASE: u64 = 0x100;

fn pid_ino(pid: u64, file: u64) -> Ino {
    ((pid + 1) << 16) | file
}

/// (pid, file number) of a per-process inode.
fn split(ino: Ino) -> Option<(u64, u64)> {
    let pid1 = ino >> 16;
    if pid1 == 0 {
        return None;
    }
    Some((pid1 - 1, ino & 0xFFFF))
}

fn proc_info(pid: u64) -> FsResult<ProcInfo> {
    process::snapshot(Some(pid)).into_iter().find(|p| !p.kernel).ok_or(ENOENT)
}

pub struct ProcFs;

impl ProcFs {
    /// The text of a regular file.
    fn content(&self, ino: Ino) -> FsResult<String> {
        if let Some((pid, file)) = split(ino) {
            let p = proc_info(pid)?;
            return Ok(match file {
                1 => {
                    let mut s = p.argv.join("\0");
                    s.push('\0');
                    s
                }
                5 => stat_line(&p),
                6 => status(&p),
                // statm: size, resident, shared, text, lib, data, dt (pages)
                7 => format!("{} {} 0 0 0 {} 0\n", p.pages, p.pages, p.heap / 4096),
                _ => return Err(EINVAL),
            });
        }
        Ok(match ino {
            3 => cpuinfo(),
            9 => crate::boot::log(), // the boot log (dmesg)
            4 => {
                let (total, ready) = process::counts();
                let [a, b, c] = process::load_averages();
                format!(
                    "{}.{:02} {}.{:02} {}.{:02} {}/{} {}\n",
                    a / 100, a % 100, b / 100, b % 100, c / 100, c % 100, ready, total, process::last_pid()
                )
            }
            5 => meminfo(),
            6 => {
                let mut s = String::new();
                for (path, fs) in crate::mounts::list_unlocked() {
                    // A disk's device (/dev/hdb, /dev/sda2...), else the
                    // filesystem's name, as Linux shows tmpfs or proc.
                    let device = crate::mounts::device(&path).unwrap_or_else(|| fs.to_string());
                    let mode = if path == "/usr" { "ro" } else { "rw" };
                    s.push_str(&format!("{} {} {} {} 0 0\n", device, path, fs, mode));
                }
                s
            }
            7 => {
                let ns = process::ticks() * process::TICK_NS;
                format!("{}.{:02} 0.00\n", ns / 1_000_000_000, ns / 10_000_000 % 100)
            }
            8 => format!("PalladiumOS version {} (rustc nightly) #1 x86_64\n", crate::VERSION),
            10 => system_stat(),
            11 => String::from("BOOT_IMAGE=palladiumos\n"), // the kernel's command line
            12 => crate::sound::status(), // the sound card (as old Linux's /proc/sound)
            14 => if_inet6(),
            15 => net_dev(),
            _ => return Err(EINVAL),
        })
    }
}

/// /proc/stat: CPU time (in 1/100 s: busy is what processes used, the rest
/// idle), boot time and process counts, as top and htop read them.
fn system_stat() -> String {
    let hundredths = |ticks: u64| ticks * process::TICK_NS / 10_000_000;
    let busy: u64 = process::snapshot(None).iter().filter(|p| !p.kernel).map(|p| p.cpu_ticks).sum();
    let up = process::ticks();
    // Every core's time: busy time shared out evenly (it isn't counted per
    // core), the rest idle.
    let n = crate::smp::cores() as u64;
    let all = up * n;
    let (user, idle) = (hundredths(busy.min(all)), hundredths(all.saturating_sub(busy)));
    let cpu = format!("{} 0 0 {} 0 0 0 0 0 0", user, idle);
    let percpu: String = (0..n)
        .map(|i| format!("cpu{} {} 0 0 {} 0 0 0 0 0 0\n", i, user / n, idle / n))
        .collect();
    let uptime_s = up * process::TICK_NS / 1_000_000_000;
    let (total, ready) = process::counts();
    format!(
        "cpu  {cpu}\n{percpu}intr 0\nctxt 0\nbtime {}\nprocesses {}\nprocs_running {}\nprocs_blocked {}\n",
        crate::rtc::now().saturating_sub(uptime_s),
        process::last_pid(),
        ready,
        total.saturating_sub(ready)
    )
}

fn kib(pages: u64) -> u64 {
    pages * 4
}

fn status(p: &ProcInfo) -> String {
    let state = match p.state {
        'R' => "R (running)",
        'Z' => "Z (zombie)",
        'T' => "T (stopped)",
        _ => "S (sleeping)",
    };
    format!(
        "Name:\t{}\nState:\t{}\nTgid:\t{}\nPid:\t{}\nPPid:\t{}\nPGid:\t{}\nUid:\t{}\t{}\t{}\t{}\nGid:\t{}\t{}\t{}\t{}\n\
         FDSize:\t{}\nVmSize:\t{} kB\nVmRSS:\t{} kB\nVmData:\t{} kB\nThreads:\t1\n\
         SigPnd:\t{:016x}\nSigBlk:\t{:016x}\nSigIgn:\t{:016x}\nSigCgt:\t{:016x}\n",
        p.name,
        state,
        p.tgid,
        p.pid,
        p.ppid,
        p.pgid,
        p.cred.ruid,
        p.cred.euid,
        p.cred.suid,
        p.cred.euid,
        p.cred.rgid,
        p.cred.egid,
        p.cred.sgid,
        p.cred.egid,
        p.fds.len(),
        kib(p.pages),
        kib(p.pages),
        p.heap / 1024,
        p.signals[0],
        p.signals[1],
        p.signals[2],
        p.signals[3],
    )
}

/// Linux's /proc/PID/stat line (the fields ps and top use are filled in;
/// times are in clock ticks of 100 Hz, as Linux reports them).
fn stat_line(p: &ProcInfo) -> String {
    let to_hz = |ticks: u64| ticks * process::TICK_NS / 10_000_000;
    format!(
        // All 52 fields, as on Linux (exit_signal: SIGCHLD).
        "{} ({}) {} {} {} {} 0 -1 0 0 0 0 0 {} 0 0 0 20 0 1 0 {} {} {} 0 0 0 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n",
        p.pid,
        p.name,
        p.state,
        p.ppid,
        p.pgid,
        p.pgid, // session
        to_hz(p.cpu_ticks), // utime
        to_hz(p.start_tick), // starttime
        p.pages * 4096, // vsize
        p.pages, // rss (pages)
    )
}

fn meminfo() -> String {
    let (total, used) = {
        let mut alloc = crate::memory::FRAME_ALLOCATOR.lock();
        match alloc.as_mut() {
            Some(a) => (a.total_frames() as u64, a.frames_in_use() as u64),
            None => (0, 0),
        }
    };
    let free = total.saturating_sub(used);
    let heap = crate::allocator::HEAP_SIZE as u64 / 1024;
    format!(
        "MemTotal:       {:8} kB\nMemFree:        {:8} kB\nMemAvailable:   {:8} kB\n\
         Buffers:        {:8} kB\nCached:         {:8} kB\nSwapTotal:             0 kB\nSwapFree:              0 kB\n\
         Shmem:                 0 kB\nSReclaimable:          0 kB\nKernelHeap:     {:8} kB\n",
        kib(total),
        kib(free),
        kib(free),
        0,
        0,
        heap
    )
}

fn cpuinfo() -> String {
    use core::arch::x86_64::__cpuid;
    let vendor = {
        let r = __cpuid(0);
        let mut v = Vec::new();
        for reg in [r.ebx, r.edx, r.ecx] {
            v.extend_from_slice(&reg.to_le_bytes());
        }
        String::from_utf8_lossy(&v).into_owned()
    };
    let model = {
        if __cpuid(0x8000_0000).eax >= 0x8000_0004 {
            let mut v = Vec::new();
            for leaf in 0x8000_0002u32..=0x8000_0004 {
                let r = __cpuid(leaf);
                for reg in [r.eax, r.ebx, r.ecx, r.edx] {
                    v.extend_from_slice(&reg.to_le_bytes());
                }
            }
            String::from_utf8_lossy(&v).trim_matches(|c: char| c == '\0' || c == ' ').to_string()
        } else {
            "unknown".to_string()
        }
    };
    // One entry per core, as on Linux (nproc and htop count them).
    let n = crate::smp::cores();
    (0..n)
        .map(|i| {
            format!(
                "processor\t: {}\nvendor_id\t: {}\nmodel name\t: {}\nphysical id\t: 0\nsiblings\t: {}\ncore id\t\t: {}\ncpu cores\t: {}\n\n",
                i, vendor, model, n, i, n
            )
        })
        .collect()
}

impl FileSystem for ProcFs {
    fn name(&self) -> &'static str {
        "proc"
    }

    fn root(&self) -> Ino {
        ROOT
    }

    fn lookup(&mut self, dir: Ino, name: &str) -> FsResult<Ino> {
        if dir == NET_DIR {
            return NET_FILES.iter().find(|(n, _)| *n == name).map(|&(_, i)| i).ok_or(ENOENT);
        }
        if dir == ROOT {
            if name == "self" {
                return Ok(SELF);
            }
            if name == "net" {
                return Ok(NET_DIR);
            }
            if let Some(&(_, ino)) = SYSTEM.iter().find(|(n, _)| *n == name) {
                return Ok(ino);
            }
            let pid: u64 = name.parse().map_err(|_| ENOENT)?;
            proc_info(pid)?;
            return Ok(pid_ino(pid, P_DIR));
        }
        match split(dir) {
            Some((pid, P_DIR)) => {
                proc_info(pid)?;
                P_FILES.iter().find(|(n, _)| *n == name).map(|&(_, f)| pid_ino(pid, f)).ok_or(ENOENT)
            }
            Some((pid, P_TASK_DIR)) => {
                let tid: u64 = name.parse().map_err(|_| ENOENT)?;
                let group = proc_info(pid)?.tgid;
                if proc_info(tid)?.tgid != group {
                    return Err(ENOENT);
                }
                Ok(pid_ino(tid, P_DIR))
            }
            Some((pid, P_FD_DIR)) => {
                let fd: usize = name.parse().map_err(|_| ENOENT)?;
                let p = proc_info(pid)?;
                p.fds.iter().find(|(n, _)| *n == fd).map(|_| pid_ino(pid, P_FD_BASE + fd as u64)).ok_or(ENOENT)
            }
            _ => Err(ENOTDIR),
        }
    }

    fn meta(&mut self, ino: Ino) -> FsResult<Meta> {
        let (kind, perm) = if ino == ROOT || ino == NET_DIR {
            (FileType::Dir, 0o555)
        } else if NET_FILES.iter().any(|&(_, i)| i == ino) {
            (FileType::File, 0o444)
        } else if ino == SELF {
            (FileType::Symlink, 0o777)
        } else if SYSTEM.iter().any(|&(_, i)| i == ino) {
            (FileType::File, 0o444)
        } else {
            let (pid, file) = split(ino).ok_or(ENOENT)?;
            let p = proc_info(pid)?;
            match file {
                P_DIR | P_FD_DIR | P_TASK_DIR => (FileType::Dir, 0o555),
                2 | 3 => (FileType::Symlink, 0o777),
                f if f >= P_FD_BASE => {
                    if !p.fds.iter().any(|(n, _)| *n as u64 == f - P_FD_BASE) {
                        return Err(ENOENT);
                    }
                    (FileType::Symlink, 0o700)
                }
                _ => (FileType::File, 0o444),
            }
        };
        let mtime = crate::rtc::now();
        // /proc/PID belongs to the process's user.
        let (uid, gid) = match split(ino) {
            Some((pid, _)) => proc_info(pid).map(|p| (p.uid, p.gid)).unwrap_or((0, 0)),
            None => (0, 0),
        };
        // Like Linux, files report size 0: read them to see the contents.
        Ok(Meta { ino, kind, size: 0, perm, nlink: 1, mtime, uid, gid })
    }

    fn read(&mut self, ino: Ino, offset: u64, buf: &mut [u8]) -> FsResult<usize> {
        let text = self.content(ino)?;
        let bytes = text.as_bytes();
        let start = (offset as usize).min(bytes.len());
        let n = buf.len().min(bytes.len() - start);
        buf[..n].copy_from_slice(&bytes[start..start + n]);
        Ok(n)
    }

    fn write(&mut self, _ino: Ino, _offset: u64, _data: &[u8]) -> FsResult<usize> {
        Err(EACCES)
    }

    fn truncate(&mut self, _ino: Ino, _len: u64) -> FsResult<()> {
        Err(EACCES)
    }

    fn readdir(&mut self, dir: Ino) -> FsResult<Vec<(String, Ino, FileType)>> {
        if dir == NET_DIR {
            return Ok(NET_FILES.iter().map(|&(n, i)| (n.to_string(), i, FileType::File)).collect());
        }
        if dir == ROOT {
            let mut out: Vec<(String, Ino, FileType)> =
                SYSTEM.iter().map(|&(n, i)| (n.to_string(), i, FileType::File)).collect();
            out.push(("self".to_string(), SELF, FileType::Symlink));
            out.push(("net".to_string(), NET_DIR, FileType::Dir));
            for p in process::snapshot(None).into_iter().filter(|p| !p.kernel) {
                out.push((p.pid.to_string(), pid_ino(p.pid, P_DIR), FileType::Dir));
            }
            return Ok(out);
        }
        match split(dir) {
            Some((pid, P_DIR)) => {
                proc_info(pid)?;
                Ok(P_FILES
                    .iter()
                    .map(|&(n, f)| {
                        let kind = match f {
                            P_FD_DIR | P_TASK_DIR => FileType::Dir,
                            2 | 3 => FileType::Symlink,
                            _ => FileType::File,
                        };
                        (n.to_string(), pid_ino(pid, f), kind)
                    })
                    .collect())
            }
            Some((pid, P_TASK_DIR)) => {
                let group = proc_info(pid)?.tgid;
                Ok(process::snapshot(None)
                    .into_iter()
                    .filter(|t| !t.kernel && t.tgid == group)
                    .map(|t| (t.pid.to_string(), pid_ino(t.pid, P_DIR), FileType::Dir))
                    .collect())
            }
            Some((pid, P_FD_DIR)) => {
                let p = proc_info(pid)?;
                Ok(p.fds.iter().map(|(fd, _)| (fd.to_string(), pid_ino(pid, P_FD_BASE + *fd as u64), FileType::Symlink)).collect())
            }
            _ => Err(ENOTDIR),
        }
    }

    fn readlink(&mut self, ino: Ino) -> FsResult<String> {
        if ino == SELF {
            return Ok(process::current_pid().to_string());
        }
        let (pid, file) = split(ino).ok_or(EINVAL)?;
        let p = proc_info(pid)?;
        let target = match file {
            2 => p.cwd,
            3 => p.exe,
            f if f >= P_FD_BASE => {
                p.fds.into_iter().find(|(n, _)| *n as u64 == f - P_FD_BASE).map(|(_, what)| what).ok_or(ENOENT)?
            }
            _ => return Err(EINVAL),
        };
        // Paths as the reader sees them: inside a chroot (Alpine), without
        // the root's own path (/usr/bin/node, not /alpine/usr/bin/node).
        let root = process::current_root();
        Ok(if root == "/" { target } else { crate::filesystem::strip_root(&root, &target).unwrap_or(target) })
    }

    fn create(&mut self, _dir: Ino, _name: &str, _kind: FileType, _perm: u32) -> FsResult<Ino> {
        Err(EPERM)
    }

    fn remove(&mut self, _dir: Ino, _name: &str, _is_dir: bool) -> FsResult<()> {
        Err(EPERM)
    }
}

/// /proc/net/dev, as Linux writes it: per interface, what it received
/// (bytes, packets, errors, drops, FIFO, frame errors, compressed,
/// multicast) and sent (bytes, packets, errors, drops, FIFO, collisions,
/// carrier, compressed).
fn net_dev() -> String {
    use core::sync::atomic::Ordering::Relaxed;
    let mut out = String::from(
        "Inter-|   Receive                                                |  Transmit\n face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n",
    );
    for (name, index) in crate::net::netlink::link_names() {
        let s = &crate::net::STATS[if index == 1 { 0 } else { 1 }];
        out.push_str(&format!(
            "{:>6}: {:>7} {:>7}    0    0    0     0          0         0 {:>8} {:>7}    0 {:>4}    0     0       0          0\n",
            name,
            s.rx_bytes.load(Relaxed),
            s.rx_packets.load(Relaxed),
            s.tx_bytes.load(Relaxed),
            s.tx_packets.load(Relaxed),
            s.tx_dropped.load(Relaxed),
        ));
    }
    out
}

/// /proc/net/if_inet6, as Linux writes it: per IPv6 address, the address
/// (32 hex digits), interface number, prefix length, scope (0 global,
/// 0x20 link, 0x10 host), flags (0x80 permanent) and interface name.
fn if_inet6() -> String {
    let mut out = String::new();
    for (index, addr, prefix) in crate::net::ipv6_addresses() {
        let hex: String = addr.octets().iter().map(|b| format!("{:02x}", b)).collect();
        let scope = if addr.is_loopback() {
            0x10
        } else if addr.segments()[0] & 0xffc0 == 0xfe80 {
            0x20
        } else {
            0
        };
        let name = if index == 1 { "lo" } else { "eth0" };
        out.push_str(&format!("{} {:02x} {:02x} {:02x} 80 {:>8}\n", hex, index, prefix, scope, name));
    }
    out
}

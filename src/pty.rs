// Copyright (c) 2026 tre4surehunter9
//! Pseudo-terminals (Unix 98 style): a terminal a program provides.
//!
//! Opening /dev/ptmx makes a new pair. The *master* end is the program
//! playing the terminal (an SSH server, `script`, a terminal emulator): what
//! it writes is "typed", and it reads what's "displayed". The *slave* end,
//! /dev/pts/N, is an ordinary terminal for the programs running on it (a
//! shell). Between them is a line discipline, like the console's:
//!
//! - Input (master writes): ICRNL/INLCR/IGNCR; with ISIG, the VINTR/VQUIT/
//!   VSUSP characters (Ctrl-C, Ctrl-\, Ctrl-Z) become SIGINT/SIGQUIT/SIGTSTP
//!   for the terminal's foreground process group; with ICANON, lines are
//!   edited (VERASE, VKILL, VWERASE) and handed over whole, VEOF ends input;
//!   otherwise bytes are available at once (VMIN/VTIME). ECHO sends them
//!   back to the master (control characters as ^X with ECHOCTL).
//! - Output (slave writes): with OPOST and ONLCR, "\n" becomes "\r\n".
//!
//! Also: sessions and controlling terminals (TIOCSCTTY, opening the slave
//! as a session leader, /dev/tty), the foreground process group
//! (TIOCSPGRP; a background read gets SIGTTIN), the window size (SIGWINCH),
//! and hangups: when the master closes, the session gets SIGHUP and the
//! slave reads end-of-file; when every slave fd closes, the master reads
//! EIO.

use alloc::collections::{BTreeMap, VecDeque};
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use spin::Mutex;

use crate::process::{self, CTTY_NONE};
use crate::syscall::{user_slice, user_slice_mut, EAGAIN, EFAULT, EINTR, EINVAL, EIO, ENOTTY, EPERM};

/// At most this many pseudo-terminals at once.
pub const MAX_PTYS: u32 = 64;
/// Input waiting for the slave (and the line being edited).
const INPUT_MAX: usize = 4096;
/// Output waiting for the master.
const OUTPUT_MAX: usize = 16 * 1024;

// termios bits (Linux values)
const IGNCR: u32 = 0o200;
const ICRNL: u32 = 0o400;
const INLCR: u32 = 0o100;
const IXON: u32 = 0o2000;
const OPOST: u32 = 0o1;
const ONLCR: u32 = 0o4;
const ISIG: u32 = 0o1;
const ICANON: u32 = 0o2;
const ECHO: u32 = 0o10;
const ECHOE: u32 = 0o20;
const ECHOK: u32 = 0o40;
const ECHONL: u32 = 0o100;
const NOFLSH: u32 = 0o200;
const ECHOCTL: u32 = 0o1000;
const ECHOKE: u32 = 0o4000;
const IEXTEN: u32 = 0o100000;
// c_cc indexes
const VINTR: usize = 0;
const VQUIT: usize = 1;
const VERASE: usize = 2;
const VKILL: usize = 3;
const VEOF: usize = 4;
const VTIME: usize = 5;
const VMIN: usize = 6;
const VSUSP: usize = 10;
const VEOL: usize = 11;
const VWERASE: usize = 14;
const VEOL2: usize = 16;

/// struct termios as the kernel's ioctls see it: c_iflag, c_oflag, c_cflag,
/// c_lflag, c_line, c_cc[19].
#[derive(Clone, Copy)]
pub struct Termios {
    pub iflag: u32,
    pub oflag: u32,
    pub cflag: u32,
    pub lflag: u32,
    pub line: u8,
    pub cc: [u8; 19],
}

pub const TERMIOS_SIZE: usize = 36;

impl Termios {
    /// What Linux gives a new pseudo-terminal.
    pub const DEFAULT: Termios = Termios {
        iflag: ICRNL | IXON,
        oflag: OPOST | ONLCR,
        cflag: 0o277, // B38400 | CS8 | CREAD
        lflag: ISIG | ICANON | ECHO | ECHOE | ECHOK | ECHOCTL | ECHOKE | IEXTEN,
        line: 0,
        // ^C ^\ DEL ^U ^D, VTIME 0, VMIN 1, -, ^Q ^S ^Z, -, ^R ^O ^W ^V
        cc: [3, 0x1c, 0x7f, 0x15, 4, 0, 1, 0, 0x11, 0x13, 0x1a, 0, 0x12, 0x0f, 0x17, 0x16, 0, 0, 0],
    };

    pub fn to_bytes(&self) -> [u8; TERMIOS_SIZE] {
        let mut b = [0u8; TERMIOS_SIZE];
        for (i, v) in [self.iflag, self.oflag, self.cflag, self.lflag].iter().enumerate() {
            b[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        b[16] = self.line;
        b[17..36].copy_from_slice(&self.cc);
        b
    }

    pub fn from_bytes(b: &[u8]) -> Termios {
        let word = |i: usize| u32::from_le_bytes(b[i * 4..i * 4 + 4].try_into().unwrap());
        let mut cc = [0u8; 19];
        cc.copy_from_slice(&b[17..36]);
        Termios { iflag: word(0), oflag: word(1), cflag: word(2), lflag: word(3), line: b[16], cc }
    }
}

struct State {
    termios: Termios,
    /// The line being edited (canonical mode).
    line: Vec<u8>,
    /// Finished lines for the slave (canonical mode); an empty one is
    /// end-of-file (VEOF on an empty line).
    lines: VecDeque<Vec<u8>>,
    /// Bytes for the slave (non-canonical mode).
    raw: VecDeque<u8>,
    /// Bytes for the master to read.
    output: VecDeque<u8>,
    /// Locked until unlockpt() (TIOCSPTLCK 0): the slave can't be opened.
    locked: bool,
    /// The session it's the controlling terminal of (0: none), and that
    /// session's foreground process group.
    sid: u64,
    fg: u64,
    /// struct winsize: rows, columns, xpixel, ypixel.
    winsize: [u16; 4],
    /// The master is closed: the slave reads end-of-file, writes fail.
    hung_up: bool,
    /// The slave was opened at least once (until then, the master's reads
    /// wait instead of failing).
    slave_opened: bool,
    /// /dev/pts/N's owner, group and mode (grantpt: the opener, 0620).
    uid: u32,
    gid: u32,
    mode: u32,
}

pub struct Pty {
    pub index: u32,
    state: Mutex<State>,
    masters: AtomicUsize,
    slaves: AtomicUsize,
}

static PTYS: Mutex<BTreeMap<u32, Weak<Pty>>> = Mutex::new(BTreeMap::new());

impl Pty {
    /// What processes waiting on this terminal block on.
    fn key(&self) -> u64 {
        self as *const Pty as u64
    }

    fn wake(&self) {
        process::wake(self.key());
    }

    /// The controlling-terminal number processes record for it.
    pub fn ctty(&self) -> u32 {
        self.index + 1
    }
}

/// The pseudo-terminal with index `n`, if it exists (its master is open).
pub fn get(n: u32) -> Option<Arc<Pty>> {
    PTYS.lock().get(&n).and_then(Weak::upgrade).filter(|p| !p.state.lock().hung_up)
}

/// The indexes of the pseudo-terminals in use, for /dev/pts.
pub fn list() -> Vec<u32> {
    let ptys = PTYS.lock();
    ptys.iter().filter(|(_, w)| w.upgrade().is_some_and(|p| !p.state.lock().hung_up)).map(|(n, _)| *n).collect()
}

/// /dev/pts/N's (uid, gid, mode).
pub fn owner(n: u32) -> Option<(u32, u32, u32)> {
    get(n).map(|p| {
        let s = p.state.lock();
        (s.uid, s.gid, s.mode)
    })
}

pub fn set_owner(n: u32, uid: Option<u32>, gid: Option<u32>, mode: Option<u32>) -> bool {
    let Some(p) = get(n) else { return false };
    let mut s = p.state.lock();
    if let Some(u) = uid {
        s.uid = u;
    }
    if let Some(g) = gid {
        s.gid = g;
    }
    if let Some(m) = mode {
        s.mode = m & 0o7777;
    }
    true
}

/// One open end. Dropping it (closing the last fd) updates the counts:
/// the master going away hangs the terminal up.
pub struct End {
    pub pty: Arc<Pty>,
    pub master: bool,
    nonblocking: AtomicBool,
}

impl End {
    pub fn is_nonblocking(&self) -> bool {
        self.nonblocking.load(Ordering::Relaxed)
    }

    pub fn set_nonblocking(&self, on: bool) {
        self.nonblocking.store(on, Ordering::Relaxed);
    }
}

impl Drop for End {
    fn drop(&mut self) {
        let pty = &self.pty;
        if self.master {
            if pty.masters.fetch_sub(1, Ordering::SeqCst) == 1 {
                hang_up(pty);
            }
        } else if pty.slaves.fetch_sub(1, Ordering::SeqCst) == 1 {
            pty.wake(); // the master reads EIO now
        }
    }
}

/// The master closed: the session loses its terminal and gets SIGHUP (the
/// foreground group, and the session leader), then SIGCONT, as on Linux.
fn hang_up(pty: &Arc<Pty>) {
    let (sid, fg) = {
        let mut s = pty.state.lock();
        s.hung_up = true;
        let r = (s.sid, s.fg);
        s.sid = 0;
        s.fg = 0;
        r
    };
    pty.wake();
    if sid != 0 {
        process::drop_ctty(pty.ctty());
        process::signal_pgrp(fg, process::SIGHUP);
        process::signal_pgrp(fg, process::SIGCONT);
        process::signal_pid(sid, process::SIGHUP);
        process::signal_pid(sid, process::SIGCONT);
    }
}

/// open("/dev/ptmx"): a new pseudo-terminal, locked, owned by the caller.
pub fn open_master(nonblocking: bool) -> Result<End, i64> {
    let cred = process::current_cred();
    let mut ptys = PTYS.lock();
    let index = (0..MAX_PTYS).find(|n| ptys.get(n).is_none_or(|w| w.upgrade().is_none())).ok_or(crate::syscall::ENOSPC)?;
    let pty = Arc::new(Pty {
        index,
        state: Mutex::new(State {
            termios: Termios::DEFAULT,
            line: Vec::new(),
            lines: VecDeque::new(),
            raw: VecDeque::new(),
            output: VecDeque::new(),
            locked: true,
            sid: 0,
            fg: 0,
            winsize: [24, 80, 0, 0],
            hung_up: false,
            slave_opened: false,
            uid: cred.ruid,
            gid: cred.rgid,
            mode: 0o620,
        }),
        masters: AtomicUsize::new(1),
        slaves: AtomicUsize::new(0),
    });
    ptys.insert(index, Arc::downgrade(&pty));
    Ok(End { pty, master: true, nonblocking: AtomicBool::new(nonblocking) })
}

/// open("/dev/pts/N") (permissions were checked by the caller). Without
/// O_NOCTTY, a session leader with no controlling terminal gets this one.
pub fn open_slave(n: u32, nonblocking: bool, noctty: bool) -> Result<End, i64> {
    let pty = get(n).ok_or(EIO)?;
    {
        let mut s = pty.state.lock();
        if s.locked {
            return Err(EIO);
        }
        s.slave_opened = true;
    }
    pty.slaves.fetch_add(1, Ordering::SeqCst);
    if !noctty {
        let (sid, pgid, ctty, leader) = process::current_session();
        if leader && ctty == CTTY_NONE {
            let _ = make_ctty(&pty, sid, pgid, false);
        }
    }
    Ok(End { pty, master: false, nonblocking: AtomicBool::new(nonblocking) })
}

/// Make `pty` the controlling terminal of session `sid` (foreground group
/// `pgid`). `steal`: root taking it from another session (TIOCSCTTY 1).
fn make_ctty(pty: &Arc<Pty>, sid: u64, pgid: u64, steal: bool) -> Result<(), i64> {
    {
        let mut s = pty.state.lock();
        if s.sid != 0 && s.sid != sid {
            if !steal {
                return Err(EPERM);
            }
            process::drop_ctty(pty.ctty());
        }
        s.sid = sid;
        s.fg = pgid;
    }
    process::set_session_ctty(sid, pty.ctty());
    Ok(())
}

/// The second open of an existing terminal through /dev/tty.
pub fn reopen_slave(pty: &Arc<Pty>) -> End {
    pty.slaves.fetch_add(1, Ordering::SeqCst);
    End { pty: pty.clone(), master: false, nonblocking: AtomicBool::new(false) }
}

// ---- the line discipline ----

fn is_ctl(c: u8) -> bool {
    c < 0x20 || c == 0x7f
}

impl State {
    /// Queue output for the master (after output processing).
    fn put_output(&mut self, data: &[u8]) {
        let onlcr = self.termios.oflag & (OPOST | ONLCR) == OPOST | ONLCR;
        for &c in data {
            if self.output.len() >= OUTPUT_MAX {
                return; // (echo when nobody reads: dropped)
            }
            if c == b'\n' && onlcr {
                self.output.push_back(b'\r');
            }
            self.output.push_back(c);
        }
    }

    /// Echo a typed character: control characters as ^X with ECHOCTL.
    fn echo(&mut self, c: u8) {
        if self.termios.lflag & ECHO == 0 {
            return;
        }
        if self.termios.lflag & ECHOCTL != 0 && is_ctl(c) && c != b'\n' && c != b'\t' {
            self.put_output(&[b'^', if c == 0x7f { b'?' } else { c + 0x40 }]);
        } else {
            self.put_output(&[c]);
        }
    }

    /// Erase the last character of the line being edited.
    fn erase_one(&mut self) -> bool {
        let Some(c) = self.line.pop() else { return false };
        if self.termios.lflag & ECHO != 0 {
            let width = if self.termios.lflag & ECHOCTL != 0 && is_ctl(c) && c != b'\t' { 2 } else { 1 };
            for _ in 0..width {
                if self.termios.lflag & ECHOE != 0 {
                    self.put_output(b"\x08 \x08");
                } else {
                    self.put_output(b"\x08");
                }
            }
        }
        true
    }

    fn input_len(&self) -> usize {
        self.raw.len() + self.line.len() + self.lines.iter().map(Vec::len).sum::<usize>()
    }

    /// One typed byte. Returns a signal for the foreground group, if it was
    /// a signal character.
    fn input(&mut self, mut c: u8) -> Option<i32> {
        let t = self.termios;
        if c == b'\r' {
            if t.iflag & IGNCR != 0 {
                return None;
            }
            if t.iflag & ICRNL != 0 {
                c = b'\n';
            }
        } else if c == b'\n' && t.iflag & INLCR != 0 {
            c = b'\r';
        }
        let is = |i: usize| t.cc[i] != 0 && c == t.cc[i];
        if t.lflag & ISIG != 0 {
            let sig = if is(VINTR) {
                Some(process::SIGINT)
            } else if is(VQUIT) {
                Some(process::SIGQUIT)
            } else if is(VSUSP) {
                Some(process::SIGTSTP)
            } else {
                None
            };
            if let Some(sig) = sig {
                if t.lflag & NOFLSH == 0 {
                    self.line.clear();
                    self.lines.clear();
                    self.raw.clear();
                }
                self.echo(c);
                if t.lflag & ECHO != 0 && t.lflag & ECHOCTL != 0 {
                    self.put_output(b"\n");
                }
                return Some(sig);
            }
        }
        if t.lflag & ICANON == 0 {
            if self.raw.len() < INPUT_MAX {
                self.raw.push_back(c);
                self.echo(c);
            }
            return None;
        }
        if is(VERASE) || (c == 0x08 && t.cc[VERASE] == 0x7f) {
            self.erase_one();
        } else if is(VKILL) {
            if t.lflag & ECHOK != 0 && t.lflag & ECHOKE == 0 && t.lflag & ECHO != 0 {
                self.line.clear();
                self.echo(c);
                self.put_output(b"\n");
            } else {
                while self.erase_one() {}
            }
        } else if is(VWERASE) && t.lflag & IEXTEN != 0 {
            while self.line.last().is_some_and(|&b| b == b' ' || b == b'\t') {
                self.erase_one();
            }
            while self.line.last().is_some_and(|&b| b != b' ' && b != b'\t') {
                self.erase_one();
            }
        } else if is(VEOF) {
            // Hand over the line as it is (none: end-of-file).
            self.lines.push_back(core::mem::take(&mut self.line));
        } else if c == b'\n' || is(VEOL) || is(VEOL2) {
            self.line.push(c);
            if t.lflag & ECHO != 0 || (c == b'\n' && t.lflag & ECHONL != 0) {
                self.put_output(&[c]);
            }
            self.lines.push_back(core::mem::take(&mut self.line));
        } else if self.input_len() < INPUT_MAX - 1 {
            self.line.push(c);
            self.echo(c);
        }
        None
    }

    /// Can the slave read something now (data, or end-of-file)?
    fn slave_readable(&self) -> bool {
        if self.termios.lflag & ICANON != 0 {
            !self.lines.is_empty() || !self.raw.is_empty()
        } else {
            !self.raw.is_empty() || !self.lines.is_empty()
        }
    }

    /// Take input for the slave: one line in canonical mode, everything
    /// waiting otherwise. Some(0) is end-of-file.
    fn take_input(&mut self, buf: &mut [u8]) -> usize {
        if self.termios.lflag & ICANON == 0 {
            // Lines finished before raw mode was switched on come first.
            while let Some(l) = self.lines.pop_front() {
                self.raw.extend(l);
            }
        }
        if !self.raw.is_empty() {
            let n = buf.len().min(self.raw.len());
            for (d, s) in buf.iter_mut().zip(self.raw.drain(..n)) {
                *d = s;
            }
            return n;
        }
        let Some(front) = self.lines.front_mut() else { return 0 };
        let n = buf.len().min(front.len());
        buf[..n].copy_from_slice(&front[..n]);
        front.drain(..n);
        if front.is_empty() {
            self.lines.pop_front();
        }
        n
    }

    /// Switching modes: input not yet handed over moves with it.
    fn set_termios(&mut self, t: Termios) {
        let was_canon = self.termios.lflag & ICANON != 0;
        self.termios = t;
        if was_canon && t.lflag & ICANON == 0 {
            while let Some(l) = self.lines.pop_front() {
                self.raw.extend(l);
            }
            self.raw.extend(self.line.drain(..));
        }
    }
}

// ---- reading and writing ----

/// The master "types" `data`. Signal characters signal the foreground
/// group. Blocks while the input is full (non-canonical mode).
pub fn master_write(end: &End, data: &[u8]) -> i64 {
    let pty = &end.pty;
    let mut done = 0;
    loop {
        let mut sigs = Vec::new();
        let fg;
        {
            let mut s = pty.state.lock();
            while done < data.len() {
                if s.termios.lflag & ICANON == 0 && s.raw.len() >= INPUT_MAX {
                    break; // full: wait for the slave to read
                }
                if let Some(sig) = s.input(data[done]) {
                    sigs.push(sig);
                }
                done += 1;
            }
            fg = s.fg;
        }
        pty.wake();
        for sig in sigs {
            process::signal_pgrp(fg, sig);
        }
        if done == data.len() {
            return done as i64;
        }
        if done > 0 && end.is_nonblocking() {
            return done as i64;
        }
        if end.is_nonblocking() {
            return -EAGAIN;
        }
        if process::signal_pending() {
            return if done > 0 { done as i64 } else { -EINTR };
        }
        process::block_on(pty.key());
    }
}

/// The master reads what the slave wrote (and echoes). EIO once every
/// slave fd is closed (the program on the terminal is gone).
pub fn master_read(end: &End, buf: &mut [u8]) -> i64 {
    let pty = &end.pty;
    if buf.is_empty() {
        return 0;
    }
    loop {
        {
            let mut s = pty.state.lock();
            if !s.output.is_empty() {
                let n = buf.len().min(s.output.len());
                for (d, c) in buf.iter_mut().zip(s.output.drain(..n)) {
                    *d = c;
                }
                drop(s);
                pty.wake(); // room for blocked slave writers
                return n as i64;
            }
            if s.slave_opened && pty.slaves.load(Ordering::SeqCst) == 0 {
                return -EIO;
            }
        }
        if end.is_nonblocking() {
            return -EAGAIN;
        }
        if process::signal_pending() {
            return -EINTR;
        }
        process::block_on(pty.key());
    }
}

/// A process in the background reading its terminal gets SIGTTIN (or
/// EIO, if it can't be stopped), as POSIX says.
fn background_read_check(pty: &Arc<Pty>) -> Result<(), i64> {
    let (_, pgid, ctty, _) = process::current_session();
    let fg = pty.state.lock().fg;
    if ctty != pty.ctty() || fg == 0 || pgid == fg {
        return Ok(());
    }
    if process::ignores_or_blocks(process::SIGTTIN) || process::current_pgrp_orphaned() {
        return Err(EIO);
    }
    process::signal_pgrp(pgid, process::SIGTTIN);
    Err(EINTR) // (restarted once the group is continued in the foreground)
}

/// A program on the terminal reads typed input.
pub fn slave_read(end: &End, buf: &mut [u8]) -> i64 {
    let pty = &end.pty;
    if let Err(e) = background_read_check(pty) {
        return -e;
    }
    if buf.is_empty() {
        return 0;
    }
    let mut deadline = None;
    loop {
        let (vmin, vtime);
        {
            let mut s = pty.state.lock();
            if s.slave_readable() {
                let n = s.take_input(buf);
                drop(s);
                pty.wake(); // room for a blocked master
                return n as i64;
            }
            if s.hung_up {
                return 0;
            }
            (vmin, vtime) = (s.termios.cc[VMIN], s.termios.cc[VTIME]);
            if s.termios.lflag & ICANON == 0 && vmin == 0 {
                if vtime == 0 {
                    return 0; // polling read: nothing there
                }
                // Wait at most VTIME tenths of a second.
                let d = *deadline.get_or_insert_with(|| {
                    process::ticks() + (vtime as u64 * 100_000_000).div_ceil(process::TICK_NS)
                });
                if process::ticks() >= d {
                    return 0;
                }
            }
        }
        if end.is_nonblocking() {
            return -EAGAIN;
        }
        if process::signal_pending() {
            return -EINTR;
        }
        match deadline {
            Some(d) => process::block_on_until(pty.key(), d),
            None => process::block_on(pty.key()),
        }
    }
}

/// A program on the terminal writes: the master will read it.
pub fn slave_write(end: &End, data: &[u8]) -> i64 {
    let pty = &end.pty;
    let mut done = 0;
    loop {
        {
            let mut s = pty.state.lock();
            if s.hung_up {
                return if done > 0 { done as i64 } else { -EIO };
            }
            // Room for the worst case ("\n" -> "\r\n") of a chunk.
            let room = OUTPUT_MAX.saturating_sub(s.output.len()) / 2;
            let n = room.min(data.len() - done);
            if n > 0 {
                s.put_output(&data[done..done + n]);
                done += n;
            }
        }
        if done > 0 {
            pty.wake();
        }
        if done == data.len() {
            return done as i64;
        }
        if end.is_nonblocking() {
            return if done > 0 { done as i64 } else { -EAGAIN };
        }
        if process::signal_pending() {
            return if done > 0 { done as i64 } else { -EINTR };
        }
        process::block_on(pty.key());
    }
}

/// For poll: (readable, writable, hung up).
pub fn readiness(end: &End) -> (bool, bool, bool) {
    let s = end.pty.state.lock();
    if end.master {
        let gone = s.slave_opened && end.pty.slaves.load(Ordering::SeqCst) == 0;
        (!s.output.is_empty() || gone, s.raw.len() < INPUT_MAX, gone)
    } else {
        (s.slave_readable() || s.hung_up, s.output.len() < OUTPUT_MAX && !s.hung_up, s.hung_up)
    }
}

/// Bytes waiting to be read on this end (FIONREAD).
fn pending(end: &End) -> usize {
    let s = end.pty.state.lock();
    if end.master {
        s.output.len()
    } else if s.termios.lflag & ICANON != 0 {
        s.raw.len() + s.lines.front().map_or(0, Vec::len)
    } else {
        s.raw.len() + s.lines.iter().map(Vec::len).sum::<usize>()
    }
}

// ---- ioctl ----

const TCGETS: u64 = 0x5401;
const TCSETS: u64 = 0x5402;
const TCSETSW: u64 = 0x5403;
const TCSETSF: u64 = 0x5404;
const TCSBRK: u64 = 0x5409;
const TCXONC: u64 = 0x540A;
const TCFLSH: u64 = 0x540B;
const TIOCEXCL: u64 = 0x540C;
const TIOCNXCL: u64 = 0x540D;
const TIOCSCTTY: u64 = 0x540E;
const TIOCGPGRP: u64 = 0x540F;
const TIOCSPGRP: u64 = 0x5410;
const TIOCOUTQ: u64 = 0x5411;
const TIOCGWINSZ: u64 = 0x5413;
const TIOCSWINSZ: u64 = 0x5414;
const FIONREAD: u64 = 0x541B;
const FIONBIO: u64 = 0x5421;
const TIOCNOTTY: u64 = 0x5422;
const TIOCGSID: u64 = 0x5429;
const TCSBRKP: u64 = 0x5425;
const TIOCGPTN: u64 = 0x8004_5430;
const TIOCSPTLCK: u64 = 0x4004_5431;

fn read_i32(arg: u64) -> Result<i32, i64> {
    let b = unsafe { user_slice(arg, 4) }.ok_or(EFAULT)?;
    Ok(i32::from_le_bytes(b.try_into().unwrap()))
}

fn write_i32(arg: u64, v: i32) -> Result<i64, i64> {
    let b = unsafe { user_slice_mut(arg, 4) }.ok_or(EFAULT)?;
    b.copy_from_slice(&v.to_le_bytes());
    Ok(0)
}

pub fn ioctl(end: &End, request: u64, arg: u64) -> i64 {
    let pty = &end.pty;
    let r: Result<i64, i64> = (|| match request {
        TCGETS => {
            let out = unsafe { user_slice_mut(arg, TERMIOS_SIZE as u64) }.ok_or(EFAULT)?;
            out.copy_from_slice(&pty.state.lock().termios.to_bytes());
            Ok(0)
        }
        TCSETS | TCSETSW | TCSETSF => {
            let input = unsafe { user_slice(arg, TERMIOS_SIZE as u64) }.ok_or(EFAULT)?;
            let t = Termios::from_bytes(input);
            let mut s = pty.state.lock();
            if request == TCSETSF {
                s.line.clear();
                s.lines.clear();
                s.raw.clear();
            }
            s.set_termios(t);
            drop(s);
            pty.wake();
            Ok(0)
        }
        TCFLSH => {
            let mut s = pty.state.lock();
            // 0: input, 1: output, 2: both (TCIFLUSH, TCOFLUSH, TCIOFLUSH)
            if arg == 0 || arg == 2 {
                s.line.clear();
                s.lines.clear();
                s.raw.clear();
            }
            if arg == 1 || arg == 2 {
                s.output.clear();
            }
            drop(s);
            pty.wake();
            Ok(0)
        }
        TCSBRK | TCSBRKP | TCXONC | TIOCEXCL | TIOCNXCL => Ok(0),
        TIOCGWINSZ => {
            let ws = pty.state.lock().winsize;
            let out = unsafe { user_slice_mut(arg, 8) }.ok_or(EFAULT)?;
            for (i, v) in ws.iter().enumerate() {
                out[i * 2..i * 2 + 2].copy_from_slice(&v.to_le_bytes());
            }
            Ok(0)
        }
        TIOCSWINSZ => {
            let input = unsafe { user_slice(arg, 8) }.ok_or(EFAULT)?;
            let mut ws = [0u16; 4];
            for (i, v) in ws.iter_mut().enumerate() {
                *v = u16::from_le_bytes([input[i * 2], input[i * 2 + 1]]);
            }
            let (changed, fg) = {
                let mut s = pty.state.lock();
                let changed = s.winsize != ws;
                s.winsize = ws;
                (changed, s.fg)
            };
            if changed {
                process::signal_pgrp(fg, process::SIGWINCH);
            }
            Ok(0)
        }
        FIONREAD => write_i32(arg, pending(end) as i32),
        TIOCOUTQ => write_i32(arg, if end.master { 0 } else { pty.state.lock().output.len() as i32 }),
        FIONBIO => {
            end.set_nonblocking(read_i32(arg)? != 0);
            Ok(0)
        }
        TIOCGPTN if end.master => write_i32(arg, pty.index as i32),
        TIOCSPTLCK if end.master => {
            let lock = read_i32(arg)? != 0;
            pty.state.lock().locked = lock;
            Ok(0)
        }
        TIOCSCTTY => {
            // The caller must lead a session that has no terminal yet.
            let (sid, pgid, ctty, leader) = process::current_session();
            if ctty == pty.ctty() {
                return Ok(0);
            }
            if !leader || ctty != CTTY_NONE {
                return Err(EPERM);
            }
            let steal = arg == 1 && process::current_cred().euid == 0;
            make_ctty(pty, sid, pgid, steal).map(|_| 0)
        }
        TIOCNOTTY => {
            let (_, _, ctty, leader) = process::current_session();
            if ctty != pty.ctty() {
                return Err(ENOTTY);
            }
            if leader {
                // The session leader lets go: the whole session loses it,
                // and the foreground group is told.
                let fg = {
                    let mut s = pty.state.lock();
                    let fg = s.fg;
                    s.sid = 0;
                    s.fg = 0;
                    fg
                };
                process::drop_ctty(pty.ctty());
                process::signal_pgrp(fg, process::SIGHUP);
                process::signal_pgrp(fg, process::SIGCONT);
            } else {
                process::set_session_ctty_of_current(CTTY_NONE);
            }
            Ok(0)
        }
        TIOCGPGRP => {
            let s = pty.state.lock();
            if !end.master && process::current_session().2 != pty.ctty() {
                return Err(ENOTTY);
            }
            write_i32(arg, s.fg as i32)
        }
        TIOCSPGRP => {
            let pgid = read_i32(arg)?;
            if pgid <= 0 {
                return Err(EINVAL);
            }
            let (sid, _, ctty, _) = process::current_session();
            let mut s = pty.state.lock();
            if ctty != pty.ctty() || s.sid != sid {
                return Err(ENOTTY);
            }
            s.fg = pgid as u64;
            Ok(0)
        }
        TIOCGSID => {
            let sid = pty.state.lock().sid;
            if sid == 0 {
                return Err(ENOTTY);
            }
            write_i32(arg, sid as i32)
        }
        _ => Err(ENOTTY),
    })();
    r.unwrap_or_else(|e| -e)
}

// Copyright (c) 2026 tre4surehunter9
//! Processes and the scheduler.
//!
//! Each user process has its own address space (level-4 page table), its own
//! 16 KiB kernel stack, and its own open files. Interrupts and syscalls from
//! ring 3 land on that process's kernel stack (the TSS's RSP0 is updated on
//! every switch), so the user's registers are saved there, and switching
//! processes is just switching kernel stacks (`context_switch`) plus CR3.
//!
//! Task 0 is the kernel itself: the boot stack running the kernel shell. It
//! never enters ring 3; it starts programs and waits for them.
//!
//! Scheduling is round-robin. A process is preempted when the timer fires
//! while it's in ring 3, and gives up the CPU when it waits (wait4, sched_yield,
//! or reading the keyboard while others want to run).

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::arch::global_asm;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;
use x86_64::instructions::interrupts;
use x86_64::registers::control::Cr3;
use x86_64::structures::paging::PhysFrame;
use x86_64::VirtAddr;

use crate::memory::{self, FRAME_ALLOCATOR};
use crate::syscall::*;
use crate::usermode::{self, ERR_NO_MEMORY, ERR_TOO_MANY_ARGS};
use crate::vfs::{self, Files};

/// Processes and threads together.
pub const MAX_PROCESSES: usize = 64;
/// Each process's kernel stack (debug builds need plenty: unoptimized code
/// has big stack frames, and syscall dispatch is one big function).
const KERNEL_STACK_SIZE: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitReason {
    Exited(i32),
    Signaled(i32),
}

// si_code values (Linux's)
pub const SI_USER: i32 = 0;
pub const SI_KERNEL: i32 = 0x80;
pub const SI_QUEUE: i32 = -1;
pub const SI_TIMER: i32 = -2;
pub const SI_TKILL: i32 = -6;
pub const CLD_EXITED: i32 = 1;
pub const CLD_KILLED: i32 = 2;
pub const CLD_STOPPED: i32 = 5;
pub const CLD_CONTINUED: i32 = 6;
pub const SEGV_MAPERR: i32 = 1;
pub const SEGV_ACCERR: i32 = 2;
pub const ILL_ILLOPN: i32 = 2;
pub const FPE_INTDIV: i32 = 1;
pub const FPE_FLTDIV: i32 = 3;
pub const FPE_FLTOVF: i32 = 4;
pub const FPE_FLTUND: i32 = 5;
pub const FPE_FLTRES: i32 = 6;
pub const FPE_FLTINV: i32 = 7;
pub const TRAP_BRKPT: i32 = 1;

/// Why a signal was sent, for the handler's siginfo (SA_SIGINFO) and
/// sigtimedwait: si_code and the fields that go with it.
#[derive(Clone, Copy, Debug, Default)]
pub struct SigInfo {
    pub code: i32,
    /// si_pid (the sender, or the child for SIGCHLD); a timer's id.
    pub pid: i32,
    /// si_uid; a timer's overrun.
    pub uid: u32,
    /// si_addr (faults), si_value (sigqueue, timers).
    pub word: u64,
    /// si_status (SIGCHLD: the exit code or the signal).
    pub status: i32,
}

impl SigInfo {
    /// Sent by the kernel itself (SIGPIPE, SIGALRM, SIGIO, keyboard signals...).
    pub const KERNEL: SigInfo = SigInfo { code: SI_KERNEL, pid: 0, uid: 0, word: 0, status: 0 };

    /// Sent by the current process (kill: SI_USER, tkill: SI_TKILL...).
    fn from_current(table: &BTreeMap<u64, Box<Process>>, code: i32, value: u64) -> SigInfo {
        let (pid, uid) = table.get(&current_pid()).map_or((0, 0), |p| (p.tgid as i32, p.cred.ruid));
        SigInfo { code, pid, uid, word: value, status: 0 }
    }

    pub fn fault(code: i32, addr: u64) -> SigInfo {
        SigInfo { code, word: addr, ..SigInfo::default() }
    }

    /// The 128-byte siginfo_t of Linux x86_64 for signal `sig`.
    pub fn bytes(&self, sig: i32) -> [u8; 128] {
        let mut b = [0u8; 128];
        b[0..4].copy_from_slice(&sig.to_le_bytes());
        b[8..12].copy_from_slice(&self.code.to_le_bytes());
        let faulting = matches!(sig, SIGSEGV | SIGBUS | SIGILL | SIGFPE | SIGTRAP);
        if faulting && self.code > 0 {
            b[16..24].copy_from_slice(&self.word.to_le_bytes()); // si_addr
        } else {
            b[16..20].copy_from_slice(&self.pid.to_le_bytes()); // si_pid / si_timerid
            b[20..24].copy_from_slice(&self.uid.to_le_bytes()); // si_uid / si_overrun
            if sig == SIGCHLD && self.code > 0 {
                b[24..28].copy_from_slice(&self.status.to_le_bytes()); // si_status
            } else {
                b[24..32].copy_from_slice(&self.word.to_le_bytes()); // si_value
            }
        }
        b
    }
}

/// An alternate signal stack (sigaltstack): handlers with SA_ONSTACK run on
/// it (a program can handle its own stack overflowing).
#[derive(Clone, Copy, Debug)]
pub struct AltStack {
    pub sp: u64,
    pub size: u64,
    /// SS_DISABLE, or 0 (enabled), with SS_AUTODISARM.
    pub flags: u32,
}

pub const SS_ONSTACK: u32 = 1;
pub const SS_DISABLE: u32 = 2;
pub const SS_AUTODISARM: u32 = 1 << 31;

impl AltStack {
    pub const DISABLED: AltStack = AltStack { sp: 0, size: 0, flags: SS_DISABLE };

    /// Is `rsp` on it?
    pub fn holds(&self, rsp: u64) -> bool {
        self.flags & SS_DISABLE == 0 && rsp > self.sp && rsp - self.sp <= self.size
    }

    /// Its ss_flags as seen from stack pointer `rsp` (sigaltstack, uc_stack).
    pub fn flags_at(&self, rsp: u64) -> u32 {
        if self.flags & SS_DISABLE != 0 {
            SS_DISABLE
        } else if self.holds(rsp) {
            SS_ONSTACK
        } else {
            self.flags & SS_AUTODISARM
        }
    }
}

impl ExitReason {
    /// Encoding used by wait4's status (what WIFEXITED/WEXITSTATUS decode).
    fn wait_status(self) -> i32 {
        match self {
            ExitReason::Exited(code) => (code & 0xff) << 8,
            ExitReason::Signaled(sig) => sig & 0x7f,
        }
    }

    /// Shell-style status: the exit code, or 128 + signal.
    pub fn shell_status(self) -> i32 {
        match self {
            ExitReason::Exited(code) => code,
            ExitReason::Signaled(sig) => 128 + sig,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Running,
    Ready,
    /// Waiting for a child to exit.
    Waiting,
    /// In nanosleep until the timer reaches `Process::wake_at`.
    Sleeping,
    /// Waiting for something identified by a key (e.g. a pipe to have data
    /// or room); `wake(key)` makes it Ready again.
    Blocked(u64),
    /// Stopped by a signal (SIGSTOP, Ctrl-Z) until SIGCONT.
    Stopped,
    Zombie(ExitReason),
}

/// A process's memory bookkeeping (shared by its threads).
#[derive(Clone)]
pub struct Mm {
    /// Current program break (end of the brk heap), see sys_brk.
    pub brk: u64,
    /// mmap'd ranges, sorted: (start, end, protection). Anonymous memory
    /// gets its pages when first touched (see mmap::fault_in).
    pub maps: Vec<crate::mmap::Map>,
}

impl Mm {
    fn new() -> Arc<Mutex<Mm>> {
        Arc::new(Mutex::new(Mm { brk: HEAP_BASE, maps: Vec::new() }))
    }
}

pub struct Process {
    pub pid: u64,
    /// The cores it may run on (sched_setaffinity; bit N: core N).
    pub affinity: u64,
    pub ppid: u64,
    pub name: String,
    pub state: State,
    /// Saved kernel stack pointer while not running (see context_switch).
    kernel_rsp: u64,
    /// None for task 0, which runs on the boot stack.
    kernel_stack: Option<Box<[u8]>>,
    pub p4: PhysFrame,
    /// Open files and the working directory: shared by a process's threads.
    pub files: Arc<Mutex<Files>>,
    /// Current program break (end of the brk heap), see sys_brk.
    /// The heap end and mmap list: shared by a process's threads (they share
    /// the address space).
    pub mm: Arc<Mutex<Mm>>,
    /// Process group (for Ctrl-C and kill(-pgid)).
    pub pgid: u64,
    /// Session (setsid): the pid of its leader.
    pub sid: u64,
    /// Controlling terminal: CTTY_CONSOLE, CTTY_NONE, or pseudo-terminal
    /// N as N + 1.
    pub ctty: u32,
    /// Signals sent but not yet acted on (bit n-1 = signal n).
    pending: u64,
    /// Why each pending signal was sent (the latest of each).
    siginfo: BTreeMap<i32, SigInfo>,
    /// sigaltstack: where handlers with SA_ONSTACK run.
    pub altstack: AltStack,
    /// Signals this thread waits for in rt_sigtimedwait (sigwait): one
    /// blocked by the thread it was sent to comes here instead.
    sigwait: u64,
    /// What to do for each signal (index n-1), see sigaction.
    /// (Boxed: it's 2 KiB, and Process values pass through small kernel stacks.)
    actions: Box<[SigAction; MAX_SIGNAL as usize]>,
    /// The signal mask: blocked signals stay pending until unblocked.
    blocked: u64,
    /// The mask to go back to after sigsuspend's handler.
    saved_mask: Option<u64>,
    /// SIGALRM timer: the tick it fires at (0 = off) and its repeat interval.
    alarm_at: u64,
    alarm_every: u64,
    /// A stop or continue not yet reported to the parent by wait4.
    wait_event: Option<u32>,
    /// Timer tick to wake up at, while Sleeping.
    wake_at: u64,
    /// The program's arguments and path (for /proc and ps).
    pub argv: Vec<String>,
    pub exe: String,
    /// Timer tick when the process was created.
    pub start_tick: u64,
    /// CPU time used, in ticks (not counting the current run).
    cpu_ticks: u64,
    /// Tick when it last started running.
    running_since: u64,
    /// Who the process runs as.
    pub cred: Cred,
    /// FPU/SSE registers while the process isn't running.
    fpu: Box<crate::fpu::FpuState>,
    /// FS and GS base addresses (thread-local storage, see arch_prctl).
    fs_base: u64,
    gs_base: u64,
    /// The thread group (process) this thread belongs to: its first
    /// thread's pid. getpid() returns it; gettid() returns `pid`.
    pub tgid: u64,
    /// set_tid_address / CLONE_CHILD_CLEARTID: zeroed and futex-woken when
    /// the thread exits (pthread_join waits on it).
    clear_child_tid: u64,
    /// A futex wait with a timeout: the tick it gives up at (0 = none).
    deadline: u64,
    /// A vfork child using its parent's address space (until it calls
    /// execve or exits; the parent waits meanwhile). It must not free `p4`.
    borrowed_vm: bool,
}

/// Credentials: real, effective and saved user and group IDs, and the
/// supplementary groups. Permission checks use the effective IDs; the real
/// ones say who started the process; the saved ones let a setuid program
/// switch back and forth.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cred {
    pub ruid: u32,
    pub euid: u32,
    pub suid: u32,
    pub rgid: u32,
    pub egid: u32,
    pub sgid: u32,
    pub groups: Vec<u32>,
}

impl Cred {
    pub const ROOT: Cred = Cred { ruid: 0, euid: 0, suid: 0, rgid: 0, egid: 0, sgid: 0, groups: Vec::new() };

    pub fn in_group(&self, gid: u32) -> bool {
        self.egid == gid || self.groups.contains(&gid)
    }

    pub fn is_root(&self) -> bool {
        self.euid == 0
    }
}

/// The brk heap starts here: level-4 slot 32, unused by the kernel.
pub const HEAP_BASE: u64 = 0x1000_0000_0000;
/// Largest brk heap a process may have.
pub const HEAP_MAX: u64 = 256 * 1024 * 1024;

/// Environment for programs started by the kernel.
/// /bin: PalladiumOS's programs; /usr/bin: BusyBox's tools (on the system disk).
/// (TERM=ansi: the console speaks the standard ANSI sequences, and every
/// system knows the name, which matters for programs on other machines
/// reached over SSH.)
pub const DEFAULT_ENV: &[&str] = &["PATH=/bin:/usr/bin", "HOME=/", "SHELL=/bin/sh", "TERM=ansi"];

impl Process {
    fn kernel_stack_top(&self) -> Option<u64> {
        let stack = self.kernel_stack.as_ref()?;
        Some((stack.as_ptr() as u64 + stack.len() as u64) & !0xf)
    }
}

/// Boxed so each Process stays at a fixed address: context_switch saves the
/// stack pointer straight into `kernel_rsp` after the table lock is released.
static TABLE: Mutex<BTreeMap<u64, Box<Process>>> = Mutex::new(BTreeMap::new());
static NEXT_PID: AtomicU64 = AtomicU64::new(1);
/// "No process": what a core runs in its idle loop (smp::idle_loop).
pub const IDLE: u64 = u64::MAX;

global_asm!(
    // context_switch(save_rsp: *mut u64, new_rsp: u64)
    // Save callee-saved registers on the current stack, store the stack
    // pointer in *save_rsp, then load new_rsp and pop the next task's
    // registers. `ret` continues wherever that task last switched out (or at
    // syscall_return for a brand-new process).
    ".global context_switch",
    "context_switch:",
    "push rbp",
    "push rbx",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    "mov [rdi], rsp",
    "mov rsp, rsi",
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop rbx",
    "pop rbp",
    "ret",
);

unsafe extern "C" {
    fn context_switch(save_rsp: *mut u64, new_rsp: u64);
}

/// Register task 0 (the kernel). Call once at boot, after the heap is up.
pub fn init() {
    memory::init_address_spaces();
    let mut table = TABLE.lock();
    if table.contains_key(&0) {
        return;
    }
    table.insert(0, Box::new(Process {
        pid: 0,
        ppid: 0,
        name: "kernel".to_string(),
        state: State::Running,
        kernel_rsp: 0,
        kernel_stack: None,
        p4: memory::kernel_p4(),
        files: Arc::new(Mutex::new(Files::new("/"))),
        mm: Mm::new(),
        pgid: 0,
        sid: 0,
        ctty: CTTY_CONSOLE,
        pending: 0,
        siginfo: BTreeMap::new(),
        altstack: AltStack::DISABLED,
        sigwait: 0,
        actions: Box::new([SigAction::DEFAULT; MAX_SIGNAL as usize]),
        blocked: 0,
        saved_mask: None,
        alarm_at: 0,
        alarm_every: 0,
        wait_event: None,
        wake_at: 0,
        argv: vec!["kernel".to_string()],
        exe: String::new(),
        start_tick: 0,
        affinity: u64::MAX,
        cpu_ticks: 0,
        running_since: 0,
        cred: Cred::ROOT,
        fpu: Box::new(crate::fpu::FpuState::initial()),
        fs_base: 0,
        gs_base: 0,
        tgid: 0,
        clear_child_tid: 0,
        deadline: 0,
        borrowed_vm: false,
    }));
    drop(table);
    crate::smp::PERCPU[0].current.store(0, Ordering::SeqCst);
    // Core 0's idle context: a stack whose first switch "returns" into the
    // idle loop (the way a new process's returns into syscall_return).
    let stack: &'static mut [u8] = Box::leak(vec![0u8; KERNEL_STACK_SIZE].into_boxed_slice());
    let top = (stack.as_ptr() as u64 + stack.len() as u64) & !0xf;
    let rsp = top - 8 * 8;
    unsafe {
        let words = rsp as *mut u64;
        for i in 0..6 {
            words.add(i).write(0);
        }
        words.add(6).write(crate::smp::idle_entry as *const () as u64);
    }
    crate::smp::PERCPU[0].idle_rsp.store(rsp, Ordering::SeqCst);
}

/// The process (thread) running on this core (IDLE in its idle loop).
pub fn current_pid() -> u64 {
    crate::smp::this().current.load(Ordering::SeqCst)
}

pub fn current_ppid() -> u64 {
    TABLE.lock().get(&current_pid()).map(|p| p.ppid).unwrap_or(0)
}

/// Run `f` on the current process's open files.
pub fn with_current_files<T>(f: impl FnOnce(&mut Files) -> T) -> T {
    // (The table lock is released first: the files are shared with other
    // threads, and have their own lock.)
    let files = TABLE.lock().get(&current_pid()).expect("no current process").files.clone();
    let mut guard = files.lock();
    f(&mut guard)
}

/// The current process's root directory (chroot), as a real path: "/"
/// unless it changed it. (Safe during boot, before there's a process.)
pub fn current_root() -> String {
    let files = TABLE.lock().get(&current_pid()).map(|p| p.files.clone());
    files.map_or_else(|| String::from("/"), |f| f.lock().root.clone())
}

/// Run `f` on the current process's memory bookkeeping (brk, mmap list).
/// Like with_current_mm, but None instead of waiting if the process table
/// or the memory description is busy (for the page fault path: a fault
/// can't wait for a lock the faulting code itself holds).
pub fn try_with_current_mm<T>(f: impl FnOnce(&mut Mm) -> T) -> Option<T> {
    let mm = TABLE.try_lock()?.get(&current_pid())?.mm.clone();
    let mut guard = mm.try_lock()?;
    Some(f(&mut guard))
}

fn current_altstack() -> AltStack {
    TABLE.lock().get(&current_pid()).map_or(AltStack::DISABLED, |p| p.altstack)
}

/// sigaltstack(new, old), with the program's stack pointer `rsp` (to say
/// whether it's on the alternate stack, which mustn't change while it is).
pub fn sys_sigaltstack(new: u64, old: u64, rsp: u64) -> i64 {
    const MINSIGSTKSZ: u64 = 2048;
    let mut table = TABLE.lock();
    let Some(p) = table.get_mut(&current_pid()) else { return -ESRCH };
    let cur = p.altstack;
    let new_stack = if new != 0 {
        let Some(n) = (unsafe { user_slice(new, 24) }) else { return -EFAULT };
        let sp = u64::from_le_bytes(n[0..8].try_into().unwrap());
        let flags = u32::from_le_bytes(n[8..12].try_into().unwrap());
        let size = u64::from_le_bytes(n[16..24].try_into().unwrap());
        if cur.holds(rsp) {
            return -EPERM; // (on it now)
        }
        if flags & !(SS_DISABLE | SS_AUTODISARM | SS_ONSTACK) != 0 {
            return -EINVAL;
        }
        Some(if flags & SS_DISABLE != 0 {
            AltStack::DISABLED
        } else if size < MINSIGSTKSZ {
            return -crate::syscall::ENOMEM;
        } else {
            AltStack { sp, size, flags: flags & SS_AUTODISARM }
        })
    } else {
        None
    };
    if old != 0 {
        let Some(o) = (unsafe { user_slice_mut(old, 24) }) else { return -EFAULT };
        o.fill(0);
        o[0..8].copy_from_slice(&cur.sp.to_le_bytes());
        o[8..12].copy_from_slice(&cur.flags_at(rsp).to_le_bytes());
        o[16..24].copy_from_slice(&cur.size.to_le_bytes());
    }
    if let Some(a) = new_stack {
        p.altstack = a;
    }
    0
}

fn current_affinity() -> u64 {
    TABLE.lock().get(&current_pid()).map_or(u64::MAX, |p| p.affinity)
}

/// sched_getaffinity: a process's cores (0: the caller), or None if
/// there's no such process.
pub fn affinity(pid: u64) -> Option<u64> {
    let pid = if pid == 0 { current_pid() } else { pid };
    TABLE.lock().get(&pid).map(|p| p.affinity)
}

/// sched_setaffinity: the cores a process may run on (0: the caller). The
/// caller, if its core isn't one of them, moves before this returns (as
/// on Linux). False if there's no such process.
pub fn set_affinity(pid: u64, mask: u64) -> bool {
    let me = current_pid();
    let pid = if pid == 0 { me } else { pid };
    {
        let mut table = TABLE.lock();
        let Some(p) = table.get_mut(&pid) else { return false };
        p.affinity = mask;
    }
    if pid == me && mask & (1 << crate::smp::cpu_index()) == 0 {
        schedule();
    }
    true
}

pub fn with_current_mm<T>(f: impl FnOnce(&mut Mm) -> T) -> T {
    let mm = TABLE.lock().get(&current_pid()).expect("no current process").mm.clone();
    let mut guard = mm.lock();
    f(&mut guard)
}

/// (pid, ppid, name, state) of every process, for debugging and tests.
pub fn list() -> Vec<(u64, u64, String, State)> {
    TABLE.lock().values().map(|p| (p.pid, p.ppid, p.name.clone(), p.state)).collect()
}

/// Is any process other than the current one ready to run, or about to
/// be: a sleep that has ended, or a pending wake-up (e.g. a pipe got data)?
/// Those only become Ready when the scheduler runs, so the timer must call
/// it for them even while a busy process has the CPU.
pub fn others_ready() -> bool {
    let cur = current_pid();
    if !WAKE_QUEUE.lock().is_empty() || ptimer_due() {
        return true;
    }
    let now = ticks();
    let keys = new_keys();
    TABLE.lock().values().any(|p| {
        (p.alarm_at != 0 && now >= p.alarm_at)
            || (p.pid != cur && keys && matches!(p.state, State::Blocked(CONSOLE_KEY | POLL_KEY)))
            || (p.pid != cur
                && (p.state == State::Ready
                    || (p.state == State::Sleeping && now >= p.wake_at)
                    || (matches!(p.state, State::Blocked(_)) && p.deadline != 0 && now >= p.deadline)))
    })
}

/// Create a kernel stack for a new process whose first run "returns" to
/// ring 3 through `syscall_return` with the registers in `frame`.
/// Written at the bottom of every kernel stack; schedule() checks it, so an
/// overflow (which would silently overwrite the heap next to the stack) is
/// caught.
const STACK_CANARY: u64 = 0x57AC_C0DE_DEAD_BEEF;
const CANARY_WORDS: usize = 8;

fn new_kernel_stack(frame: &SyscallFrame) -> (Box<[u8]>, u64) {
    let mut stack = vec![0u8; KERNEL_STACK_SIZE].into_boxed_slice();
    for w in stack[..CANARY_WORDS * 8].chunks_mut(8) {
        w.copy_from_slice(&STACK_CANARY.to_le_bytes());
    }
    let top = (stack.as_ptr() as u64 + stack.len() as u64) & !0xf;
    let frame_size = core::mem::size_of::<SyscallFrame>() as u64;
    let frame_addr = top - frame_size;
    // Below the frame: what context_switch pops (r15, r14, r13, r12, rbx, rbp)
    // and then the return address.
    let rsp = frame_addr - 7 * 8;
    unsafe {
        (frame_addr as *mut SyscallFrame).write(*frame);
        let words = rsp as *mut u64;
        for i in 0..6 {
            words.add(i).write(0);
        }
        words.add(6).write(syscall_return_addr());
    }
    (stack, rsp)
}

fn add_process(process: Process) -> Result<u64, i64> {
    let mut table = TABLE.lock();
    if table.len() >= MAX_PROCESSES {
        return Err(EAGAIN);
    }
    let pid = process.pid;
    table.insert(pid, Box::new(process));
    Ok(pid)
}

fn basename(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

/// Switch to the next ready process (round-robin). If the current process
/// is still runnable and nothing else is ready, return immediately; if it
/// can't run (waiting or exited) and nothing is ready, halt until something is.
pub fn schedule() {
    {
        let table = TABLE.lock();
        if let Some(p) = table.get(&current_pid()) {
            if let Some(stack) = &p.kernel_stack {
                if stack[..CANARY_WORDS * 8].chunks(8).any(|w| u64::from_le_bytes(w.try_into().unwrap()) != STACK_CANARY) {
                    panic!("kernel stack overflow in pid {} ({})", p.pid, p.name);
                }
            }
        }
    }
    let cpu = crate::smp::cpu_index();
    let here = &crate::smp::PERCPU[cpu];
    let multi = crate::smp::cores() > 1;
    interrupts::without_interrupts(|| loop {
        let switch = {
            let mut table = TABLE.lock();
            let cur = current_pid();
            deliver_tty_signals(&mut table);
            fire_alarms_locked(&mut table);
            fire_posix_timers_locked(&mut table);
            let now = ticks();
            let mut woken = core::mem::take(&mut *WAKE_QUEUE.lock());
            let arrived = crate::task::keyboard::arrivals();
            if KEYS_SEEN.swap(arrived, Ordering::SeqCst) != arrived {
                woken.extend([CONSOLE_KEY, POLL_KEY]);
                if let Some(mut owners) = SIGIO_INPUT.try_lock() {
                    owners.retain(|&pid| signal_locked(&mut table, pid, SIGIO));
                }
            }
            for p in table.values_mut() {
                match p.state {
                    State::Sleeping if now >= p.wake_at => p.state = State::Ready,
                    State::Blocked(key) if woken.contains(&key) => p.state = State::Ready,
                    State::Blocked(_) if p.deadline != 0 && now >= p.deadline => p.state = State::Ready,
                    _ => {}
                }
            }
            // What this core may run: a ready process (threads of one
            // process too, on several cores at once: changed mappings are
            // shot down on the others, see smp::shootdown), allowed here,
            // and the kernel task only on core 0 (it runs on the boot stack).
            let may_run = |p: &Process| p.state == State::Ready && (p.pid != 0 || cpu == 0) && p.affinity & (1 << cpu) != 0;
            // Round-robin from after the current process, never the
            // current process itself (switching to itself would load its
            // stale saved stack pointer). From idle, after the last one
            // picked here, which may be picked again.
            let next = if cur == IDLE {
                let last = here.last.load(Ordering::Relaxed);
                table.range(last.saturating_add(1)..).chain(table.range(..=last)).find(|(_, p)| may_run(p))
            } else {
                table.range(cur + 1..).chain(table.range(..cur)).find(|(_, p)| may_run(p))
            }
            .map(|(&pid, _)| pid);

            match next {
                None => {
                    // Nothing else to run. Keep going if the current task can
                    // (it may have just been woken: a sleep ended, a signal).
                    if cur == IDLE {
                        return; // the idle loop waits for an interrupt
                    }
                    let p = table.get_mut(&cur).expect("current process missing");
                    if matches!(p.state, State::Running | State::Ready) && p.affinity & (1 << cpu) != 0 {
                        p.state = State::Running;
                        return;
                    }
                    if p.state == State::Running {
                        // Ready to run, but not here (its affinity): this
                        // core goes idle and leaves it to one it may use.
                        p.state = State::Ready;
                    }
                    // It can't run: this core goes idle (it waits there, not on
                    // this process's kernel stack, which another core may take
                    // the process up on once it's woken).
                    p.cpu_ticks += now.saturating_sub(p.running_since);
                    let save: *mut u64 = &mut p.kernel_rsp;
                    crate::fpu::save(&mut p.fpu);
                    let (_, flags) = Cr3::read();
                    unsafe { Cr3::write(memory::kernel_p4(), flags) };
                    here.p4.store(0, Ordering::SeqCst);
                    here.current.store(IDLE, Ordering::SeqCst);
                    here.tgid.store(0, Ordering::SeqCst);
                    Some((save, here.idle_rsp.load(Ordering::SeqCst)))
                }
                Some(next) => {
                    let save: *mut u64 = if cur == IDLE {
                        here.idle_rsp.as_ptr()
                    } else {
                        let current = table.get_mut(&cur).expect("current process missing");
                        if current.state == State::Running {
                            current.state = State::Ready;
                        }
                        current.cpu_ticks += now.saturating_sub(current.running_since);
                        crate::fpu::save(&mut current.fpu);
                        &mut current.kernel_rsp
                    };
                    here.last.store(next, Ordering::Relaxed);

                    let target = table.get_mut(&next).unwrap();
                    crate::fpu::restore(&target.fpu);
                    set_fs_gs(target.fs_base, target.gs_base);
                    target.state = State::Running;
                    target.running_since = now;
                    if let Some(top) = target.kernel_stack_top() {
                        crate::gdt::set_kernel_stack(VirtAddr::new(top));
                    }
                    // (With more cores, always: this core's cached mappings of
                    // this address space may be stale.)
                    let (active, flags) = Cr3::read();
                    if multi || active != target.p4 {
                        unsafe { Cr3::write(target.p4, flags) };
                    }
                    here.p4.store(target.p4.start_address().as_u64(), Ordering::SeqCst);
                    here.current.store(next, Ordering::SeqCst);
                    here.tgid.store(target.tgid, Ordering::SeqCst);
                    Some((save, target.kernel_rsp))
                }
            }
        };

        match switch {
            Some((save, new_rsp)) => {
                // The table lock is released: the next task may need it.
                unsafe { context_switch(save, new_rsp) };
                return;
            }
            None => return,
        }
    });
    // Nothing else to run here, and the caller goes on: on more cores, let
    // the kernel go for a moment. The caller may be waiting for a process
    // another core is running, which may be waiting to get into the kernel
    // (a loop around schedule() on one core would never let it in).
    if multi {
        breathe();
    }
}

/// Let other cores have the kernel for a moment (if they want it).
fn breathe() {
    if crate::smp::unlock_kernel_for_others() {
        for _ in 0..64 {
            core::hint::spin_loop();
        }
        crate::smp::lock_kernel();
    }
}

/// Let other ready processes run (sched_yield).
pub fn yield_now() {
    schedule();
    // Let pending interrupts in (keys, the timer, the network card): two
    // processes spinning on sched_yield would otherwise never take them.
    interrupts::enable();
    core::hint::spin_loop(); // (sti takes effect after one more instruction)
    interrupts::disable();
}

/// Called from the timer interrupt when it fires in ring 3.
pub fn preempt() {
    if others_ready() {
        schedule();
    }
}

/// End the current process. Its parent is woken if it's waiting; its
/// children are handed to the kernel (task 0), which reaps them.
pub fn exit_current(reason: ExitReason) -> ! {
    drop_files_of_current();
    {
        let mut table = TABLE.lock();
        let cur = current_pid();
        assert!(cur != 0, "the kernel task can't exit");
        clear_child_tid(&mut table, cur);
        let ppid = {
            let p = table.get_mut(&cur).unwrap();
            p.state = State::Zombie(reason);
            p.ppid
        };
        for p in table.values_mut() {
            if p.ppid == cur && p.pid != cur {
                p.ppid = 0;
            }
        }
        if let Some(parent) = table.get_mut(&ppid) {
            if parent.state == State::Waiting {
                parent.state = State::Ready;
            }
        }
        let info = child_info(&table, cur);
        signal_info_locked(&mut table, ppid, SIGCHLD, info);
        if table.get(&cur).is_some_and(|p| p.borrowed_vm) {
            wake(vfork_key(cur)); // a vfork child: the parent may go on
        }
        // The last thread of a process that isn't its first: the parent has
        // been told (it reaps the first thread); the kernel reaps this one.
        if let Some(p) = table.get_mut(&cur) {
            if p.pid != p.tgid {
                p.ppid = 0;
            }
        }
        // If an orphan already exited, the kernel may be waiting for it.
        if let Some(kernel) = table.get_mut(&0) {
            if kernel.state == State::Waiting {
                kernel.state = State::Ready;
            }
        }
    }
    schedule();
    unreachable!("a zombie was scheduled");
}

/// An exiting thread lets go of its files: everything closes (pipe ends,
/// terminals...) unless other threads still share them. Done before it's a
/// zombie (nobody reaps it meanwhile) and without the process table held:
/// closing can take it (a terminal's master hangs up its session).
fn drop_files_of_current() {
    let old = TABLE.lock().get_mut(&current_pid()).map(|p| core::mem::replace(&mut p.files, Arc::new(Mutex::new(Files::empty()))));
    drop(old);
}

/// Kill the current process (all its threads) with a signal (used by
/// fault handlers).
pub fn kill_current(signal: i32) -> ! {
    exit_group(ExitReason::Signaled(signal))
}

/// A thread is done: its CLONE_CHILD_CLEARTID word is zeroed and futex-woken
/// (pthread_join waits on it).
fn clear_child_tid(table: &mut BTreeMap<u64, Box<Process>>, tid: u64) {
    let Some(p) = table.get(&tid) else { return };
    let (addr, p4) = (p.clear_child_tid, p.p4);
    if addr == 0 {
        return;
    }
    if let Some(word) = unsafe { user_slice_mut(addr, 4) } {
        word.copy_from_slice(&0u32.to_le_bytes());
    }
    futex_wake_locked(table, futex_key(p4, addr), u32::MAX);
}

/// Other live threads of the current thread's process.
fn other_threads(table: &BTreeMap<u64, Box<Process>>, cur: u64) -> Vec<u64> {
    let tgid = table[&cur].tgid;
    table.values().filter(|t| t.tgid == tgid && t.pid != cur && !matches!(t.state, State::Zombie(_))).map(|t| t.pid).collect()
}

/// End another thread now (if it's running on another core, it stops at
/// that core's next trip into the kernel). Its kernel stack goes when it's
/// reaped (not while a core still runs it: see running_elsewhere): by the kernel, unless it's the process's
/// first thread, which its parent waits for.
/// Returns its files, for the caller to drop once the table is unlocked
/// (see drop_files_of_current).
#[must_use]
fn end_thread_locked(table: &mut BTreeMap<u64, Box<Process>>, tid: u64, reason: ExitReason) -> Option<Arc<Mutex<Files>>> {
    let t = table.get_mut(&tid)?;
    t.state = State::Zombie(reason);
    let files = core::mem::replace(&mut t.files, Arc::new(Mutex::new(Files::empty())));
    if t.pid != t.tgid {
        t.ppid = 0;
    }
    Some(files)
}

/// exit(): end the calling thread. The last thread of a process ends the
/// process; a process's first thread that exits early stays a zombie
/// until the others are done (that's what its parent waits for).
pub fn exit_thread(code: i32) -> ! {
    drop_files_of_current();
    let alone = {
        let mut table = TABLE.lock();
        let cur = current_pid();
        clear_child_tid(&mut table, cur);
        let others = other_threads(&table, cur);
        if !others.is_empty() && table[&cur].pid != table[&cur].tgid {
            // Not the first thread, and not the last: just this thread.
            let _ = end_thread_locked(&mut table, cur, ExitReason::Exited(code)); // (already dropped)
            wake_kernel_locked(&mut table);
            false
        } else {
            true
        }
    };
    if alone {
        drop_posix_timers(current_tgid());
        exit_current(ExitReason::Exited(code));
    }
    schedule();
    unreachable!("an exited thread was scheduled");
}

/// exit_group(), a fatal signal, a fault: end every thread of the process.
pub fn exit_group(reason: ExitReason) -> ! {
    // A program that had the screen (an X server) gives it back.
    crate::vga_buffer::process_exited(current_tgid());
    drop_posix_timers(current_tgid());
    drop_files_of_current();
    let (leader_alive_elsewhere, their_files) = {
        let mut table = TABLE.lock();
        let cur = current_pid();
        let tgid = table[&cur].tgid;
        let mut files = Vec::new();
        for t in other_threads(&table, cur) {
            files.extend(end_thread_locked(&mut table, t, reason));
        }
        (tgid != cur, files)
    };
    drop(their_files); // (the table is unlocked)
    if leader_alive_elsewhere {
        // A thread other than the first ends the process: the first thread
        // (ended above, a zombie now) is what the parent sees exit.
        let mut table = TABLE.lock();
        let cur = current_pid();
        let tgid = table[&cur].tgid;
        clear_child_tid(&mut table, cur);
        let _ = end_thread_locked(&mut table, cur, reason); // (already dropped)
        notify_exit_locked(&mut table, tgid);
        drop(table);
        schedule();
        unreachable!("an exited thread was scheduled");
    }
    exit_current(reason)
}

fn wake_kernel_locked(table: &mut BTreeMap<u64, Box<Process>>) {
    if let Some(kernel) = table.get_mut(&0) {
        if kernel.state == State::Waiting {
            kernel.state = State::Ready;
        }
    }
}

/// A process ended: wake its parent (if it waits) and send it SIGCHLD.
fn notify_exit_locked(table: &mut BTreeMap<u64, Box<Process>>, pid: u64) {
    let ppid = table.get(&pid).map_or(0, |p| p.ppid);
    if let Some(parent) = table.get_mut(&ppid) {
        if parent.state == State::Waiting {
            parent.state = State::Ready;
        }
    }
    let info = child_info(table, pid);
    signal_info_locked(table, ppid, SIGCHLD, info);
    wake_kernel_locked(table);
}

/// SIGCHLD's siginfo for a child that exited, was killed, stopped or
/// continued.
fn child_info(table: &BTreeMap<u64, Box<Process>>, child: u64) -> SigInfo {
    let Some(c) = table.get(&child) else { return SigInfo::KERNEL };
    let (code, status) = match (c.state, c.wait_event) {
        (State::Zombie(ExitReason::Exited(code)), _) => (CLD_EXITED, code),
        (State::Zombie(ExitReason::Signaled(sig)), _) => (CLD_KILLED, sig),
        (_, Some(0xffff)) => (CLD_CONTINUED, SIGCONT),
        (_, Some(ev)) if ev & 0xff == 0x7f => (CLD_STOPPED, (ev >> 8) as i32),
        _ => (SI_KERNEL, 0),
    };
    SigInfo { code, pid: c.tgid as i32, uid: c.cred.ruid, word: 0, status }
}

/// Is `pid` still on another core? (A process ended by another core keeps
/// running there until that core's next trip into the kernel: its kernel
/// stack and address space must stay until then.)
fn running_elsewhere(pid: u64) -> bool {
    let me = crate::smp::cpu_index();
    (0..crate::smp::MAX_CPUS).any(|c| c != me && crate::smp::PERCPU[c].current.load(Ordering::SeqCst) == pid)
}

/// Free a reaped process's memory. Its kernel stack is freed when it drops.
fn free_process(p: Box<Process>) {
    // Threads share the address space: the last one to go frees it.
    if p.kernel_stack.is_some() && !p.borrowed_vm && Arc::strong_count(&p.mm) == 1 {
        let mut alloc = FRAME_ALLOCATOR.lock();
        memory::free_address_space(p.p4, alloc.as_mut().unwrap());
    }
}

/// Wait for a child of the current process. `pid` is a specific child or -1
/// for any. Returns Ok(None) if `nohang` and no child has exited yet.
pub fn wait(pid: i64, nohang: bool) -> Result<Option<(u64, ExitReason)>, i64> {
    loop {
        if !nohang && signal_pending() {
            return Err(EINTR);
        }
        let reaped = {
            let mut table = TABLE.lock();
            let cur = current_pid();
            let matches = |p: &Process| p.ppid == cur && p.pid != cur && (pid == -1 || p.pid as i64 == pid);
            if !table.values().any(|p| matches(p)) {
                return Err(ECHILD);
            }
            let zombie = table
                .values()
                .find(|p| matches(p) && matches!(p.state, State::Zombie(_)) && !running_elsewhere(p.pid))
                .map(|p| p.pid);
            // (One still leaving another core: soon. Try again.)
            let leaving = zombie.is_none()
                && table.values().any(|p| matches(p) && matches!(p.state, State::Zombie(_)));
            match zombie {
                Some(zpid) => table.remove(&zpid),
                None if leaving => {
                    drop(table);
                    schedule();
                    continue;
                }
                None if nohang => return Ok(None),
                None if loud_pending_locked(&table, cur) => return Err(EINTR),
                None => {
                    table.get_mut(&cur).unwrap().state = State::Waiting;
                    None
                }
            }
        };
        match reaped {
            Some(p) => {
                let State::Zombie(reason) = p.state else { unreachable!() };
                let pid = p.pid;
                free_process(p);
                return Ok(Some((pid, reason)));
            }
            None => schedule(),
        }
    }
}

// ---- syscalls ----

/// fork(): duplicate the current process. Returns the child's pid to the
/// parent; the child returns from the same syscall with 0.
pub fn fork(frame: &SyscallFrame) -> i64 {
    fork_with(frame, CloneKind::Fork, 0)
}

/// Key a vfork parent waits on until its child stops using its memory.
fn vfork_key(child: u64) -> u64 {
    0xF0F0_0000_0000_0000 | child
}

/// What a new task shares with its creator.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CloneKind {
    /// A new process with a copy-on-write copy of the memory.
    Fork,
    /// A new process borrowing the parent's memory until it execs or exits.
    Vfork,
    /// A new thread of the same process: memory, files, signal handlers.
    Thread { tls: Option<u64>, clear_tid: u64 },
}

/// clone(flags, stack, parent_tid, child_tid, tls):
/// - fork (just an exit signal),
/// - vfork (CLONE_VM | CLONE_VFORK, what posix_spawn uses): the child runs
///   in the parent's memory, on `stack`, and the parent waits until it
///   calls execve or exits,
/// - a thread (CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND |
///   CLONE_THREAD, as pthread_create does), with CLONE_SETTLS (its TLS),
///   CLONE_PARENT_SETTID / CLONE_CHILD_SETTID (write its tid) and
///   CLONE_CHILD_CLEARTID (zero and futex-wake that word when it exits).
pub fn sys_clone(frame: &SyscallFrame, flags: u64, stack: u64, parent_tid: u64, child_tid: u64, tls: u64) -> i64 {
    const CLONE_VM: u64 = 0x100;
    const CLONE_FS: u64 = 0x200;
    const CLONE_FILES: u64 = 0x400;
    const CLONE_SIGHAND: u64 = 0x800;
    const CLONE_VFORK: u64 = 0x4000;
    const CLONE_THREAD: u64 = 0x10000;
    const CLONE_SYSVSEM: u64 = 0x40000;
    const CLONE_SETTLS: u64 = 0x80000;
    const CLONE_PARENT_SETTID: u64 = 0x100000;
    const CLONE_CHILD_CLEARTID: u64 = 0x200000;
    const CLONE_DETACHED: u64 = 0x400000;
    const CLONE_CHILD_SETTID: u64 = 0x1000000;
    const THREAD: u64 = CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND | CLONE_THREAD;
    let exit_signal = flags & 0xff;
    let known = CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND | CLONE_VFORK | CLONE_THREAD | CLONE_SYSVSEM
        | CLONE_SETTLS | CLONE_PARENT_SETTID | CLONE_CHILD_CLEARTID | CLONE_DETACHED | CLONE_CHILD_SETTID;
    if flags & !0xff & !known != 0 || (exit_signal != 0 && exit_signal != SIGCHLD as u64) {
        return -EINVAL;
    }
    let kind = if flags & CLONE_THREAD != 0 {
        // A thread shares everything a pthread does (nothing in between).
        if flags & THREAD != THREAD {
            return -EINVAL;
        }
        CloneKind::Thread {
            tls: (flags & CLONE_SETTLS != 0).then_some(tls),
            clear_tid: if flags & CLONE_CHILD_CLEARTID != 0 { child_tid } else { 0 },
        }
    } else if flags & CLONE_VM != 0 {
        if flags & CLONE_VFORK == 0 {
            return -EINVAL; // shared memory without a thread: not supported
        }
        CloneKind::Vfork
    } else {
        CloneKind::Fork
    };
    let pid = fork_with(frame, kind, stack);
    if pid <= 0 {
        return pid;
    }
    // Tell the creator (and, for a thread, the thread itself) its tid.
    // (The memory is shared for threads, so both writes land for both.)
    for (flag, ptr) in [(CLONE_PARENT_SETTID, parent_tid), (CLONE_CHILD_SETTID, child_tid)] {
        if flags & flag != 0 {
            if let Some(out) = unsafe { user_slice_mut(ptr, 4) } {
                out.copy_from_slice(&(pid as u32).to_le_bytes());
            }
        }
    }
    if kind == CloneKind::Vfork {
        // Wait (uninterruptibly: the child is using our memory) until the
        // child has its own address space or is gone.
        loop {
            {
                let mut table = TABLE.lock();
                if !table.get(&(pid as u64)).is_some_and(|c| c.borrowed_vm && !matches!(c.state, State::Zombie(_))) {
                    break;
                }
                table.get_mut(&current_pid()).unwrap().state = State::Blocked(vfork_key(pid as u64));
            }
            schedule();
        }
    }
    pid
}

/// Make a new task (see CloneKind), with the child on `stack` if it's not 0.
fn fork_with(frame: &SyscallFrame, kind: CloneKind, stack: u64) -> i64 {
    let share = kind != CloneKind::Fork; // the same address space
    let thread = matches!(kind, CloneKind::Thread { .. });
    // The child gets the FPU registers as they are now.
    let mut fpu = Box::new(crate::fpu::FpuState::initial());
    crate::fpu::save(&mut fpu);
    let (p4, files, name, ppid, mm, (pgid, sid, ctty), (actions, blocked), argv, exe, cred, (fs_base, gs_base), tgid) = {
        let table = TABLE.lock();
        if table.len() >= MAX_PROCESSES {
            return -EAGAIN;
        }
        let cur = table.get(&current_pid()).unwrap();
        (
            cur.p4,
            // A thread shares the files and memory bookkeeping; a new
            // process gets copies.
            if thread { cur.files.clone() } else { Arc::new(Mutex::new(cur.files.lock().clone())) },
            cur.name.clone(),
            // A thread's parent is its process's parent.
            if thread { cur.ppid } else { cur.pid },
            if thread { cur.mm.clone() } else { Arc::new(Mutex::new(cur.mm.lock().clone())) },
            (cur.pgid, cur.sid, cur.ctty),
            (cur.actions.clone(), cur.blocked),
            cur.argv.clone(),
            cur.exe.clone(),
            cur.cred.clone(),
            (cur.fs_base, cur.gs_base),
            if thread { Some(cur.tgid) } else { None },
        )
    };

    let child_p4 = if share {
        p4
    } else {
        let mut alloc = FRAME_ALLOCATOR.lock();
        match memory::copy_address_space(p4, alloc.as_mut().unwrap()) {
            Ok(p4) => p4,
            Err(_) => return -ENOMEM,
        }
    };

    let mut child_frame = *frame;
    child_frame.rax = 0; // fork returns 0 in the child
    if stack != 0 {
        child_frame.rsp = stack;
    }
    let (kernel_stack, kernel_rsp) = new_kernel_stack(&child_frame);
    let pid = NEXT_PID.fetch_add(1, Ordering::SeqCst);
    let child = Process {
        pid,
        ppid,
        name,
        state: State::Ready,
        kernel_rsp,
        kernel_stack: Some(kernel_stack),
        p4: child_p4,
        files,
        mm,
        pgid,
        sid,
        ctty,
        pending: 0,
        siginfo: BTreeMap::new(),
        altstack: if thread { AltStack::DISABLED } else { current_altstack() }, // (Linux: not for threads)
        sigwait: 0,
        actions, // handlers and the mask are inherited; pending signals and alarms aren't
        blocked,
        saved_mask: None,
        alarm_at: 0,
        alarm_every: 0,
        wait_event: None,
        wake_at: 0,
        argv,
        exe,
        start_tick: ticks(),
        affinity: current_affinity(), // (inherited, as on Linux)
        cpu_ticks: 0,
        running_since: 0,
        cred,
        fpu,
        fs_base: match kind {
            CloneKind::Thread { tls: Some(tls), .. } => tls,
            _ => fs_base,
        },
        gs_base,
        tgid: tgid.unwrap_or(pid),
        clear_child_tid: match kind {
            CloneKind::Thread { clear_tid, .. } => clear_tid,
            _ => 0,
        },
        deadline: 0,
        borrowed_vm: kind == CloneKind::Vfork,
    };
    match add_process(child) {
        Ok(pid) => pid as i64,
        Err(e) => {
            if !share {
                let mut alloc = FRAME_ALLOCATOR.lock();
                memory::free_address_space(child_p4, alloc.as_mut().unwrap());
            }
            -e
        }
    }
}

/// Read a NULL-terminated array of user string pointers (argv/envp).
fn user_string_array(ptr: u64) -> Result<Vec<String>, i64> {
    let mut out = Vec::new();
    if ptr == 0 {
        return Ok(out); // Linux treats a NULL argv/envp as empty
    }
    let mut total = 0;
    for i in 0.. {
        if i > usermode::MAX_ARGS {
            return Err(E2BIG);
        }
        let slot = ptr.checked_add(i as u64 * 8).ok_or(EFAULT)?;
        let Some(bytes) = (unsafe { user_slice(slot, 8) }) else {
            return Err(EFAULT);
        };
        let p = u64::from_le_bytes(bytes.try_into().unwrap());
        if p == 0 {
            break;
        }
        let s = vfs::user_string(p)?;
        total += s.len() + 1;
        if total > usermode::MAX_ARG_BYTES {
            return Err(E2BIG);
        }
        out.push(s);
    }
    Ok(out)
}

fn load_error_errno(e: &str) -> i64 {
    match e {
        ERR_TOO_MANY_ARGS => E2BIG,
        ERR_NO_MEMORY => ENOMEM,
        usermode::ERR_NO_INTERP => ENOENT,
        _ => ENOEXEC,
    }
}

/// Read an executable for execve. A `#!` script runs its interpreter
/// instead: `#!/bin/sh -x` in /s.sh with argv [s.sh, a] becomes
/// /bin/sh with argv [/bin/sh, -x, /s.sh, a]. Returns (program path,
/// ELF bytes, argv).
fn load_executable(path: &str, argv: Vec<String>) -> Result<(String, usermode::Program<'static>, Vec<String>, Option<crate::mounts::Meta>), i64> {
    let cred = current_cred();
    // The file (read piece by piece when loaded) and its first bytes.
    let check = |path: &str| -> Result<(usermode::Program<'static>, Vec<u8>, crate::mounts::Meta), i64> {
        let abs = crate::vfs::absolute(path);
        let (_, meta) = crate::mounts::stat(&abs)?;
        if meta.kind == crate::mounts::FileType::Dir {
            return Err(EACCES);
        }
        crate::perm::check(&meta, &cred, crate::perm::EXEC)?; // not executable (for us)
        let file = usermode::Program::open(&abs)?;
        let head = file.head(256).map_err(|_| crate::syscall::EIO)?;
        Ok((file, head, meta))
    };
    let (file, bytes, meta) = check(path)?;
    if crate::elf::is_elf(&bytes) {
        return Ok((path.to_string(), file, argv, Some(meta)));
    }
    if !bytes.starts_with(b"#!") {
        // Not an executable format: a text file must not be run as raw code.
        // (Flat binaries can only be started by the kernel shell, for tests.)
        return Err(ENOEXEC);
    }
    let line_end = bytes.iter().position(|&b| b == b'\n').unwrap_or(bytes.len()).min(256);
    let line = core::str::from_utf8(&bytes[2..line_end]).map_err(|_| ENOEXEC)?.trim();
    let (interp, arg) = match line.split_once(|c: char| c == ' ' || c == '\t') {
        Some((i, a)) => (i, Some(a.trim())),
        None => (line, None),
    };
    if interp.is_empty() {
        return Err(ENOEXEC);
    }
    let (interp_file, interp_head, _) = check(interp)?;
    if !crate::elf::is_elf(&interp_head) {
        return Err(ENOEXEC); // an interpreter that's a script itself isn't supported
    }
    let mut new_argv = vec![interp.to_string()];
    if let Some(a) = arg.filter(|a| !a.is_empty()) {
        new_argv.push(a.to_string());
    }
    new_argv.push(path.to_string());
    new_argv.extend(argv.into_iter().skip(1));
    // (setuid/setgid bits on scripts are ignored, as on Linux.)
    Ok((interp.to_string(), interp_file, new_argv, None))
}

/// execve(path, argv, envp): replace the current program. On success it
/// doesn't return: the syscall "returns" into the new program's entry point.
pub fn execve(frame: &mut SyscallFrame, path: u64, argv: u64, envp: u64) -> i64 {
    // In a process with threads, only its first thread may exec (the new
    // program takes over the process; the other threads end).
    {
        let table = TABLE.lock();
        let cur = current_pid();
        if table[&cur].pid != table[&cur].tgid && !other_threads(&table, cur).is_empty() {
            return -EBUSY;
        }
    }
    let path = match vfs::user_string(path) {
        Ok(p) if p.is_empty() => return -ENOENT,
        Ok(p) => p,
        Err(e) => return -e,
    };
    let (argv, envp) = match (user_string_array(argv), user_string_array(envp)) {
        (Ok(a), Ok(e)) => (a, e),
        (Err(e), _) | (_, Err(e)) => return -e,
    };
    let (path, bytes, argv, exe_meta) = match load_executable(&path, argv) {
        Ok(found) => found,
        Err(e) => return -e,
    };

    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let envp_refs: Vec<&str> = envp.iter().map(String::as_str).collect();
    // A setuid/setgid program runs as the file's owner/group.
    let setuid = exe_meta.as_ref().filter(|m| m.perm & crate::perm::S_ISUID != 0).map(|m| m.uid);
    let setgid = exe_meta.as_ref().filter(|m| m.perm & crate::perm::S_ISGID != 0).map(|m| m.gid);
    let c = current_cred();
    let ids = usermode::Ids { uid: c.ruid, euid: setuid.unwrap_or(c.euid), gid: c.rgid, egid: setgid.unwrap_or(c.egid) };
    let image = match usermode::build_image(&bytes, &argv_refs, &envp_refs, ids) {
        Ok(image) => image,
        Err(e) => return -load_error_errno(e),
    };

    // Point of no return: switch to the new address space, free the old one.
    // A new program has no timers.
    drop_posix_timers(current_tgid());
    // (Files closed here are dropped after the table is unlocked: see
    // drop_files_of_current.)
    let mut closed_files = Vec::new();
    let closed_fds;
    let old_p4 = {
        let mut table = TABLE.lock();
        let cur = current_pid();
        // The other threads end; they keep the old address space until
        // they're reaped (the last one frees it).
        for t in other_threads(&table, cur) {
            closed_files.extend(end_thread_locked(&mut table, t, ExitReason::Signaled(SIGKILL)));
        }
        wake_kernel_locked(&mut table);
        let p = table.get_mut(&cur).unwrap();
        let old_shared = Arc::strong_count(&p.mm) > 1;
        closed_fds = Some(p.files.lock().close_on_exec());
        p.name = basename(&path);
        p.argv = argv.clone();
        p.exe = crate::filesystem::normalize(&p.files.lock().cwd, &path);
        p.mm = Mm::new(); // the new program starts with an empty heap and no mmaps
        p.fs_base = 0;
        p.gs_base = 0;
        p.altstack = AltStack::DISABLED; // (the old program's memory is gone)
        let borrowed = core::mem::replace(&mut p.borrowed_vm, false);
        // Handlers are gone with the old program: back to the default action
        // (ignored signals stay ignored; the mask and pending signals stay).
        for a in p.actions.iter_mut() {
            if a.handler != SIG_IGN {
                *a = SigAction::DEFAULT;
            }
        }
        (core::mem::replace(&mut p.p4, image.p4), borrowed, old_shared)
    };
    let (old_p4, borrowed, old_shared) = old_p4;
    drop(closed_files);
    drop(closed_fds);
    apply_exec_ids(setuid, setgid);
    let (_, flags) = Cr3::read();
    unsafe { Cr3::write(image.p4, flags) };
    // This core is in the new address space now: say so (shootdowns go to
    // the cores in an address space, by this; the old one's frame is freed
    // below and may become another process's).
    crate::smp::PERCPU[crate::smp::cpu_index()].p4.store(image.p4.start_address().as_u64(), Ordering::SeqCst);
    crate::fpu::reset();
    set_fs_gs(0, 0);
    if borrowed {
        // A vfork child: the old address space is the parent's. Let it go on.
        wake(vfork_key(current_pid()));
    } else if !old_shared {
        let mut alloc = FRAME_ALLOCATOR.lock();
        memory::free_address_space(old_p4, alloc.as_mut().unwrap());
    }

    *frame = SyscallFrame::new_user(image.entry, image.stack_pointer);
    0 // becomes rax at the new program's entry point
}

/// wait4(pid, wstatus, options, rusage): pid > 0 is one child, -1 any,
/// 0 any in our process group, < -1 any in group -pid. Options: WNOHANG,
/// WUNTRACED (also report stopped children), WCONTINUED (and continued).
pub fn sys_wait4(pid: u64, status_ptr: u64, options: u64, rusage: u64) -> i64 {
    const WNOHANG: u64 = 1;
    const WUNTRACED: u64 = 2;
    const WCONTINUED: u64 = 8;
    let status_out = if status_ptr != 0 {
        match unsafe { user_slice_mut(status_ptr, 4) } {
            Some(s) => Some(s),
            None => return -EFAULT,
        }
    } else {
        None
    };
    let wait = WaitFor {
        exited: true,
        stopped: options & WUNTRACED != 0,
        continued: options & WCONTINUED != 0,
        nohang: options & WNOHANG != 0,
        nowait: false,
    };
    match wait_core(pid as i64 as i32 as i64, wait, rusage) {
        Ok(Some((pid, status, _))) => {
            if let Some(out) = status_out {
                out.copy_from_slice(&status.to_le_bytes());
            }
            pid as i64
        }
        Ok(None) => 0,
        Err(e) => -e,
    }
}

/// What wait4 and waitid report: exits, stops, continues; and whether to
/// return at once if nothing is there (WNOHANG), and to leave the child
/// waitable (WNOWAIT).
#[derive(Clone, Copy)]
struct WaitFor {
    exited: bool,
    stopped: bool,
    continued: bool,
    nohang: bool,
    nowait: bool,
}

/// waitid(idtype, id, info, options, rusage): wait4 with a siginfo (how
/// the child changed, its pid, uid and status), and the WEXITED, WSTOPPED,
/// WCONTINUED, WNOHANG and WNOWAIT options.
pub fn sys_waitid(idtype: u64, id: u64, info: u64, options: u64, rusage: u64) -> i64 {
    const WNOHANG: u64 = 1;
    const WSTOPPED: u64 = 2;
    const WEXITED: u64 = 4;
    const WCONTINUED: u64 = 8;
    const WNOWAIT: u64 = 0x0100_0000;
    const P_ALL: u64 = 0;
    const P_PID: u64 = 1;
    const P_PGID: u64 = 2;
    if options & !(WNOHANG | WSTOPPED | WEXITED | WCONTINUED | WNOWAIT | 0x6000_0000) != 0
        || options & (WEXITED | WSTOPPED | WCONTINUED) == 0
    {
        return -EINVAL;
    }
    // As wait4's pid argument.
    let sel = match idtype {
        P_ALL => -1,
        P_PID if id as i64 > 0 => id as i64,
        P_PGID if id == 0 => 0,
        P_PGID if id as i64 > 0 => -(id as i64),
        _ => return -EINVAL, // (P_PIDFD: no pidfds)
    };
    let wait = WaitFor {
        exited: options & WEXITED != 0,
        stopped: options & WSTOPPED != 0,
        continued: options & WCONTINUED != 0,
        nohang: options & WNOHANG != 0,
        nowait: options & WNOWAIT != 0,
    };
    let found = match wait_core(sel, wait, rusage) {
        Ok(f) => f,
        Err(e) => return -e,
    };
    if info != 0 {
        let Some(out) = (unsafe { user_slice_mut(info, 128) }) else { return -EFAULT };
        out.fill(0);
        if let Some((pid, status, uid)) = found {
            // si_code: CLD_EXITED 1, KILLED 2, DUMPED 3, STOPPED 5, CONTINUED 6.
            let (code, value) = if status == 0xffff {
                (6, SIGCONT as u32)
            } else if status & 0xff == 0x7f {
                (5, (status >> 8) & 0xff)
            } else if status & 0x7f == 0 {
                (1, (status >> 8) & 0xff)
            } else if status & 0x80 != 0 {
                (3, status & 0x7f)
            } else {
                (2, status & 0x7f)
            };
            out[0..4].copy_from_slice(&(SIGCHLD as u32).to_le_bytes());
            out[8..12].copy_from_slice(&(code as u32).to_le_bytes());
            out[16..20].copy_from_slice(&(pid as u32).to_le_bytes());
            out[20..24].copy_from_slice(&uid.to_le_bytes());
            out[24..28].copy_from_slice(&value.to_le_bytes());
        }
    }
    0
}

/// Wait for a child matching `pid` (wait4's meaning) to change as `wait`
/// asks: (pid, wait status, uid), or None for WNOHANG with nothing there.
fn wait_core(pid: i64, wait: WaitFor, rusage: u64) -> Result<Option<(u64, u32, u32)>, i64> {
    loop {
        if !wait.nohang && signal_pending() {
            return Err(EINTR);
        }
        let found = {
            let mut table = TABLE.lock();
            let cur = current_pid();
            let my_pgid = table.get(&cur).map(|p| p.pgid).unwrap_or(0);
            // Only whole processes (their first thread), never other threads.
            let matches = |p: &Process| {
                p.ppid == cur
                    && p.pid == p.tgid
                    && p.pid != cur
                    && match pid {
                        -1 => true,
                        0 => p.pgid == my_pgid,
                        n if n < -1 => p.pgid == (-n) as u64,
                        n => p.pid as i64 == n,
                    }
            };
            if !table.values().any(|p| matches(p)) {
                return Err(ECHILD);
            }
            // A process is done when its first thread is a zombie and no
            // other thread of it is still running.
            let done = |p: &Process| {
                matches!(p.state, State::Zombie(_))
                    && !running_elsewhere(p.pid)
                    && !table.values().any(|t| t.tgid == p.pid && t.pid != p.pid && !matches!(t.state, State::Zombie(_)))
            };
            let zombie = if wait.exited { table.values().find(|p| matches(p) && done(p)).map(|p| p.pid) } else { None };
            if let Some(zpid) = zombie {
                if wait.nowait {
                    // Reported, but left to be waited for again.
                    let p = &table[&zpid];
                    let State::Zombie(reason) = p.state else { unreachable!() };
                    Some((zpid, reason.wait_status() as u32, p.cred.ruid))
                } else {
                    let p = table.remove(&zpid).unwrap();
                    let State::Zombie(reason) = p.state else { unreachable!() };
                    let (cpu, uid) = (p.cpu_ticks, p.cred.ruid);
                    drop(table);
                    free_process(p);
                    if rusage != 0 {
                        fill_rusage(rusage, cpu);
                    }
                    Some((zpid, reason.wait_status() as u32, uid))
                }
            } else {
                // A stop (status 0x7f) or continue (0xffff) not yet reported?
                let wanted = |e: u32| (e & 0xff == 0x7f && wait.stopped) || (e == 0xffff && wait.continued);
                let event = table.values().find(|p| matches(p) && p.wait_event.is_some_and(wanted)).map(|p| p.pid);
                match event {
                    Some(epid) => {
                        let p = table.get_mut(&epid).unwrap();
                        let uid = p.cred.ruid;
                        let e = if wait.nowait { p.wait_event.unwrap() } else { p.wait_event.take().unwrap() };
                        Some((epid, e, uid))
                    }
                    // (A child that's done but still leaving another core:
                    // soon. Look again without waiting.)
                    None if wait.exited
                        && table.values().any(|p| matches(p) && matches!(p.state, State::Zombie(_)) && running_elsewhere(p.pid)) =>
                    {
                        None
                    }
                    None if wait.nohang => return Ok(None),
                    None if loud_pending_locked(&table, cur) => return Err(EINTR),
                    None => {
                        table.get_mut(&cur).unwrap().state = State::Waiting;
                        None
                    }
                }
            }
        };
        match found {
            Some(f) => return Ok(Some(f)),
            None => schedule(),
        }
    }
}

// ---- used by the kernel shell ----

/// The kernel starts programs by name ("sh"): their path is /bin/NAME.
fn program_path(name: &str) -> String {
    if name.starts_with('/') {
        name.to_string()
    } else {
        alloc::format!("/bin/{}", name)
    }
}

/// Start a program as a child of the kernel, with the kernel shell's
/// working directory. Returns its pid.
pub fn spawn(bytes: &[u8], argv: &[&str], envp: &[&str]) -> Result<u64, &'static str> {
    spawn_as(bytes, argv, envp, current_pid(), CTTY_CONSOLE)
}

/// Start a program as the kernel's child (whatever is running now), as root,
/// in a session of its own with no controlling terminal: the first terminal
/// it opens becomes it (a login on another terminal: virtio-console's).
pub fn spawn_session(bytes: &[u8], argv: &[&str], envp: &[&str]) -> Result<u64, &'static str> {
    let pid = spawn_as(bytes, argv, envp, 0, CTTY_NONE)?;
    let mut s = OTHER_SESSIONS.lock();
    s.retain(|&sid| alive(sid)); // (sessions whose leader is gone: forgotten)
    s.push(pid); // (it leads its session: sid = pid)
    Ok(pid)
}

/// Sessions the kernel started on other terminals (spawn_session): the
/// console's logout leaves them be.
static OTHER_SESSIONS: Mutex<Vec<u64>> = Mutex::new(Vec::new());

/// Whether `p` belongs to a session on another terminal.
fn on_other_terminal(p: &Process, others: &[u64]) -> bool {
    others.contains(&p.sid)
}

/// Whether process `pid` is there and not a zombie.
pub fn alive(pid: u64) -> bool {
    TABLE.lock().get(&pid).is_some_and(|p| !matches!(p.state, State::Zombie(_)))
}

fn spawn_as(bytes: &[u8], argv: &[&str], envp: &[&str], ppid: u64, ctty: u32) -> Result<u64, &'static str> {
    let c = if ctty == CTTY_NONE { Cred::ROOT } else { current_cred() };
    let ids = usermode::Ids { uid: c.ruid, euid: c.euid, gid: c.rgid, egid: c.egid };
    let image = usermode::build_image(&usermode::Program::Bytes(bytes), argv, envp, ids)?;
    let frame = SyscallFrame::new_user(image.entry, image.stack_pointer);
    let (kernel_stack, kernel_rsp) = new_kernel_stack(&frame);
    let pid = NEXT_PID.fetch_add(1, Ordering::SeqCst);
    let cwd = crate::filesystem::CWD.lock().clone();
    let name = argv.first().map(|a| basename(a)).unwrap_or_else(|| "program".to_string());
    let process = Process {
        pid,
        ppid,
        name,
        state: State::Ready,
        kernel_rsp,
        kernel_stack: Some(kernel_stack),
        p4: image.p4,
        files: Arc::new(Mutex::new(Files::new(&cwd))),
        mm: Mm::new(),
        pgid: pid, // leader of a new process group
        sid: pid,  // and session, on the console
        ctty,
        pending: 0,
        siginfo: BTreeMap::new(),
        altstack: AltStack::DISABLED,
        sigwait: 0,
        actions: Box::new([SigAction::DEFAULT; MAX_SIGNAL as usize]),
        blocked: 0,
        saved_mask: None,
        alarm_at: 0,
        alarm_every: 0,
        wait_event: None,
        wake_at: 0,
        argv: argv.iter().map(|a| a.to_string()).collect(),
        exe: program_path(argv.first().copied().unwrap_or("")),
        start_tick: ticks(),
        affinity: u64::MAX,
        cpu_ticks: 0,
        running_since: 0,
        cred: c, // programs the kernel starts run as root
        fpu: Box::new(crate::fpu::FpuState::initial()),
        fs_base: 0,
        gs_base: 0,
        tgid: pid,
        clear_child_tid: 0,
        deadline: 0,
        borrowed_vm: false,
    };
    add_process(process).map_err(|_| {
        let mut alloc = FRAME_ALLOCATOR.lock();
        memory::free_address_space(image.p4, alloc.as_mut().unwrap());
        "too many processes"
    })
}

/// Run a program and wait for it and everything it started. Returns the
/// shell-style status (exit code, or 128 + signal).
pub fn run_program(bytes: &[u8], argv: &[&str]) -> Result<i32, &'static str> {
    run_program_env(bytes, argv, DEFAULT_ENV)
}

pub fn run_program_env(bytes: &[u8], argv: &[&str], envp: &[&str]) -> Result<i32, &'static str> {
    assert_eq!(current_pid(), 0, "run_program must be called by the kernel");
    crate::tty::new_session();
    let pid = spawn(bytes, argv, envp)?;
    // The program's group gets the keyboard (Ctrl-C goes to it).
    FOREGROUND.store(pid, Ordering::SeqCst);

    let mut status = None;
    // Reap the program, then any orphans it left behind (they were handed
    // to the kernel), until none on the console remain. (Sessions on other
    // terminals, like virtio-console's login, are the kernel's children
    // too: they go on, and are reaped here when they end.)
    loop {
        if status.is_some() {
            let others = OTHER_SESSIONS.lock().clone();
            let table = TABLE.lock();
            if !table.values().any(|p| p.ppid == 0 && p.pid != 0 && !on_other_terminal(p, &others)) {
                break;
            }
        }
        match wait(-1, false) {
            Ok(Some((p, reason))) if p == pid => {
                status = Some(reason);
                // Like a terminal hanging up: whatever it left running gets
                // SIGHUP, so e.g. `sleep 100 &` can't keep the console busy.
                // (Not the sessions on other terminals: virtio-console's.)
                let others = OTHER_SESSIONS.lock().clone();
                let mut table = TABLE.lock();
                let left: Vec<u64> = table.values().filter(|q| q.pid != 0 && !on_other_terminal(q, &others)).map(|q| q.pid).collect();
                for p in left {
                    signal_locked(&mut table, p, SIGHUP);
                }
            }
            Ok(_) => {}
            Err(_) => break, // ECHILD: no children left
        }
    }
    FOREGROUND.store(0, Ordering::SeqCst);
    // A program may leave the console in raw mode or with odd colors.
    crate::tty::reset_mode();
    x86_64::instructions::interrupts::without_interrupts(|| crate::vga_buffer::WRITER.lock().reset_ansi());
    Ok(status.expect("program vanished").shell_status())
}

/// brk(addr): move the end of the heap. Returns the new break, or the
/// current one if `addr` is 0, out of range, or memory ran out (Linux
/// semantics: the caller compares the result with what it asked for).
pub fn sys_brk(addr: u64) -> i64 {
    use x86_64::structures::paging::{FrameAllocator, FrameDeallocator, Mapper, Page, PageTableFlags as F, Size4KiB};

    let (p4, current) = {
        let table = TABLE.lock();
        let p = table.get(&current_pid()).unwrap();
        (p.p4, p.mm.lock().brk)
    };
    if addr < HEAP_BASE || addr > HEAP_BASE + HEAP_MAX {
        return current as i64;
    }

    let page_up = |a: u64| (a + 4095) & !4095;
    let (old_end, new_end) = (page_up(current), page_up(addr));
    let mut mapper = memory::mapper_for(p4);
    let mut alloc = FRAME_ALLOCATOR.lock();
    let frames = alloc.as_mut().unwrap();

    if new_end > old_end {
        let mut page_addr = old_end;
        while page_addr < new_end {
            let page = Page::<Size4KiB>::containing_address(VirtAddr::new(page_addr));
            let mapped = frames.allocate_frame().and_then(|frame| {
                memory::zero_frame(frame);
                let flags = F::PRESENT | F::WRITABLE | F::USER_ACCESSIBLE;
                match unsafe { mapper.map_to(page, frame, flags, frames) } {
                    Ok(flush) => {
                        flush.flush();
                        Some(())
                    }
                    Err(_) => {
                        unsafe { frames.deallocate_frame(frame) };
                        None
                    }
                }
            });
            if mapped.is_none() {
                // Out of memory: undo this call's mappings, keep the old break.
                let mut undo = old_end;
                while undo < page_addr {
                    let page = Page::<Size4KiB>::containing_address(VirtAddr::new(undo));
                    if let Ok((frame, flush)) = mapper.unmap(page) {
                        flush.flush();
                        unsafe { frames.deallocate_frame(frame) };
                    }
                    undo += 4096;
                }
                return current as i64;
            }
            page_addr += 4096;
        }
    } else {
        let mut page_addr = new_end;
        let mut gone = Vec::new();
        while page_addr < old_end {
            let page = Page::<Size4KiB>::containing_address(VirtAddr::new(page_addr));
            if let Ok((frame, flush)) = mapper.unmap(page) {
                flush.flush();
                gone.push(frame);
            }
            page_addr += 4096;
        }
        // Freed once other cores running this address space forgot them.
        if !gone.is_empty() {
            crate::smp::shootdown(current_p4().start_address().as_u64());
        }
        for frame in gone {
            frames.release(frame); // it may be shared with a fork
        }
    }
    drop(alloc);

    with_current_mm(|mm| mm.brk = addr);
    addr as i64
}

// ---- signals, process groups, time ----

pub const SIGHUP: i32 = 1;
pub const SIGINT: i32 = 2;
pub const SIGQUIT: i32 = 3;
pub const SIGKILL: i32 = 9;
pub const SIGILL: i32 = 4;
pub const SIGTRAP: i32 = 5;
pub const SIGBUS: i32 = 7;
pub const SIGFPE: i32 = 8;
pub const SIGSEGV: i32 = 11;
pub const SIGPIPE: i32 = 13;
pub const SIGALRM: i32 = 14;
pub const SIGCHLD: i32 = 17;
pub const SIGCONT: i32 = 18;
pub const SIGSTOP: i32 = 19;
pub const SIGTSTP: i32 = 20;
pub const SIGTTIN: i32 = 21;
pub const SIGTTOU: i32 = 22;
pub const SIGWINCH: i32 = 28;
pub const SIGIO: i32 = 29;

/// Processes that asked for SIGIO when keyboard or mouse input arrives
/// (O_ASYNC with F_SETOWN on the console or the mouse: an X server).
pub static SIGIO_INPUT: Mutex<Vec<u64>> = Mutex::new(Vec::new());
const MAX_SIGNAL: i32 = 64;

pub const SIG_DFL: u64 = 0;
pub const SIG_IGN: u64 = 1;

// sigaction flags (Linux values)
const SA_NOCLDSTOP: u64 = 1;
const SA_RESTORER: u64 = 0x0400_0000;
const SA_ONSTACK: u64 = 0x0800_0000;
const SA_RESTART: u64 = 0x1000_0000;
const SA_NODEFER: u64 = 0x4000_0000;
const SA_RESETHAND: u64 = 0x8000_0000;

/// What a process does when it gets a signal: struct sigaction as Linux
/// passes it (handler, flags, restorer, mask).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SigAction {
    /// SIG_DFL, SIG_IGN, or the address of a handler function.
    pub handler: u64,
    pub flags: u64,
    /// Where the handler returns to: code that calls rt_sigreturn.
    pub restorer: u64,
    /// Signals blocked while the handler runs.
    pub mask: u64,
}

impl SigAction {
    pub const DEFAULT: SigAction = SigAction { handler: SIG_DFL, flags: 0, restorer: 0, mask: 0 };
}

/// What SIG_DFL means for a signal.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DefaultAction {
    Terminate,
    Ignore,
    Stop,
    Continue,
}

fn default_action(sig: i32) -> DefaultAction {
    match sig {
        SIGCHLD | 23 | 28 => DefaultAction::Ignore, // CHLD, URG, WINCH
        SIGCONT => DefaultAction::Continue,
        SIGSTOP | SIGTSTP | SIGTTIN | SIGTTOU => DefaultAction::Stop,
        _ => DefaultAction::Terminate,
    }
}

const STOP_SIGNALS: u64 = (1 << (SIGSTOP - 1)) | (1 << (SIGTSTP - 1)) | (1 << (SIGTTIN - 1)) | (1 << (SIGTTOU - 1));
/// SIGKILL and SIGSTOP can't be blocked, caught or ignored.
const UNBLOCKABLE: u64 = (1 << (SIGKILL - 1)) | (1 << (SIGSTOP - 1));

/// Process group that owns the keyboard (gets SIGINT on Ctrl-C). 0 = the kernel.
static FOREGROUND: AtomicU64 = AtomicU64::new(0);
/// Signals the keyboard interrupt asked for (Ctrl-C, Ctrl-Z, Ctrl-\), as a
/// bit set; sent at the next safe point (the interrupt handler itself must
/// not take locks).
static TTY_SIGNALS: AtomicU64 = AtomicU64::new(0);
static TICKS: AtomicU64 = AtomicU64::new(0);

/// Length of a timer tick: 11932 / 1193182 Hz (interrupts::PIT_DIVISOR),
/// about 10 ms.
pub const TICK_NS: u64 = 10_000_151;

pub fn foreground() -> u64 {
    FOREGROUND.load(Ordering::SeqCst)
}

/// Called from the keyboard interrupt on Ctrl-C (when the console is cooked).
pub fn ctrl_c_pressed() {
    tty_signal(SIGINT);
}

/// Called from the keyboard interrupt for Ctrl-C (SIGINT), Ctrl-Z
/// (SIGTSTP) and Ctrl-\ (SIGQUIT).
pub fn tty_signal(sig: i32) {
    TTY_SIGNALS.fetch_or(bit(sig), Ordering::SeqCst);
}

/// Called from the timer interrupt.
pub fn tick() {
    // The time-stamp counter now, and how far it goes in a tick (averaged:
    // the timer's interrupts arrive a little late or early), for clock_ns.
    let tsc = unsafe { core::arch::x86_64::_rdtsc() };
    let last = TICK_TSC.load(Ordering::Relaxed);
    if last != 0 && tsc > last {
        let d = tsc - last;
        let per = TSC_PER_TICK.load(Ordering::Relaxed);
        let per = if per == 0 { d } else { (per * 15 + d) / 16 };
        TSC_PER_TICK.store(per, Ordering::Relaxed);
    }
    CLOCK_SEQ.fetch_add(1, Ordering::AcqRel);
    TICK_TSC.store(tsc, Ordering::Relaxed);
    TICKS.fetch_add(1, Ordering::Relaxed);
    CLOCK_SEQ.fetch_add(1, Ordering::AcqRel);
}

/// The tick to wake at for a wait of `ns` nanoseconds from now: the first
/// one at or after clock_ns() + ns (so a sleep lasts at least that long by
/// the clock programs read: it's finer than a tick).
pub fn tick_after_ns(ns: u64) -> u64 {
    clock_ns().saturating_add(ns).div_ceil(TICK_NS)
}

/// The time-stamp counter at the last tick, and per tick (see clock_ns).
static TICK_TSC: AtomicU64 = AtomicU64::new(0);
static TSC_PER_TICK: AtomicU64 = AtomicU64::new(0);
/// Odd while tick() is changing them.
static CLOCK_SEQ: AtomicU64 = AtomicU64::new(0);
/// The latest clock_ns returned: it never goes back.
static CLOCK_LAST: AtomicU64 = AtomicU64::new(0);

/// Nanoseconds since boot, finer than a tick: the ticks, and the time-stamp
/// counter since the last one (at most a tick's worth). For reading the
/// time (clock_gettime, gettimeofday): two calls in a row see different
/// times, as programs expect (musl's mkstemp names files from it, and
/// retries with the same name while it stands still). Never goes back.
pub fn clock_ns() -> u64 {
    let (ticks, since) = loop {
        let seq = CLOCK_SEQ.load(Ordering::Acquire);
        if seq & 1 != 0 {
            core::hint::spin_loop();
            continue;
        }
        let (t, at, per) = (TICKS.load(Ordering::Relaxed), TICK_TSC.load(Ordering::Relaxed), TSC_PER_TICK.load(Ordering::Relaxed));
        if CLOCK_SEQ.load(Ordering::Acquire) != seq {
            continue;
        }
        let now = unsafe { core::arch::x86_64::_rdtsc() };
        let since = if per == 0 || now <= at { 0 } else { ((now - at) as u128 * TICK_NS as u128 / per as u128).min(TICK_NS as u128 - 1) as u64 };
        break (t, since);
    };
    let ns = ticks * TICK_NS + since;
    let prev = CLOCK_LAST.fetch_max(ns, Ordering::AcqRel);
    ns.max(prev)
}

/// Every 5 seconds, the load averages: from core 0's timer interrupt, with
/// the big kernel lock held. (Not in tick(), which runs before it: the
/// process table taken there behind another core's back made that core's
/// page faults fail their try_lock and kill programs with SIGSEGV.)
pub fn sample_load_if_due() {
    let now = ticks();
    if now >= NEXT_LOAD_SAMPLE.load(Ordering::Relaxed) {
        sample_load(now);
    }
}

/// Load averages over 1, 5 and 15 minutes, as on Linux: fixed point with
/// 11 fraction bits, updated every 5 seconds with the number of processes
/// running or ready to run.
static LOAD: [AtomicU64; 3] = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];
static NEXT_LOAD_SAMPLE: AtomicU64 = AtomicU64::new(0);
const LOAD_FIXED_1: u64 = 1 << 11;
/// e^(-5 s / 1, 5, 15 min), times LOAD_FIXED_1.
const LOAD_EXP: [u64; 3] = [1884, 2014, 2037];

/// From the timer interrupt: never waits for the process table (a busy
/// table means try again next tick).
fn sample_load(now: u64) {
    let Some(table) = TABLE.try_lock() else { return };
    let running = table.values().filter(|p| p.pid != 0 && matches!(p.state, State::Running | State::Ready)).count() as u64;
    drop(table);
    for (load, exp) in LOAD.iter().zip(LOAD_EXP) {
        let old = load.load(Ordering::Relaxed);
        load.store((old * exp + running * LOAD_FIXED_1 * (LOAD_FIXED_1 - exp)) / LOAD_FIXED_1, Ordering::Relaxed);
    }
    NEXT_LOAD_SAMPLE.store(now + 5_000_000_000 / TICK_NS, Ordering::Relaxed);
}

/// The load averages, in hundredths (1.25 is 125).
pub fn load_averages() -> [u64; 3] {
    LOAD.each_ref().map(|l| l.load(Ordering::Relaxed) * 100 / LOAD_FIXED_1)
}

pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

fn bit(sig: i32) -> u64 {
    1u64 << (sig - 1)
}

fn lowest(set: u64) -> i32 {
    set.trailing_zeros() as i32 + 1
}

/// Is `pgid` an orphaned process group: no member has a parent outside the
/// group (other than the kernel)? Nobody could continue such a group, so
/// Ctrl-Z and friends (SIGTSTP, SIGTTIN, SIGTTOU) don't stop it (POSIX).
/// Programs the kernel starts (login, sh) are in orphaned groups.
fn orphaned_locked(table: &BTreeMap<u64, Box<Process>>, pgid: u64) -> bool {
    !table.values().any(|m| {
        m.pgid == pgid && m.ppid != 0 && table.get(&m.ppid).is_some_and(|parent| parent.pgid != pgid)
    })
}

/// Would delivering `sig` to `p` do nothing (so it may as well be dropped)?
fn discarded(p: &Process, sig: i32) -> bool {
    let act = p.actions[(sig - 1) as usize];
    // A blocked signal is kept, as on Linux (its handling may change before
    // it's unblocked, or a signalfd/sigwait takes it); signal_pending drops
    // it later if it's still ignored.
    sig != SIGKILL
        && p.blocked & bit(sig) == 0
        && p.sigwait & bit(sig) == 0
        && (act.handler == SIG_IGN
            || (act.handler == SIG_DFL && matches!(default_action(sig), DefaultAction::Ignore | DefaultAction::Continue)))
}

/// Tell a parent its child stopped, continued or exited: wake it if it's in
/// wait4, and send it SIGCHLD (unless it asked for SA_NOCLDSTOP).
fn notify_parent_locked(table: &mut BTreeMap<u64, Box<Process>>, child: u64, stop_or_continue: bool) {
    let Some(ppid) = table.get(&child).map(|c| c.ppid) else { return };
    let Some(parent) = table.get_mut(&ppid) else { return };
    if parent.state == State::Waiting {
        parent.state = State::Ready;
    }
    let nocldstop = parent.actions[(SIGCHLD - 1) as usize].flags & SA_NOCLDSTOP != 0;
    if ppid != 0 && !(stop_or_continue && nocldstop) {
        let info = child_info(table, child);
        signal_info_locked(table, ppid, SIGCHLD, info);
    }
}

/// Send `sig` to one process, from the kernel. Returns false if there's no
/// such process.
fn signal_locked(table: &mut BTreeMap<u64, Box<Process>>, pid: u64, sig: i32) -> bool {
    signal_info_locked(table, pid, sig, SigInfo::KERNEL)
}

/// Send `sig` to one process, saying why (its siginfo).
fn signal_info_locked(table: &mut BTreeMap<u64, Box<Process>>, pid: u64, sig: i32, info: SigInfo) -> bool {
    if pid == 0 {
        return false; // the kernel can't be signaled
    }
    let orphan = {
        let Some(p) = table.get(&pid) else { return false };
        if sig == 0 || matches!(p.state, State::Zombie(_)) {
            return true;
        }
        orphaned_locked(table, p.pgid)
    };
    // Blocked where it was sent, but a thread of the process is in
    // sigwait for it (a program's signal thread): it goes there.
    let pid = {
        let p = &table[&pid];
        if sig != SIGKILL && p.blocked & bit(sig) != 0 {
            let tgid = p.tgid;
            table
                .values()
                .find(|t| t.tgid == tgid && t.sigwait & bit(sig) != 0 && !matches!(t.state, State::Zombie(_)))
                .map_or(pid, |t| t.pid)
        } else {
            pid
        }
    };
    let mut continued = false;
    {
        let p = table.get_mut(&pid).unwrap();
        if sig == SIGCONT {
            // Continuing happens whatever the handler: the process must run
            // to handle anything. Pending stops are cancelled.
            p.pending &= !STOP_SIGNALS;
            if p.state == State::Stopped {
                p.state = State::Ready;
                p.wait_event = Some(0xffff);
                continued = true;
            }
        } else if STOP_SIGNALS & bit(sig) != 0 {
            p.pending &= !bit(SIGCONT);
        }
        let default_stop = p.actions[(sig - 1) as usize].handler == SIG_DFL && sig != SIGSTOP && STOP_SIGNALS & bit(sig) != 0;
        if !discarded(p, sig) && !(default_stop && orphan) {
            p.pending |= bit(sig);
            p.siginfo.insert(sig, info);
            let wake = sig == SIGKILL || p.blocked & bit(sig) == 0 || p.sigwait & bit(sig) != 0;
            if wake && matches!(p.state, State::Waiting | State::Sleeping | State::Blocked(_)) {
                p.state = State::Ready; // so it notices
            }

            if sig == SIGKILL && p.state == State::Stopped {
                p.state = State::Ready;
            }
        }
    }
    // Threads of the process in poll/epoll recheck their fds (a signalfd
    // may be readable now).
    let tgid = table[&pid].tgid;
    for t in table.values_mut().filter(|t| t.tgid == tgid && t.state == State::Blocked(POLL_KEY)) {
        t.state = State::Ready;
    }
    if continued {
        notify_parent_locked(table, pid, true);
    }
    true
}

fn signal_group_locked(table: &mut BTreeMap<u64, Box<Process>>, pgid: u64, sig: i32) -> usize {
    // One delivery per process (thread group) in the group.
    let mut members: Vec<u64> = table.values().filter(|p| p.pgid == pgid && p.pid != 0).map(|p| p.tgid).collect();
    members.dedup();
    members.iter().filter(|&&tgid| signal_process_locked(table, tgid, sig)).count()
}

/// Send `sig` to a process (thread group), as kill() does. Stop, continue
/// and SIGKILL reach every thread; any other signal goes to one thread
/// that isn't blocking it (the first thread if they all are). Returns
/// false if there's no such process.
fn signal_process_locked(table: &mut BTreeMap<u64, Box<Process>>, tgid: u64, sig: i32) -> bool {
    signal_process_info_locked(table, tgid, sig, SigInfo::KERNEL)
}

fn signal_process_info_locked(table: &mut BTreeMap<u64, Box<Process>>, tgid: u64, sig: i32, info: SigInfo) -> bool {
    let threads: Vec<(u64, bool)> = table
        .values()
        .filter(|t| t.tgid == tgid && !matches!(t.state, State::Zombie(_)))
        .map(|t| (t.pid, sig > 0 && t.blocked & bit(sig) != 0))
        .collect();
    if threads.len() <= 1 {
        return signal_info_locked(table, tgid, sig, info); // one thread (or a zombie)
    }
    if sig == SIGKILL || sig == SIGCONT || (sig > 0 && STOP_SIGNALS & bit(sig) != 0) {
        for (t, _) in &threads {
            signal_info_locked(table, *t, sig, info);
        }
        return true;
    }
    let target = threads.iter().find(|(_, blocked)| !blocked).map_or(threads[0].0, |(t, _)| *t);
    signal_info_locked(table, target, sig, info)
}

/// Send the signals typed at the keyboard to the foreground process group.
fn deliver_tty_signals(table: &mut BTreeMap<u64, Box<Process>>) {
    let mut sigs = TTY_SIGNALS.swap(0, Ordering::SeqCst);
    while sigs != 0 {
        let sig = lowest(sigs);
        sigs &= !bit(sig);
        let fg = foreground();
        if fg == 0 || !table.values().any(|p| p.pgid == fg && p.pid != 0) {
            continue;
        }
        if sig == SIGTSTP && orphaned_locked(table, fg) {
            continue; // nobody could continue it
        }
        signal_group_locked(table, fg, sig);
        match sig {
            SIGINT => crate::println!("^C"),
            SIGTSTP => crate::println!("^Z"),
            SIGQUIT => crate::println!("^\\"),
            _ => {}
        }
    }
}

/// Fire SIGALRM for every alarm that's due.
fn fire_alarms_locked(table: &mut BTreeMap<u64, Box<Process>>) {
    let now = ticks();
    let due: Vec<u64> = table.values().filter(|p| p.alarm_at != 0 && now >= p.alarm_at).map(|p| p.pid).collect();
    for pid in due {
        let p = table.get_mut(&pid).unwrap();
        p.alarm_at = if p.alarm_every > 0 { now + p.alarm_every } else { 0 };
        signal_locked(table, pid, SIGALRM);
    }
}

/// Is there a signal for `pid` that will interrupt what it's doing: one it
/// handles, or one that ends it? (Not blocked, ignored, or stop/continue.)
fn loud_pending_locked(table: &BTreeMap<u64, Box<Process>>, pid: u64) -> bool {
    let Some(p) = table.get(&pid) else { return false };
    let mut set = p.pending & !p.blocked;
    while set != 0 {
        let sig = lowest(set);
        set &= !bit(sig);
        let act = p.actions[(sig - 1) as usize];
        if act.handler > SIG_IGN || (act.handler == SIG_DFL && default_action(sig) == DefaultAction::Terminate) {
            return true;
        }
    }
    false
}

/// Called by blocking syscalls while they wait, and before returning to the
/// program. Deals with signals the program never sees: ignored ones are
/// dropped, and stop signals stop the process right here (it continues
/// waiting after SIGCONT, so `sleep 10`, Ctrl-Z, `fg` still sleeps 10 s).
/// Returns true if a signal remains that should interrupt the syscall
/// (EINTR): one with a handler, or one that ends the process.
pub fn signal_pending() -> bool {
    loop {
        {
            let mut table = TABLE.lock();
            deliver_tty_signals(&mut table);
            fire_alarms_locked(&mut table);
            fire_posix_timers_locked(&mut table);
            let cur = current_pid();
            if cur == 0 {
                return false;
            }
            let orphan = orphaned_locked(&table, table[&cur].pgid);
            let p = table.get_mut(&cur).unwrap();
            let mut set = p.pending & !p.blocked;
            let mut stop = None;
            while set != 0 {
                let sig = lowest(set);
                set &= !bit(sig);
                let act = p.actions[(sig - 1) as usize];
                if act.handler > SIG_IGN || (act.handler == SIG_DFL && default_action(sig) == DefaultAction::Terminate) {
                    return true;
                }
                p.pending &= !bit(sig);
                if act.handler == SIG_DFL && default_action(sig) == DefaultAction::Stop && !(orphan && sig != SIGSTOP) {
                    stop = Some(sig);
                    break;
                }
            }
            let Some(sig) = stop else { return false };
            p.state = State::Stopped;
            p.wait_event = Some(((sig as u32) << 8) | 0x7f);
            notify_parent_locked(&mut table, cur, true);
        }
        schedule(); // until SIGCONT (or SIGKILL)
    }
}

/// Syscalls that SA_RESTART restarts after a handler (not sleeps or waits
/// for signals, which always report EINTR, as on Linux).
fn restartable(num: u64) -> bool {
    !matches!(num, SYS_NANOSLEEP | SYS_PAUSE | SYS_RT_SIGSUSPEND)
}

/// Syscalls that report EINTR after a handler even with SA_RESTART, as
/// Linux's do (signal(7)): waiting for any of several things. Programs
/// rely on it: a handler sets a flag, and the select loop sees it.
/// (Stopped and continued with no handler, they still carry on.)
fn restartable_after_handler(num: u64) -> bool {
    restartable(num) && !matches!(num, SYS_POLL | SYS_PPOLL | SYS_SELECT | SYS_PSELECT6 | SYS_CLOCK_NANOSLEEP)
}

/// Act on pending signals before returning to ring 3 (after a syscall,
/// with its number and result, or from the timer). A handler is run by
/// rewriting `frame` so the program "returns" into it; the default action
/// may end the process instead.
pub fn handle_signals(frame: &mut SyscallFrame, syscall: Option<(u64, i64)>) {
    let restart = |frame: &mut SyscallFrame| {
        // Back up over `int 0x80` (2 bytes) with the syscall number in rax:
        // the program makes the same call again.
        if let Some((num, _)) = syscall {
            frame.rax = num;
            frame.rip -= 2;
        }
    };
    if !signal_pending() {
        // Interrupted, but nothing for the program to see (e.g. it was
        // stopped and continued): just carry on with the syscall.
        if matches!(syscall, Some((num, ret)) if ret == -EINTR && restartable(num)) {
            restart(frame);
        }
        let mut table = TABLE.lock();
        if let Some(p) = table.get_mut(&current_pid()) {
            if let Some(mask) = p.saved_mask.take() {
                p.blocked = mask;
            }
        }
        return;
    }
    let (sig, act, old_mask, info, alt, on_alt) = {
        let mut table = TABLE.lock();
        let p = table.get_mut(&current_pid()).unwrap();
        // signal_pending dropped everything quieter: the lowest deliverable
        // signal is the one to act on.
        let sig = lowest(p.pending & !p.blocked);
        p.pending &= !bit(sig);
        let info = p.siginfo.remove(&sig).unwrap_or(SigInfo::KERNEL);
        let act = p.actions[(sig - 1) as usize];
        // SA_ONSTACK: onto the alternate stack, unless already on it.
        let alt = p.altstack;
        let on_alt = act.flags & SA_ONSTACK != 0 && alt.flags & SS_DISABLE == 0 && !alt.holds(frame.rsp);
        if on_alt && alt.flags & SS_AUTODISARM != 0 {
            p.altstack = AltStack::DISABLED; // back with sigreturn
        }
        if act.handler == SIG_DFL {
            drop(table);
            exit_group(ExitReason::Signaled(sig));
        }
        let old_mask = p.saved_mask.take().unwrap_or(p.blocked);
        let mut during = p.blocked | act.mask;
        if act.flags & SA_NODEFER == 0 {
            during |= bit(sig);
        }
        p.blocked = during & !UNBLOCKABLE;
        if act.flags & SA_RESETHAND != 0 {
            p.actions[(sig - 1) as usize] = SigAction::DEFAULT;
        }
        (sig, act, old_mask, info, alt, on_alt)
    };
    if matches!(syscall, Some((num, ret)) if ret == -EINTR && restartable_after_handler(num)) && act.flags & SA_RESTART != 0 {
        restart(frame);
    }
    // No way back from the handler without a restorer (libc always gives one).
    let frame_info = crate::signal::Delivery {
        info: info.bytes(sig),
        stack_top: on_alt.then(|| alt.sp + alt.size),
        uc_stack: (alt.sp, alt.flags_at(frame.rsp), alt.size),
    };
    if act.flags & SA_RESTORER == 0
        || crate::signal::setup_frame(frame, sig, act.handler, act.restorer, old_mask, &frame_info).is_err()
    {
        exit_group(ExitReason::Signaled(SIGSEGV));
    }
}

/// A fault in the program (from the exception handlers, which have its
/// whole frame): its handler runs, if it has one that isn't blocked, with
/// `info` (the address...). Returns false if not: the caller ends the
/// program with `sig` (as Linux does when the signal is blocked or ignored:
/// a fault can't be skipped).
pub fn fault_signal(frame: &mut SyscallFrame, sig: i32, info: SigInfo) -> bool {
    {
        let mut table = TABLE.lock();
        let p = table.get_mut(&current_pid()).unwrap();
        let act = p.actions[(sig - 1) as usize];
        if act.handler == SIG_DFL || act.handler == SIG_IGN || p.blocked & bit(sig) != 0 {
            return false;
        }
        p.pending |= bit(sig);
        p.siginfo.insert(sig, info);
    }
    handle_signals(frame, None);
    true
}

/// rt_sigreturn(): back from a handler. Restores the registers and signal
/// mask saved in the frame on the user stack. Returns the restored rax.
pub fn sys_rt_sigreturn(frame: &mut SyscallFrame) -> i64 {
    match crate::signal::restore_frame(frame) {
        Ok((mask, (sp, flags, size))) => {
            let mut table = TABLE.lock();
            let p = table.get_mut(&current_pid()).unwrap();
            p.blocked = mask & !UNBLOCKABLE;
            // The alternate stack as it was when the signal came (as Linux
            // does: SS_AUTODISARM turned it off meanwhile).
            p.altstack = if flags & SS_DISABLE != 0 || size < 2048 {
                AltStack::DISABLED
            } else {
                AltStack { sp, size, flags: flags & SS_AUTODISARM }
            };
            frame.rax as i64
        }
        Err(()) => exit_group(ExitReason::Signaled(SIGSEGV)),
    }
}

/// kill(pid, sig)
pub fn sys_kill(pid: u64, sig: u64) -> i64 {
    let (pid, sig) = (pid as i64 as i32 as i64, sig as i64);
    if !(0..=MAX_SIGNAL as i64).contains(&sig) {
        return -EINVAL;
    }
    let sig = sig as i32;
    let mut table = TABLE.lock();
    let cur = current_pid();
    let me = table.get(&cur).map(|p| p.cred.clone()).unwrap_or(Cred::ROOT);
    // POSIX: root may signal anything; others only processes whose real or
    // saved user ID matches their real or effective one. (SIGCONT may go to
    // anything in the same... we have no sessions, so the same rule.)
    let allowed = |p: &Process| {
        me.euid == 0 || [me.ruid, me.euid].iter().any(|&u| u == p.cred.ruid || u == p.cred.suid)
    };
    // Processes (thread groups): kill(TID) of any thread means its process.
    let my_tgid = table.get(&cur).map_or(0, |p| p.tgid);
    let mut targets: Vec<u64> = if pid > 0 {
        table.get(&(pid as u64)).map(|p| p.tgid).into_iter().collect()
    } else if pid == 0 {
        let pgid = table.get(&cur).map(|p| p.pgid).unwrap_or(0);
        table.values().filter(|p| p.pgid == pgid && p.pid != 0).map(|p| p.tgid).collect()
    } else if pid == -1 {
        table.values().filter(|p| p.pid != 0 && p.tgid != my_tgid).map(|p| p.tgid).collect()
    } else {
        table.values().filter(|p| p.pgid == (-pid) as u64 && p.pid != 0).map(|p| p.tgid).collect()
    };
    targets.sort_unstable();
    targets.dedup();
    if targets.is_empty() {
        return -ESRCH;
    }
    let permitted: Vec<u64> = targets.iter().copied().filter(|t| table.get(t).is_some_and(|p| allowed(p))).collect();
    if permitted.is_empty() {
        return -EPERM;
    }
    let info = SigInfo::from_current(&table, SI_USER, 0);
    for t in permitted {
        signal_process_info_locked(&mut table, t, sig, info);
    }
    0
}

/// tgkill(tgid, tid, sig) and tkill(tid, sig): one thread is one process here.
/// tkill(tid, sig) / tgkill(tgid, tid, sig): a signal for one thread
/// (raise() and pthread_kill use these). `tgid` 0 means "don't check".
pub fn sys_tkill(tgid: u64, tid: u64, sig: u64) -> i64 {
    if tid as i64 <= 0 || !(0..=MAX_SIGNAL as u64).contains(&sig) {
        return -EINVAL;
    }
    let mut table = TABLE.lock();
    let me = table.get(&current_pid()).map(|p| p.cred.clone()).unwrap_or(Cred::ROOT);
    let Some(t) = table.get(&tid).filter(|t| tgid == 0 || t.tgid == tgid) else { return -ESRCH };
    let allowed = me.euid == 0 || [me.ruid, me.euid].iter().any(|&u| u == t.cred.ruid || u == t.cred.suid);
    if !allowed {
        return -EPERM;
    }
    let info = SigInfo::from_current(&table, SI_TKILL, 0);
    signal_info_locked(&mut table, tid, sig as i32, info);
    0
}

/// rt_sigqueueinfo(tgid, sig, info) and rt_tgsigqueueinfo(tgid, tid, sig,
/// info) (`tid` 0: the process): a signal with a value (sigqueue). Only
/// negative si_codes (SI_QUEUE...) unless it's to oneself, as on Linux.
pub fn sys_rt_sigqueueinfo(tgid: u64, tid: u64, sig: u64, uinfo: u64) -> i64 {
    if !(1..=MAX_SIGNAL as u64).contains(&sig) {
        return -EINVAL;
    }
    let Some(u) = (unsafe { user_slice(uinfo, 32) }) else { return -EFAULT };
    let code = i32::from_le_bytes(u[8..12].try_into().unwrap());
    let value = u64::from_le_bytes(u[24..32].try_into().unwrap());
    let mut table = TABLE.lock();
    let me = table.get(&current_pid()).map(|p| (p.cred.clone(), p.tgid));
    let Some((cred, my_tgid)) = me else { return -ESRCH };
    let target = if tid != 0 { tid } else { tgid };
    let Some(t) = table.get(&target).filter(|t| t.tgid == tgid) else { return -ESRCH };
    if (code >= 0 || code == SI_TKILL) && t.tgid != my_tgid {
        return -EPERM;
    }
    let allowed = cred.euid == 0 || [cred.ruid, cred.euid].iter().any(|&u| u == t.cred.ruid || u == t.cred.suid);
    if !allowed {
        return -EPERM;
    }
    let info = SigInfo::from_current(&table, code, value);
    if tid != 0 {
        signal_info_locked(&mut table, tid, sig as i32, info);
    } else {
        signal_process_info_locked(&mut table, tgid, sig as i32, info);
    }
    0
}

/// setpgid(pid, pgid): 0 means "the caller" / "same as pid".
pub fn sys_setpgid(pid: u64, pgid: u64) -> i64 {
    let mut table = TABLE.lock();
    let cur = current_pid();
    let pid = if pid == 0 { cur } else { pid };
    let pgid = if pgid == 0 { pid } else { pgid };
    match table.get_mut(&pid) {
        Some(p) if p.pid == cur || p.ppid == cur => {
            p.pgid = pgid;
            0
        }
        _ => -ESRCH,
    }
}

/// getpgid(pid) (and getpgrp() = getpgid(0))
pub fn sys_getpgid(pid: u64) -> i64 {
    let table = TABLE.lock();
    let pid = if pid == 0 { current_pid() } else { pid };
    table.get(&pid).map(|p| p.pgid as i64).unwrap_or(-ESRCH)
}

/// The controlling terminal is the console (every program the kernel
/// starts has it).
pub const CTTY_CONSOLE: u32 = 0;
/// No controlling terminal (after setsid, until one is acquired).
pub const CTTY_NONE: u32 = u32::MAX;

/// setsid(): a new session (and process group), led by the caller, with no
/// controlling terminal. A group leader can't (EPERM), as in POSIX.
pub fn sys_setsid() -> i64 {
    let mut table = TABLE.lock();
    let cur = current_pid();
    let tgid = table.get(&cur).map_or(cur, |p| p.tgid);
    if table.values().any(|p| p.pgid == tgid && p.pid != 0) {
        return -EPERM;
    }
    for p in table.values_mut().filter(|p| p.tgid == tgid) {
        p.sid = tgid;
        p.pgid = tgid;
        p.ctty = CTTY_NONE;
    }
    tgid as i64
}

/// getsid(pid) (0: the caller)
pub fn sys_getsid(pid: u64) -> i64 {
    let table = TABLE.lock();
    let pid = if pid == 0 { current_pid() } else { pid };
    table.get(&pid).map(|p| p.sid as i64).unwrap_or(-ESRCH)
}

/// The caller's (session, process group, controlling terminal, session
/// leader?): what terminal code needs.
pub fn current_session() -> (u64, u64, u32, bool) {
    let table = TABLE.lock();
    table.get(&current_pid()).map_or((0, 0, CTTY_CONSOLE, false), |p| (p.sid, p.pgid, p.ctty, p.sid == p.tgid))
}

/// Give the caller's whole session controlling terminal `ctty`.
pub fn set_session_ctty(sid: u64, ctty: u32) {
    let mut table = TABLE.lock();
    for p in table.values_mut().filter(|p| p.sid == sid && p.pid != 0) {
        p.ctty = ctty;
    }
}

/// The caller alone gives up its controlling terminal (TIOCNOTTY).
pub fn set_session_ctty_of_current(ctty: u32) {
    let mut table = TABLE.lock();
    let tgid = table.get(&current_pid()).map_or(0, |p| p.tgid);
    for p in table.values_mut().filter(|p| p.tgid == tgid) {
        p.ctty = ctty;
    }
}

/// A terminal went away: processes that had it have no controlling
/// terminal now.
pub fn drop_ctty(ctty: u32) {
    let mut table = TABLE.lock();
    for p in table.values_mut().filter(|p| p.ctty == ctty) {
        p.ctty = CTTY_NONE;
    }
}

/// Send `sig` to every process in group `pgid` (from terminals: Ctrl-C,
/// hangups, window changes). Returns whether anyone got it.
pub fn signal_pgrp(pgid: u64, sig: i32) -> bool {
    if pgid == 0 {
        return false;
    }
    let mut table = TABLE.lock();
    if sig == SIGTSTP && orphaned_locked(&table, pgid) {
        return false; // nobody could continue it
    }
    signal_group_locked(&mut table, pgid, sig) > 0
}

/// Send `sig` to one process (by pid; a session leader).
pub fn signal_pid(pid: u64, sig: i32) {
    let mut table = TABLE.lock();
    if let Some(tgid) = table.get(&pid).map(|p| p.tgid) {
        signal_process_locked(&mut table, tgid, sig);
    }
}

/// Would `sig` be ignored or blocked by the caller? (A background read
/// from a terminal fails with EIO then, instead of stopping.)
pub fn ignores_or_blocks(sig: i32) -> bool {
    let table = TABLE.lock();
    table.get(&current_pid()).is_some_and(|p| {
        p.blocked & bit(sig) != 0 || p.actions[(sig - 1) as usize].handler == SIG_IGN
    })
}

/// Is the caller's process group orphaned (see orphaned_locked)?
pub fn current_pgrp_orphaned() -> bool {
    let table = TABLE.lock();
    table.get(&current_pid()).is_some_and(|p| orphaned_locked(&table, p.pgid))
}

pub fn set_foreground(pgid: u64) {
    FOREGROUND.store(pgid, Ordering::SeqCst);
}

fn read_u64(ptr: u64) -> Result<u64, i64> {
    match unsafe { user_slice(ptr, 8) } {
        Some(b) => Ok(u64::from_le_bytes(b.try_into().unwrap())),
        None => Err(EFAULT),
    }
}

fn write_u64(ptr: u64, v: u64) -> Result<(), i64> {
    match unsafe { user_slice_mut(ptr, 8) } {
        Some(out) => {
            out.copy_from_slice(&v.to_le_bytes());
            Ok(())
        }
        None => Err(EFAULT),
    }
}

/// rt_sigaction(sig, act, oldact, sigsetsize): set what a signal does:
/// SIG_DFL, SIG_IGN, or a handler (with flags and a mask).
pub fn sys_rt_sigaction(sig: u64, act: u64, oldact: u64) -> i64 {
    let sig = sig as i32;
    if !(1..=MAX_SIGNAL).contains(&sig) || (act != 0 && bit(sig) & UNBLOCKABLE != 0) {
        return -EINVAL;
    }
    let new = if act != 0 {
        match unsafe { user_slice(act, 32) } {
            Some(b) => {
                let word = |i: usize| u64::from_le_bytes(b[i * 8..i * 8 + 8].try_into().unwrap());
                Some(SigAction { handler: word(0), flags: word(1), restorer: word(2), mask: word(3) & !UNBLOCKABLE })
            }
            None => return -EFAULT,
        }
    } else {
        None
    };
    let mut table = TABLE.lock();
    let p = table.get_mut(&current_pid()).unwrap();
    let old = p.actions[(sig - 1) as usize];
    if oldact != 0 {
        let Some(out) = (unsafe { user_slice_mut(oldact, 32) }) else { return -EFAULT };
        for (i, v) in [old.handler, old.flags, old.restorer, old.mask].into_iter().enumerate() {
            out[i * 8..i * 8 + 8].copy_from_slice(&v.to_le_bytes());
        }
    }
    if let Some(new) = new {
        // Handlers belong to the process: every thread gets the change.
        let tgid = p.tgid;
        for t in table.values_mut().filter(|t| t.tgid == tgid) {
            t.actions[(sig - 1) as usize] = new;
            // Setting a signal to be ignored drops it if it's pending (POSIX).
            if discarded(t, sig) {
                t.pending &= !bit(sig);
            }
        }
    }
    0
}

/// rt_sigprocmask(how, set, oldset, sigsetsize): change the signal mask.
pub fn sys_rt_sigprocmask(how: u64, set: u64, oldset: u64) -> i64 {
    const SIG_BLOCK: u64 = 0;
    const SIG_UNBLOCK: u64 = 1;
    const SIG_SETMASK: u64 = 2;
    let new = if set != 0 {
        match read_u64(set) {
            Ok(v) => Some(v),
            Err(e) => return -e,
        }
    } else {
        None
    };
    let mut table = TABLE.lock();
    let p = table.get_mut(&current_pid()).unwrap();
    if oldset != 0 {
        if let Err(e) = write_u64(oldset, p.blocked) {
            return -e;
        }
    }
    if let Some(v) = new {
        p.blocked = match how {
            SIG_BLOCK => p.blocked | v,
            SIG_UNBLOCK => p.blocked & !v,
            SIG_SETMASK => v,
            _ => return -EINVAL,
        } & !UNBLOCKABLE;
        // Newly unblocked signals are delivered on the way out of this syscall.
    }
    0
}

/// rt_sigpending(set, sigsetsize): the signals waiting because they're blocked.
pub fn sys_rt_sigpending(set: u64) -> i64 {
    let pending = {
        let table = TABLE.lock();
        let p = &table[&current_pid()];
        p.pending & p.blocked
    };
    match write_u64(set, pending) {
        Ok(()) => 0,
        Err(e) => -e,
    }
}

/// rt_sigtimedwait(set, info, timeout, sigsetsize): wait for one of the
/// signals in `set` (normally blocked) and take it, without running a
/// handler: its number, and its siginfo in `info`. EAGAIN after the
/// timeout, EINTR if another signal interrupts.
pub fn sys_rt_sigtimedwait(set: u64, info: u64, timeout: u64) -> i64 {
    let set = match read_u64(set) {
        Ok(s) => s & !UNBLOCKABLE,
        Err(e) => return -e,
    };
    let deadline = if timeout == 0 {
        None
    } else {
        let Some(ts) = (unsafe { user_slice(timeout, 16) }) else { return -EFAULT };
        let sec = u64::from_le_bytes(ts[0..8].try_into().unwrap());
        let nsec = u64::from_le_bytes(ts[8..16].try_into().unwrap());
        Some(tick_after_ns(sec.saturating_mul(1_000_000_000).saturating_add(nsec)))
    };
    loop {
        let taken = {
            let mut table = TABLE.lock();
            let cur = current_pid();
            let tgid = table[&cur].tgid;
            // Ours, or one the process got before we started waiting.
            let from = [cur, tgid].into_iter().find(|t| table.get(t).is_some_and(|p| p.pending & set != 0));
            match from {
                Some(t) => {
                    let p = table.get_mut(&t).unwrap();
                    let sig = lowest(p.pending & set);
                    p.pending &= !bit(sig);
                    let info = p.siginfo.remove(&sig).unwrap_or(SigInfo::KERNEL);
                    table.get_mut(&cur).unwrap().sigwait = 0;
                    Some((sig, info))
                }
                None => {
                    table.get_mut(&cur).unwrap().sigwait = set;
                    None
                }
            }
        };
        if let Some((sig, why)) = taken {
            if info != 0 {
                let Some(out) = (unsafe { user_slice_mut(info, 128) }) else { return -EFAULT };
                out.copy_from_slice(&why.bytes(sig));
            }
            return sig as i64;
        }
        let stop = |r: i64| {
            with_current(|p| p.sigwait = 0);
            r
        };
        if deadline.is_some_and(|d| ticks() >= d) {
            return stop(-crate::syscall::EAGAIN);
        }
        if signal_pending() {
            return stop(-EINTR);
        }
        match deadline {
            Some(d) => block_on_until(SIGNAL_ONLY, d),
            None => block_on(SIGNAL_ONLY),
        }
    }
}

/// Take a pending signal in `set`: the thread's own, or one sent to its
/// process (signalfd's read).
pub fn take_signal(set: u64) -> Option<i32> {
    let mut table = TABLE.lock();
    let cur = current_pid();
    let tgid = table[&cur].tgid;
    let from = [cur, tgid].into_iter().find(|t| table.get(t).is_some_and(|p| p.pending & set != 0))?;
    let p = table.get_mut(&from).unwrap();
    let sig = lowest(p.pending & set);
    p.pending &= !bit(sig);
    Some(sig)
}

/// Is a signal in `set` pending for this thread or its process?
pub fn signal_waiting(set: u64) -> bool {
    let table = TABLE.lock();
    let cur = current_pid();
    let tgid = table[&cur].tgid;
    [cur, tgid].iter().any(|t| table.get(t).is_some_and(|p| p.pending & set != 0))
}

/// Sleep until a signal in `set` (or one that interrupts) arrives. False
/// if one that interrupts is there (EINTR).
pub fn wait_for_signal(set: u64) -> bool {
    with_current(|p| p.sigwait = set);
    let interrupted = signal_pending();
    if !interrupted {
        block_on(SIGNAL_ONLY);
    }
    with_current(|p| p.sigwait = 0);
    !interrupted
}

/// Never woken by wake(): sigsuspend and pause wait for a signal only.
const SIGNAL_ONLY: u64 = u64::MAX;

/// rt_sigsuspend(mask, sigsetsize): wait for a signal with `mask` in effect
/// (the old mask comes back after the handler). Always returns EINTR.
/// With mask = None, it's pause().
pub fn sys_rt_sigsuspend(mask: Option<u64>) -> i64 {
    let mask = match mask.map(read_u64) {
        Some(Ok(m)) => Some(m),
        Some(Err(e)) => return -e,
        None => None,
    };
    if let Some(m) = mask {
        let mut table = TABLE.lock();
        let p = table.get_mut(&current_pid()).unwrap();
        p.saved_mask = Some(p.blocked);
        p.blocked = m & !UNBLOCKABLE;
    }
    loop {
        if signal_pending() {
            return -EINTR;
        }
        {
            let mut table = TABLE.lock();
            let cur = current_pid();
            if loud_pending_locked(&table, cur) {
                return -EINTR;
            }
            table.get_mut(&cur).unwrap().state = State::Blocked(SIGNAL_ONLY);
        }
        schedule();
    }
}

fn ticks_from(sec: u64, usec: u64) -> u64 {
    (sec.saturating_mul(1_000_000_000).saturating_add(usec.saturating_mul(1000))).div_ceil(TICK_NS)
}

/// The current alarm as (seconds, microseconds) left, and its interval.
fn alarm_left(p: &Process) -> ((u64, u64), (u64, u64)) {
    let split = |t: u64| {
        let ns = t * TICK_NS;
        (ns / 1_000_000_000, (ns % 1_000_000_000) / 1000)
    };
    let left = if p.alarm_at == 0 { 0 } else { p.alarm_at.saturating_sub(ticks()).max(1) };
    (split(left), split(p.alarm_every))
}

/// alarm(seconds): SIGALRM after that long (0 cancels). Returns the seconds
/// left on the previous alarm.
pub fn sys_alarm(secs: u64) -> i64 {
    let mut table = TABLE.lock();
    let p = table.get_mut(&current_pid()).unwrap();
    let ((s, us), _) = alarm_left(p);
    // Rounded to the nearest second, but at least 1.
    let old = if p.alarm_at == 0 { 0 } else { (s + (us >= 500_000) as u64).max(1) };
    p.alarm_at = if secs == 0 { 0 } else { ticks() + ticks_from(secs, 0) };
    p.alarm_every = 0;
    old as i64
}

/// getitimer(which, value) / setitimer(which, new, old): only ITIMER_REAL
/// (SIGALRM) is supported. struct itimerval = { interval, value } timevals.
pub fn sys_itimer(which: u64, new: u64, old: u64) -> i64 {
    if which != 0 {
        return -EINVAL;
    }
    let new = if new != 0 {
        match unsafe { user_slice(new, 32) } {
            Some(b) => {
                let w = |i: usize| u64::from_le_bytes(b[i * 8..i * 8 + 8].try_into().unwrap());
                Some((ticks_from(w(0), w(1)), ticks_from(w(2), w(3))))
            }
            None => return -EFAULT,
        }
    } else {
        None
    };
    let mut table = TABLE.lock();
    let p = table.get_mut(&current_pid()).unwrap();
    if old != 0 {
        let ((vs, vus), (is, ius)) = alarm_left(p);
        let Some(out) = (unsafe { user_slice_mut(old, 32) }) else { return -EFAULT };
        for (i, v) in [is, ius, vs, vus].into_iter().enumerate() {
            out[i * 8..i * 8 + 8].copy_from_slice(&v.to_le_bytes());
        }
    }
    if let Some((interval, value)) = new {
        p.alarm_at = if value == 0 { 0 } else { ticks() + value };
        p.alarm_every = interval;
    }
    0
}

/// nanosleep(req, rem): sleep for at least the requested time (rounded up
/// to timer ticks). A signal ends it early with EINTR.
pub fn sys_nanosleep(req: u64, rem: u64) -> i64 {
    let Some(ts) = (unsafe { user_slice(req, 16) }) else { return -EFAULT };
    let sec = i64::from_le_bytes(ts[0..8].try_into().unwrap());
    let nsec = i64::from_le_bytes(ts[8..16].try_into().unwrap());
    if sec < 0 || !(0..1_000_000_000).contains(&nsec) {
        return -EINVAL;
    }
    let total = (sec as u64).saturating_mul(1_000_000_000).saturating_add(nsec as u64);
    sleep_ns(total, rem)
}

/// clock_nanosleep(clock, flags, req, rem): like nanosleep, on any clock;
/// with TIMER_ABSTIME, `req` is the time to wake up at.
pub fn sys_clock_nanosleep(clock: u64, flags: u64, req: u64, rem: u64) -> i64 {
    const TIMER_ABSTIME: u64 = 1;
    if !matches!(clock, 0 | 1 | 4 | 7) {
        return -EINVAL;
    }
    let Some(ts) = (unsafe { user_slice(req, 16) }) else { return -EFAULT };
    let sec = i64::from_le_bytes(ts[0..8].try_into().unwrap());
    let nsec = i64::from_le_bytes(ts[8..16].try_into().unwrap());
    if sec < 0 || !(0..1_000_000_000).contains(&nsec) {
        return -EINVAL;
    }
    let mut total = (sec as u64).saturating_mul(1_000_000_000).saturating_add(nsec as u64);
    if flags & TIMER_ABSTIME != 0 {
        let now = if clock == 0 { crate::rtc::now_ns() } else { clock_ns() };
        total = total.saturating_sub(now);
        return sleep_ns(total, 0); // nothing "remains" of an absolute time
    }
    sleep_ns(total, rem)
}

/// Sleep `total` nanoseconds (rounded up to ticks); EINTR (with the time
/// left in `rem`, if given) when a signal interrupts it.
fn sleep_ns(total: u64, rem: u64) -> i64 {
    let wake_at = tick_after_ns(total);
    loop {
        if ticks() >= wake_at {
            return 0;
        }
        if signal_pending() {
            break;
        }
        {
            let mut table = TABLE.lock();
            let p = table.get_mut(&current_pid()).unwrap();
            p.wake_at = wake_at;
            p.state = State::Sleeping;
        }
        schedule();
    }
    {
        if rem != 0 {
            if let Some(out) = unsafe { user_slice_mut(rem, 16) } {
                let left = wake_at.saturating_sub(ticks()) * TICK_NS;
                out[0..8].copy_from_slice(&((left / 1_000_000_000) as i64).to_le_bytes());
                out[8..16].copy_from_slice(&((left % 1_000_000_000) as i64).to_le_bytes());
            }
        }
        return -EINTR;
    }
}

/// clock_gettime(clock, tp)
pub fn sys_clock_gettime(clock: u64, tp: u64) -> i64 {
    // REALTIME(_COARSE) is the date (from the CMOS clock); PROCESS/THREAD_
    // CPUTIME the CPU time used; MONOTONIC(_RAW/_COARSE) and BOOTTIME count
    // from boot.
    let ns = match clock {
        0 | 5 => crate::rtc::now_ns(),
        2 | 3 => cpu_time_ns(),
        1 | 4 | 6 | 7 => clock_ns(),
        _ => return -EINVAL,
    };
    let Some(out) = (unsafe { user_slice_mut(tp, 16) }) else { return -EFAULT };
    out[0..8].copy_from_slice(&((ns / 1_000_000_000) as i64).to_le_bytes());
    out[8..16].copy_from_slice(&((ns % 1_000_000_000) as i64).to_le_bytes());
    0
}

// ---- POSIX timers (timer_create, timer_settime...) ----

/// A timer of a process (timer_create): when it expires it sends a signal
/// to the process, or to one of its threads (SIGEV_THREAD_ID: how musl
/// runs SIGEV_THREAD timers, with a thread waiting in sigwaitinfo).
struct PosixTimer {
    tgid: u64,
    id: i32,
    /// SIGEV_SIGNAL (0), SIGEV_NONE (1), SIGEV_THREAD_ID (4).
    notify: i32,
    signo: i32,
    tid: u64,
    /// sigev_value, given back in the siginfo.
    value: u64,
    realtime: bool,
    /// When it expires next (ticks), 0 if disarmed; then every `interval`.
    next: u64,
    interval: u64,
    /// Expirations missed meanwhile (timer_getoverrun).
    overrun: i32,
}

static PTIMERS: Mutex<Vec<PosixTimer>> = Mutex::new(Vec::new());

const SIGEV_SIGNAL: i32 = 0;
const SIGEV_NONE: i32 = 1;
const SIGEV_THREAD_ID: i32 = 4;

/// Is a timer due (so the scheduler should run)?
fn ptimer_due() -> bool {
    let now = ticks();
    PTIMERS.try_lock().is_some_and(|t| t.iter().any(|t| t.next != 0 && now >= t.next))
}

/// Send the signals of the timers that expired (called with the table).
fn fire_posix_timers_locked(table: &mut BTreeMap<u64, Box<Process>>) {
    let now = ticks();
    let Some(mut timers) = PTIMERS.try_lock() else { return };
    timers.retain(|t| table.contains_key(&t.tgid));
    for t in timers.iter_mut().filter(|t| t.next != 0 && now >= t.next) {
        let missed = if t.interval > 0 { ((now - t.next) / t.interval) as i32 } else { 0 };
        t.next = if t.interval > 0 { t.next + (missed as u64 + 1) * t.interval } else { 0 };
        if t.notify == SIGEV_NONE {
            continue;
        }
        let target = if t.notify == SIGEV_THREAD_ID { t.tid } else { t.tgid };
        let pending = table.get(&target).is_some_and(|p| p.pending & bit(t.signo) != 0);
        if pending {
            // Not taken yet: it counts as overrun, as POSIX says.
            t.overrun = t.overrun.saturating_add(1 + missed);
            continue;
        }
        t.overrun = missed;
        let info = SigInfo { code: SI_TIMER, pid: t.id, uid: missed as u32, word: t.value, status: 0 };
        if t.notify == SIGEV_THREAD_ID {
            signal_info_locked(table, target, t.signo, info);
        } else {
            signal_process_info_locked(table, target, t.signo, info);
        }
    }
}


/// A process's timers go when it ends or runs another program.
pub fn drop_posix_timers(tgid: u64) {
    PTIMERS.lock().retain(|t| t.tgid != tgid);
}

fn ns_to_ticks(ns: u64) -> u64 {
    if ns == 0 { 0 } else { ns.div_ceil(TICK_NS).max(1) }
}

fn read_timespec(b: &[u8]) -> Result<u64, i64> {
    let sec = i64::from_le_bytes(b[0..8].try_into().unwrap());
    let nsec = i64::from_le_bytes(b[8..16].try_into().unwrap());
    if sec < 0 || !(0..1_000_000_000).contains(&nsec) {
        return Err(EINVAL);
    }
    Ok((sec as u64).saturating_mul(1_000_000_000).saturating_add(nsec as u64))
}

fn write_timespec(b: &mut [u8], ns: u64) {
    b[0..8].copy_from_slice(&((ns / 1_000_000_000) as i64).to_le_bytes());
    b[8..16].copy_from_slice(&((ns % 1_000_000_000) as i64).to_le_bytes());
}

/// timer_create(clock, sigevent, timerid)
pub fn sys_timer_create(clock: u64, sev: u64, idp: u64) -> i64 {
    let realtime = match clock {
        0 | 5 => true,
        1 | 4 | 6 | 7 => false,
        _ => return -EINVAL,
    };
    let (cur, tgid) = (current_pid(), current_tgid());
    // No sigevent: SIGALRM to the process, with the timer's id as value.
    let (value, signo, notify, tid) = if sev == 0 {
        (u64::MAX, SIGALRM, SIGEV_SIGNAL, 0)
    } else {
        let Some(b) = (unsafe { user_slice(sev, 20) }) else { return -EFAULT };
        (
            u64::from_le_bytes(b[0..8].try_into().unwrap()),
            i32::from_le_bytes(b[8..12].try_into().unwrap()),
            i32::from_le_bytes(b[12..16].try_into().unwrap()),
            i32::from_le_bytes(b[16..20].try_into().unwrap()) as u64,
        )
    };
    match notify {
        SIGEV_NONE => {}
        SIGEV_SIGNAL | SIGEV_THREAD_ID if (1..=MAX_SIGNAL as i32).contains(&signo) => {}
        _ => return -EINVAL,
    }
    if notify == SIGEV_THREAD_ID && !TABLE.lock().get(&tid).is_some_and(|t| t.tgid == tgid) {
        return -EINVAL;
    }
    let _ = cur;
    let mut timers = PTIMERS.lock();
    let id = (0..).find(|i| !timers.iter().any(|t| t.tgid == tgid && t.id == *i)).unwrap();
    let Some(out) = (unsafe { user_slice_mut(idp, 4) }) else { return -EFAULT };
    out.copy_from_slice(&id.to_le_bytes());
    let value = if value == u64::MAX && sev == 0 { id as u64 } else { value };
    timers.push(PosixTimer { tgid, id, notify, signo, tid, value, realtime, next: 0, interval: 0, overrun: 0 });
    0
}

/// timer_settime(timerid, flags, new itimerspec, old itimerspec)
pub fn sys_timer_settime(id: u64, flags: u64, new: u64, old: u64) -> i64 {
    const TIMER_ABSTIME: u64 = 1;
    let Some(b) = (unsafe { user_slice(new, 32) }) else { return -EFAULT };
    let (interval, value) = match (read_timespec(&b[0..16]), read_timespec(&b[16..32])) {
        (Ok(i), Ok(v)) => (i, v),
        _ => return -EINVAL,
    };
    let tgid = current_tgid();
    let now = ticks();
    let mut timers = PTIMERS.lock();
    let Some(t) = timers.iter_mut().find(|t| t.tgid == tgid && t.id == id as i32) else { return -EINVAL };
    if old != 0 {
        let Some(o) = (unsafe { user_slice_mut(old, 32) }) else { return -EFAULT };
        write_timespec(&mut o[0..16], t.interval * TICK_NS);
        write_timespec(&mut o[16..32], if t.next == 0 { 0 } else { t.next.saturating_sub(now).max(1) * TICK_NS });
    }
    t.interval = ns_to_ticks(interval);
    t.next = if value == 0 {
        0
    } else if flags & TIMER_ABSTIME != 0 {
        let now_ns = if t.realtime { crate::rtc::now_ns() } else { now * TICK_NS };
        now + ns_to_ticks(value.saturating_sub(now_ns)).max(1)
    } else {
        now + ns_to_ticks(value)
    };
    t.overrun = 0;
    0
}

/// timer_gettime(timerid, itimerspec)
pub fn sys_timer_gettime(id: u64, curr: u64) -> i64 {
    let tgid = current_tgid();
    let now = ticks();
    let timers = PTIMERS.lock();
    let Some(t) = timers.iter().find(|t| t.tgid == tgid && t.id == id as i32) else { return -EINVAL };
    let Some(o) = (unsafe { user_slice_mut(curr, 32) }) else { return -EFAULT };
    write_timespec(&mut o[0..16], t.interval * TICK_NS);
    write_timespec(&mut o[16..32], if t.next == 0 { 0 } else { t.next.saturating_sub(now).max(1) * TICK_NS });
    0
}

/// timer_getoverrun(timerid)
pub fn sys_timer_getoverrun(id: u64) -> i64 {
    let tgid = current_tgid();
    PTIMERS.lock().iter().find(|t| t.tgid == tgid && t.id == id as i32).map_or(-EINVAL, |t| t.overrun as i64)
}

/// timer_delete(timerid)
pub fn sys_timer_delete(id: u64) -> i64 {
    let tgid = current_tgid();
    let mut timers = PTIMERS.lock();
    let before = timers.len();
    timers.retain(|t| !(t.tgid == tgid && t.id == id as i32));
    if timers.len() == before { -EINVAL } else { 0 }
}

// ---- blocking on events ----

/// Keys to wake up, collected until the scheduler next runs. A queue (with
/// its own lock) instead of waking directly, so it's safe to call from
/// anywhere, including while the process table is locked (e.g. a pipe end
/// closed as a process exits).
static WAKE_QUEUE: Mutex<Vec<u64>> = Mutex::new(Vec::new());

/// What processes in poll() and select() block on: they wait for anything
/// at all to happen (every wake() also wakes them), then check their fds
/// again.
pub const POLL_KEY: u64 = 0x7E7E_0000_504F_4C4C;
/// What processes reading the console block on: the scheduler wakes them
/// when keys arrive (the keyboard interrupt itself can't take locks).
pub const CONSOLE_KEY: u64 = 0x7E7E_0000_434F_4E53;

/// Wake every process blocked on `key` (and everyone in poll or select).
pub fn wake(key: u64) {
    let mut queue = WAKE_QUEUE.lock();
    for k in [key, POLL_KEY] {
        if !queue.contains(&k) {
            queue.push(k);
        }
    }
}

/// Scancode arrivals the scheduler has woken keyboard readers for.
static KEYS_SEEN: AtomicU64 = AtomicU64::new(0);

/// Have keys arrived since the scheduler last woke keyboard readers?
fn new_keys() -> bool {
    crate::task::keyboard::arrivals() != KEYS_SEEN.load(Ordering::SeqCst)
}

/// Sleep until someone calls `wake(key)` (or a signal arrives). Callers
/// re-check their condition afterwards.
pub fn block_on(key: u64) {
    if signal_pending() {
        return; // (this also stops the process here for SIGTSTP & co.)
    }
    {
        let mut table = TABLE.lock();
        let cur = current_pid();
        if loud_pending_locked(&table, cur) {
            return;
        }
        table.get_mut(&cur).unwrap().state = State::Blocked(key);
    }
    schedule();
}

/// Like block_on, but also wake up at tick `deadline` (network waits: TCP
/// timers need the stack polled even when no packet arrives).
pub fn block_on_until(key: u64, deadline: u64) {
    if signal_pending() {
        return;
    }
    {
        let mut table = TABLE.lock();
        let cur = current_pid();
        if loud_pending_locked(&table, cur) {
            return;
        }
        let p = table.get_mut(&cur).unwrap();
        p.state = State::Blocked(key);
        p.deadline = deadline;
    }
    schedule();
    with_current(|p| p.deadline = 0);
}

/// Send a signal to the current process (e.g. SIGPIPE).
pub fn raise(sig: i32) {
    let mut table = TABLE.lock();
    let cur = current_pid();
    signal_locked(&mut table, cur, sig);
}

// ---- information for /proc ----

/// A snapshot of one process, for /proc.
pub struct ProcInfo {
    pub pid: u64,
    /// The process (thread group) this thread belongs to.
    pub tgid: u64,
    pub ppid: u64,
    pub pgid: u64,
    pub name: String,
    pub argv: Vec<String>,
    pub exe: String,
    pub cwd: String,
    /// 'R' running/ready, 'S' sleeping or waiting, 'Z' zombie.
    pub state: char,
    pub start_tick: u64,
    pub cpu_ticks: u64,
    /// Pages of user memory mapped.
    pub pages: u64,
    /// Bytes in the brk heap.
    pub heap: u64,
    /// (fd, description) of each open fd.
    pub fds: Vec<(usize, String)>,
    pub kernel: bool,
    pub cred: Cred,
    pub uid: u32,
    pub gid: u32,
    /// Signals: pending, blocked, ignored, caught (with a handler).
    pub signals: [u64; 4],
}

fn state_letter(s: State) -> char {
    match s {
        State::Running | State::Ready => 'R',
        State::Waiting | State::Sleeping | State::Blocked(_) => 'S',
        State::Stopped => 'T',
        State::Zombie(_) => 'Z',
    }
}

/// Information about every process (or just `only`).
pub fn snapshot(only: Option<u64>) -> Vec<ProcInfo> {
    let now = ticks();
    let cur = current_pid();
    let table = TABLE.lock();
    table
        .values()
        .filter(|p| only.is_none_or(|o| o == p.pid))
        .map(|p| {
            // Its time on a core now (the caller's, and on more cores any
            // process running elsewhere this moment).
            let running = if p.pid == cur || p.state == State::Running { now.saturating_sub(p.running_since) } else { 0 };
            let zombie = matches!(p.state, State::Zombie(_));
            // Lock each once, here: a guard made inside the struct literal
            // below would live until its end, and a second lock() would spin.
            let (cwd, fds) = {
                let files = p.files.lock();
                (files.cwd.clone(), files.describe())
            };
            let brk = p.mm.lock().brk;
            ProcInfo {
                pid: p.pid,
                tgid: p.tgid,
                ppid: p.ppid,
                pgid: p.pgid,
                name: p.name.clone(),
                argv: p.argv.clone(),
                exe: p.exe.clone(),
                cwd,
                state: state_letter(p.state),
                start_tick: p.start_tick,
                cpu_ticks: p.cpu_ticks + running,
                pages: if p.kernel_stack.is_some() && !zombie { memory::count_user_pages(p.p4) } else { 0 },
                heap: brk.saturating_sub(HEAP_BASE),
                fds,
                kernel: p.kernel_stack.is_none(),
                cred: p.cred.clone(),
                uid: p.cred.ruid,
                gid: p.cred.rgid,
                signals: {
                    let set = |f: &dyn Fn(&SigAction) -> bool| {
                        p.actions.iter().enumerate().filter(|(_, a)| f(a)).fold(0u64, |m, (i, _)| m | 1 << i)
                    };
                    [p.pending, p.blocked, set(&|a| a.handler == SIG_IGN), set(&|a| a.handler > SIG_IGN)]
                },
            }
        })
        .collect()
}

/// (processes, ready to run) for /proc/loadavg.
pub fn counts() -> (usize, usize) {
    let table = TABLE.lock();
    let ready = table.values().filter(|p| matches!(p.state, State::Running | State::Ready)).count();
    (table.len(), ready)
}

pub fn last_pid() -> u64 {
    NEXT_PID.load(Ordering::SeqCst).saturating_sub(1)
}

// ---- credentials ----

/// The current process's credentials.
pub fn current_cred() -> Cred {
    TABLE.lock().get(&current_pid()).map(|p| p.cred.clone()).unwrap_or(Cred::ROOT)
}

fn with_cred<T>(f: impl FnOnce(&mut Cred) -> T) -> T {
    let mut table = TABLE.lock();
    f(&mut table.get_mut(&current_pid()).unwrap().cred)
}

/// The IDs a user (or group) value of -1 means "leave unchanged".
const KEEP: u32 = u32::MAX;

/// setresuid(r, e, s) (and the helper for setuid/setreuid): root may set
/// anything; others may only use their current real, effective or saved ID.
fn set_uids(r: u32, e: u32, s: u32) -> i64 {
    with_cred(|c| {
        let ok = |v: u32| v == KEEP || c.euid == 0 || v == c.ruid || v == c.euid || v == c.suid;
        if !(ok(r) && ok(e) && ok(s)) {
            return -EPERM;
        }
        if r != KEEP {
            c.ruid = r;
        }
        if e != KEEP {
            c.euid = e;
        }
        if s != KEEP {
            c.suid = s;
        }
        0
    })
}

fn set_gids(r: u32, e: u32, s: u32) -> i64 {
    with_cred(|c| {
        let ok = |v: u32| v == KEEP || c.euid == 0 || v == c.rgid || v == c.egid || v == c.sgid;
        if !(ok(r) && ok(e) && ok(s)) {
            return -EPERM;
        }
        if r != KEEP {
            c.rgid = r;
        }
        if e != KEEP {
            c.egid = e;
        }
        if s != KEEP {
            c.sgid = s;
        }
        0
    })
}

/// The uid/gid family of syscalls.
pub fn sys_id(num: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    let (u1, u2, u3) = (a1 as u32, a2 as u32, a3 as u32);
    match num {
        SYS_GETUID => current_cred().ruid as i64,
        SYS_GETEUID => current_cred().euid as i64,
        SYS_GETGID => current_cred().rgid as i64,
        SYS_GETEGID => current_cred().egid as i64,
        SYS_SETUID => {
            // root: all three; others: just the effective ID (to r or s).
            if current_cred().euid == 0 {
                set_uids(u1, u1, u1)
            } else {
                set_uids(KEEP, u1, KEEP)
            }
        }
        SYS_SETGID => {
            if current_cred().euid == 0 {
                set_gids(u1, u1, u1)
            } else {
                set_gids(KEEP, u1, KEEP)
            }
        }
        SYS_SETREUID => {
            let c = current_cred();
            // If the real ID is set (or the effective one changes to
            // something other than the real ID), the saved ID follows.
            let saved = if u1 != KEEP || (u2 != KEEP && u2 != c.ruid) { if u2 != KEEP { u2 } else { c.euid } } else { KEEP };
            set_uids(u1, u2, saved)
        }
        SYS_SETREGID => {
            let c = current_cred();
            let saved = if u1 != KEEP || (u2 != KEEP && u2 != c.rgid) { if u2 != KEEP { u2 } else { c.egid } } else { KEEP };
            set_gids(u1, u2, saved)
        }
        SYS_SETRESUID => set_uids(u1, u2, u3),
        SYS_SETRESGID => set_gids(u1, u2, u3),
        SYS_GETRESUID | SYS_GETRESGID => {
            let c = current_cred();
            let vals = if num == SYS_GETRESUID { [c.ruid, c.euid, c.suid] } else { [c.rgid, c.egid, c.sgid] };
            for (ptr, v) in [a1, a2, a3].into_iter().zip(vals) {
                match unsafe { user_slice_mut(ptr, 4) } {
                    Some(out) => out.copy_from_slice(&v.to_le_bytes()),
                    None => return -EFAULT,
                }
            }
            0
        }
        _ => -ENOSYS,
    }
}

/// getgroups(size, list)
pub fn sys_getgroups(size: u64, list: u64) -> i64 {
    let groups = current_cred().groups;
    if size == 0 {
        return groups.len() as i64;
    }
    if (size as usize) < groups.len() {
        return -EINVAL;
    }
    let Some(out) = (unsafe { user_slice_mut(list, groups.len() as u64 * 4) }) else { return -EFAULT };
    for (i, g) in groups.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&g.to_le_bytes());
    }
    groups.len() as i64
}

/// setgroups(size, list): root only.
pub fn sys_setgroups(size: u64, list: u64) -> i64 {
    if current_cred().euid != 0 {
        return -EPERM;
    }
    if size > 64 {
        return -EINVAL;
    }
    let Some(input) = (unsafe { user_slice(list, size * 4) }) else { return -EFAULT };
    let groups: Vec<u32> = input.chunks(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect();
    with_cred(|c| c.groups = groups);
    0
}

/// After execve of a setuid/setgid program: run as its owner/group.
pub fn apply_exec_ids(setuid: Option<u32>, setgid: Option<u32>) {
    with_cred(|c| {
        if let Some(u) = setuid {
            c.euid = u;
        }
        if let Some(g) = setgid {
            c.egid = g;
        }
        // POSIX: the saved IDs become the effective ones.
        c.suid = c.euid;
        c.sgid = c.egid;
    });
}

// ---- thread-local storage ----

fn set_fs_gs(fs: u64, gs: u64) {
    use x86_64::registers::model_specific::Msr;
    unsafe {
        Msr::new(0xC000_0100).write(fs); // FS_BASE
        Msr::new(0xC000_0101).write(gs); // GS_BASE (the kernel never uses GS)
    }
}

/// arch_prctl(code, addr): set or get the FS/GS base. libc points FS at
/// the thread's control block (thread-local variables, errno, ...).
pub fn sys_arch_prctl(code: u64, addr: u64) -> i64 {
    const ARCH_SET_GS: u64 = 0x1001;
    const ARCH_SET_FS: u64 = 0x1002;
    const ARCH_GET_FS: u64 = 0x1003;
    const ARCH_GET_GS: u64 = 0x1004;
    let mut table = TABLE.lock();
    let p = table.get_mut(&current_pid()).unwrap();
    match code {
        ARCH_SET_FS | ARCH_SET_GS => {
            if addr >= memory::USER_SPACE_END {
                return -EPERM;
            }
            if code == ARCH_SET_FS {
                p.fs_base = addr;
            } else {
                p.gs_base = addr;
            }
            set_fs_gs(p.fs_base, p.gs_base);
            0
        }
        ARCH_GET_FS | ARCH_GET_GS => {
            let v = if code == ARCH_GET_FS { p.fs_base } else { p.gs_base };
            drop(table);
            match unsafe { user_slice_mut(addr, 8) } {
                Some(out) => {
                    out.copy_from_slice(&v.to_le_bytes());
                    0
                }
                None => -EFAULT,
            }
        }
        _ => -EINVAL,
    }
}

/// Run `f` on the current process (for mmap.rs).
pub fn with_current<T>(f: impl FnOnce(&mut Process) -> T) -> T {
    let mut table = TABLE.lock();
    f(table.get_mut(&current_pid()).unwrap())
}

/// The current address space.
pub fn current_p4() -> PhysFrame {
    TABLE.lock()[&current_pid()].p4
}

/// times(buf): the process's CPU time (all counted as user time; children
/// not counted) in 1/100 s, and the return value: ticks since boot.
pub fn sys_times(buf: u64) -> i64 {
    let hundredths = |ns: u64| ns / 10_000_000;
    if buf != 0 {
        let Some(out) = (unsafe { user_slice_mut(buf, 32) }) else { return -EFAULT };
        out.fill(0);
        out[0..8].copy_from_slice(&hundredths(cpu_time_ns()).to_le_bytes());
    }
    hundredths(ticks() * TICK_NS) as i64
}

/// CPU time used by the current process, in nanoseconds.
fn cpu_time_ns() -> u64 {
    let table = TABLE.lock();
    let p = &table[&current_pid()];
    (p.cpu_ticks + ticks().saturating_sub(p.running_since)) * TICK_NS
}

/// clock_getres(clock, res): every clock ticks with the timer.
pub fn sys_clock_getres(clock: u64, res: u64) -> i64 {
    if clock > 7 {
        return -EINVAL;
    }
    if res != 0 {
        let Some(out) = (unsafe { user_slice_mut(res, 16) }) else { return -EFAULT };
        out[0..8].copy_from_slice(&0i64.to_le_bytes());
        out[8..16].copy_from_slice(&(TICK_NS as i64).to_le_bytes());
    }
    0
}

/// struct rusage for wait4: the child's CPU time (all counted as user
/// time: the kernel doesn't split it); the other fields are zero.
fn fill_rusage(ptr: u64, cpu_ticks: u64) {
    if let Some(out) = unsafe { user_slice_mut(ptr, 144) } {
        out.fill(0);
        let us = cpu_ticks * TICK_NS / 1000;
        out[0..8].copy_from_slice(&((us / 1_000_000) as i64).to_le_bytes()); // ru_utime.tv_sec
        out[8..16].copy_from_slice(&((us % 1_000_000) as i64).to_le_bytes()); // ru_utime.tv_usec
    }
}

// ---- threads: ids and futexes ----

/// getpid(): the process (thread group) ID.
pub fn current_tgid() -> u64 {
    TABLE.lock().get(&current_pid()).map_or(0, |p| p.tgid)
}

/// How many processes (and threads) exist.
pub fn process_count() -> usize {
    TABLE.lock().len()
}

/// prctl(option, arg2): naming the thread, and the options programs set
/// as a precaution (accepted: they change nothing here).
pub fn sys_prctl(option: u64, arg2: u64) -> i64 {
    const PR_SET_PDEATHSIG: u64 = 1;
    const PR_GET_PDEATHSIG: u64 = 2;
    const PR_GET_DUMPABLE: u64 = 3;
    const PR_SET_DUMPABLE: u64 = 4;
    const PR_SET_NAME: u64 = 15;
    const PR_GET_NAME: u64 = 16;
    const PR_SET_NO_NEW_PRIVS: u64 = 38;
    const PR_GET_NO_NEW_PRIVS: u64 = 39;
    const PR_SET_VMA: u64 = 0x5356_4d41;
    match option {
        PR_SET_NAME => {
            let Some(b) = (unsafe { user_slice(arg2, 16) }) else { return -EFAULT };
            let name = b.split(|&c| c == 0).next().unwrap_or(&[]);
            let name = String::from_utf8_lossy(name).into_owned();
            with_current(|p| p.name = name);
            0
        }
        PR_GET_NAME => {
            let name = with_current(|p| p.name.clone());
            let Some(out) = (unsafe { user_slice_mut(arg2, 16) }) else { return -EFAULT };
            out.fill(0);
            let n = name.len().min(15);
            out[..n].copy_from_slice(&name.as_bytes()[..n]);
            0
        }
        PR_GET_PDEATHSIG => {
            let Some(out) = (unsafe { user_slice_mut(arg2, 4) }) else { return -EFAULT };
            out.fill(0);
            0
        }
        PR_GET_DUMPABLE => 1,
        PR_GET_NO_NEW_PRIVS => 0,
        PR_SET_PDEATHSIG | PR_SET_DUMPABLE | PR_SET_NO_NEW_PRIVS | PR_SET_VMA => 0,
        _ => -EINVAL,
    }
}

/// set_tid_address(addr): zero and futex-wake `addr` when this thread
/// exits. Returns the thread ID.
pub fn sys_set_tid_address(addr: u64) -> i64 {
    with_current(|p| {
        p.clear_child_tid = addr;
        p.pid as i64
    })
}

/// A futex is identified by its address space and address: bit 63 set,
/// bit 62 clear (kernel heap addresses, which other wait keys are, have
/// both set), the address space folded into bits 47-61. (Two address
/// spaces folding to the same bits only cause harmless extra wake-ups.)
/// The key for a futex operation `op` on `addr`. Without FUTEX_PRIVATE_FLAG,
/// on shared memory (mmap MAP_SHARED: a semaphore or lock between
/// processes), it's the physical address, the same for every process
/// mapping it; otherwise the address in this address space.
fn futex_key_op(op: u64, p4: PhysFrame, addr: u64) -> u64 {
    const FUTEX_PRIVATE_FLAG: u64 = 128;
    if op & FUTEX_PRIVATE_FLAG == 0 {
        // (The page may not be there yet in this process: fill it in.)
        let _ = crate::memory::is_user_range(addr, 4, false);
        if let Some((phys, flags)) = crate::memory::translate_user(p4, addr) {
            if flags.contains(crate::memory::SHARED) {
                return (1 << 62) | phys;
            }
        }
    }
    futex_key(p4, addr)
}

fn futex_key(p4: PhysFrame, addr: u64) -> u64 {
    (1 << 63) | (((p4.start_address().as_u64() >> 12) % 0x7FFF) << 47) | (addr & ((1 << 47) - 1))
}

/// Wake up to `max` threads waiting on `key`. Returns how many.
fn futex_wake_locked(table: &mut BTreeMap<u64, Box<Process>>, key: u64, max: u32) -> u32 {
    let mut woken = 0;
    for p in table.values_mut() {
        if woken >= max {
            break;
        }
        if p.state == State::Blocked(key) {
            p.state = State::Ready;
            p.deadline = 0;
            woken += 1;
        }
    }
    woken
}

fn read_u32(addr: u64) -> Result<u32, i64> {
    match unsafe { user_slice(addr, 4) } {
        Some(b) => Ok(u32::from_le_bytes(b.try_into().unwrap())),
        None => Err(EFAULT),
    }
}

/// futex(uaddr, op, val, timeout | val2, uaddr2, val3): what pthread
/// mutexes, condition variables and joins are built on.
///   WAIT (and WAIT_BITSET): sleep if *uaddr == val, until woken (or the
///     timeout: relative for WAIT, absolute for WAIT_BITSET)
///   WAKE (and WAKE_BITSET): wake up to val waiters
///   REQUEUE / CMP_REQUEUE: wake val, move up to val2 others to uaddr2
/// The PRIVATE flag is accepted (every futex is per-process here).
pub fn sys_futex(uaddr: u64, op: u64, val: u64, timeout: u64, uaddr2: u64, val3: u64) -> i64 {
    const WAIT: u64 = 0;
    const WAKE: u64 = 1;
    const REQUEUE: u64 = 3;
    const CMP_REQUEUE: u64 = 4;
    const WAIT_BITSET: u64 = 9;
    const WAKE_BITSET: u64 = 10;
    const CLOCK_REALTIME: u64 = 256;
    const ETIMEDOUT: i64 = 110;
    if uaddr & 3 != 0 {
        return -EINVAL;
    }
    let p4 = current_p4();
    let key = futex_key_op(op, p4, uaddr);
    match op & 0x7f {
        WAIT | WAIT_BITSET => {
            match read_u32(uaddr) {
                Ok(v) if v == val as u32 => {}
                Ok(_) => return -EAGAIN, // it changed: don't sleep
                Err(e) => return -e,
            }
            let deadline = if timeout != 0 {
                let Some(ts) = (unsafe { user_slice(timeout, 16) }) else { return -EFAULT };
                let sec = u64::from_le_bytes(ts[0..8].try_into().unwrap());
                let nsec = u64::from_le_bytes(ts[8..16].try_into().unwrap());
                let mut ns = sec.saturating_mul(1_000_000_000).saturating_add(nsec);
                if op & 0x7f == WAIT_BITSET {
                    // An absolute time on CLOCK_MONOTONIC (or REALTIME).
                    let now = if op & CLOCK_REALTIME != 0 { crate::rtc::now_ns() } else { clock_ns() };
                    if ns <= now {
                        return -ETIMEDOUT;
                    }
                    ns -= now;
                }
                Some(tick_after_ns(ns).max(ticks() + 1))
            } else {
                None
            };
            if signal_pending() {
                return -EINTR;
            }
            {
                let mut table = TABLE.lock();
                let p = table.get_mut(&current_pid()).unwrap();
                p.state = State::Blocked(key);
                p.deadline = deadline.unwrap_or(0);
            }
            schedule();
            with_current(|p| p.deadline = 0);
            if deadline.is_some_and(|d| ticks() >= d) {
                return -ETIMEDOUT;
            }
            if signal_pending() {
                return -EINTR;
            }
            0 // woken (or spuriously: callers check their condition again)
        }
        WAKE | WAKE_BITSET => futex_wake_locked(&mut TABLE.lock(), key, val as u32) as i64,
        REQUEUE | CMP_REQUEUE => {
            if op & 0x7f == CMP_REQUEUE {
                match read_u32(uaddr) {
                    Ok(v) if v == val3 as u32 => {}
                    Ok(_) => return -EAGAIN,
                    Err(e) => return -e,
                }
            }
            if uaddr2 & 3 != 0 {
                return -EINVAL;
            }
            let mut table = TABLE.lock();
            let woken = futex_wake_locked(&mut table, key, val as u32);
            let (to, mut moved) = (futex_key_op(op, p4, uaddr2), 0u64);
            for p in table.values_mut() {
                if moved >= timeout {
                    break; // (the 4th argument is val2 here: how many to move)
                }
                if p.state == State::Blocked(key) {
                    p.state = State::Blocked(to);
                    moved += 1;
                }
            }
            woken as i64 + if op & 0x7f == CMP_REQUEUE { moved as i64 } else { 0 }
        }
        _ => -ENOSYS,
    }
}

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

pub const MAX_PROCESSES: usize = 32;
const KERNEL_STACK_SIZE: usize = 16 * 1024;
const WNOHANG: u64 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitReason {
    Exited(i32),
    Signaled(i32),
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

pub struct Process {
    pub pid: u64,
    pub ppid: u64,
    pub name: String,
    pub state: State,
    /// Saved kernel stack pointer while not running (see context_switch).
    kernel_rsp: u64,
    /// None for task 0, which runs on the boot stack.
    kernel_stack: Option<Box<[u8]>>,
    pub p4: PhysFrame,
    pub files: Files,
    /// Current program break (end of the brk heap), see sys_brk.
    pub brk: u64,
    /// Process group (for Ctrl-C and kill(-pgid)).
    pub pgid: u64,
    /// Signals sent but not yet acted on (bit n-1 = signal n).
    pending: u64,
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
    /// Memory mapped with mmap (start, end), sorted.
    pub maps: Vec<(u64, u64)>,
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
pub const DEFAULT_ENV: &[&str] = &["PATH=/bin", "HOME=/", "SHELL=/bin/sh", "TERM=vga"];

impl Process {
    fn kernel_stack_top(&self) -> Option<u64> {
        let stack = self.kernel_stack.as_ref()?;
        Some((stack.as_ptr() as u64 + stack.len() as u64) & !0xf)
    }
}

/// Boxed so each Process stays at a fixed address: context_switch saves the
/// stack pointer straight into `kernel_rsp` after the table lock is released.
static TABLE: Mutex<BTreeMap<u64, Box<Process>>> = Mutex::new(BTreeMap::new());
static CURRENT: AtomicU64 = AtomicU64::new(0);
static NEXT_PID: AtomicU64 = AtomicU64::new(1);

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
        files: Files::new("/"),
        brk: HEAP_BASE,
        pgid: 0,
        pending: 0,
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
        cpu_ticks: 0,
        running_since: 0,
        cred: Cred::ROOT,
        fpu: Box::new(crate::fpu::FpuState::initial()),
        fs_base: 0,
        gs_base: 0,
        maps: Vec::new(),
        borrowed_vm: false,
    }));
    CURRENT.store(0, Ordering::SeqCst);
}

pub fn current_pid() -> u64 {
    CURRENT.load(Ordering::SeqCst)
}

pub fn current_ppid() -> u64 {
    TABLE.lock().get(&current_pid()).map(|p| p.ppid).unwrap_or(0)
}

/// Run `f` on the current process's open files.
pub fn with_current_files<T>(f: impl FnOnce(&mut Files) -> T) -> T {
    let mut table = TABLE.lock();
    let p = table.get_mut(&current_pid()).expect("no current process");
    f(&mut p.files)
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
    if !WAKE_QUEUE.lock().is_empty() {
        return true;
    }
    let now = ticks();
    TABLE.lock().values().any(|p| {
        (p.alarm_at != 0 && now >= p.alarm_at)
            || (p.pid != cur && (p.state == State::Ready || (p.state == State::Sleeping && now >= p.wake_at)))
    })
}

/// Create a kernel stack for a new process whose first run "returns" to
/// ring 3 through `syscall_return` with the registers in `frame`.
fn new_kernel_stack(frame: &SyscallFrame) -> (Box<[u8]>, u64) {
    let stack = vec![0u8; KERNEL_STACK_SIZE].into_boxed_slice();
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
    // Did we halt while waiting? Then the wait isn't CPU time.
    let mut idled = false;
    interrupts::without_interrupts(|| loop {
        let switch = {
            let mut table = TABLE.lock();
            let cur = current_pid();
            deliver_tty_signals(&mut table);
            fire_alarms_locked(&mut table);
            let now = ticks();
            let woken = core::mem::take(&mut *WAKE_QUEUE.lock());
            for p in table.values_mut() {
                match p.state {
                    State::Sleeping if now >= p.wake_at => p.state = State::Ready,
                    State::Blocked(key) if woken.contains(&key) => p.state = State::Ready,
                    _ => {}
                }
            }
            let next = table
                .range(cur + 1..)
                .chain(table.range(..cur))
                .find(|(_, p)| p.state == State::Ready)
                .map(|(&pid, _)| pid);

            match next {
                None => {
                    // Nothing else to run. Keep going if the current task can
                    // (it may have just been woken: a sleep ended, a signal).
                    if let Some(p) = table.get_mut(&cur) {
                        if matches!(p.state, State::Running | State::Ready) {
                            p.state = State::Running;
                            if idled {
                                p.running_since = now;
                            }
                            return;
                        }
                        if !idled {
                            // About to halt: bill the time so far, not the wait.
                            p.cpu_ticks += now.saturating_sub(p.running_since);
                            idled = true;
                        }
                    }
                    None
                }
                Some(next) => {
                    let current = table.get_mut(&cur).expect("current process missing");
                    if current.state == State::Running {
                        current.state = State::Ready;
                    }
                    if !idled {
                        current.cpu_ticks += now.saturating_sub(current.running_since);
                    }
                    let save: *mut u64 = &mut current.kernel_rsp;
                    crate::fpu::save(&mut current.fpu);

                    let target = table.get_mut(&next).unwrap();
                    crate::fpu::restore(&target.fpu);
                    set_fs_gs(target.fs_base, target.gs_base);
                    target.state = State::Running;
                    target.running_since = now;
                    if let Some(top) = target.kernel_stack_top() {
                        crate::gdt::set_kernel_stack(VirtAddr::new(top));
                    }
                    let (active, flags) = Cr3::read();
                    if active != target.p4 {
                        unsafe { Cr3::write(target.p4, flags) };
                    }
                    CURRENT.store(next, Ordering::SeqCst);
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
            None => {
                // Nothing to run: wait for an interrupt, then look again.
                interrupts::enable_and_hlt();
                interrupts::disable();
            }
        }
    })
}

/// Halt until the next interrupt (waiting for a key), without counting the
/// wait as the current process's CPU time.
pub fn idle_halt() {
    {
        let mut table = TABLE.lock();
        if let Some(p) = table.get_mut(&current_pid()) {
            p.cpu_ticks += ticks().saturating_sub(p.running_since);
        }
    }
    interrupts::enable_and_hlt();
    interrupts::disable();
    let mut table = TABLE.lock();
    if let Some(p) = table.get_mut(&current_pid()) {
        p.running_since = ticks();
    }
}

/// Let other ready processes run (sched_yield).
pub fn yield_now() {
    schedule();
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
    {
        let mut table = TABLE.lock();
        let cur = current_pid();
        assert!(cur != 0, "the kernel task can't exit");
        let ppid = {
            let p = table.get_mut(&cur).unwrap();
            p.state = State::Zombie(reason);
            p.files = Files::empty(); // closes everything (pipe ends included)
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
        signal_locked(&mut table, ppid, SIGCHLD);
        if table.get(&cur).is_some_and(|p| p.borrowed_vm) {
            wake(vfork_key(cur)); // a vfork child: the parent may go on
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

/// Kill the current process with a signal (used by fault handlers).
pub fn kill_current(signal: i32) -> ! {
    exit_current(ExitReason::Signaled(signal))
}

/// Free a reaped process's memory. Its kernel stack is freed when it drops.
fn free_process(p: Box<Process>) {
    if p.kernel_stack.is_some() && !p.borrowed_vm {
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
                .find(|p| matches(p) && matches!(p.state, State::Zombie(_)))
                .map(|p| p.pid);
            match zombie {
                Some(zpid) => table.remove(&zpid),
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
    fork_with(frame, false, 0)
}

/// Key a vfork parent waits on until its child stops using its memory.
fn vfork_key(child: u64) -> u64 {
    0xF0F0_0000_0000_0000 | child
}

/// clone(flags, stack, parent_tid, child_tid, tls): fork, or a vfork
/// (CLONE_VM | CLONE_VFORK, what posix_spawn uses): the child runs in the
/// parent's memory, on `stack`, and the parent waits until it calls execve
/// or exits. Threads (CLONE_VM without CLONE_VFORK, CLONE_THREAD...) aren't
/// supported yet.
pub fn sys_clone(frame: &SyscallFrame, flags: u64, stack: u64) -> i64 {
    const CLONE_VM: u64 = 0x100;
    const CLONE_VFORK: u64 = 0x4000;
    let exit_signal = flags & 0xff;
    let other = flags & !0xff & !(CLONE_VM | CLONE_VFORK);
    if other != 0 || (exit_signal != 0 && exit_signal != SIGCHLD as u64) {
        return -EINVAL;
    }
    let share = flags & CLONE_VM != 0;
    if share && flags & CLONE_VFORK == 0 {
        return -EINVAL; // a thread
    }
    let pid = fork_with(frame, share, stack);
    if pid > 0 && share {
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

/// fork, sharing the address space (vfork) or copying it, with the child
/// on `stack` if it's not 0.
fn fork_with(frame: &SyscallFrame, share: bool, stack: u64) -> i64 {
    // The child gets the FPU registers as they are now.
    let mut fpu = Box::new(crate::fpu::FpuState::initial());
    crate::fpu::save(&mut fpu);
    let (p4, files, name, ppid, brk, pgid, (actions, blocked), argv, exe, cred, (fs_base, gs_base, maps)) = {
        let table = TABLE.lock();
        if table.len() >= MAX_PROCESSES {
            return -EAGAIN;
        }
        let cur = table.get(&current_pid()).unwrap();
        (cur.p4, cur.files.clone(), cur.name.clone(), cur.pid, cur.brk, cur.pgid, (cur.actions.clone(), cur.blocked), cur.argv.clone(), cur.exe.clone(), cur.cred.clone(), (cur.fs_base, cur.gs_base, cur.maps.clone()))
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
        brk, // the heap pages were copied with the rest of the address space
        pgid,
        pending: 0,
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
        cpu_ticks: 0,
        running_since: 0,
        cred,
        fpu,
        fs_base,
        gs_base,
        maps,
        borrowed_vm: share,
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
        _ => ENOEXEC,
    }
}

/// Read an executable for execve. A `#!` script runs its interpreter
/// instead: `#!/bin/sh -x` in /s.sh with argv [s.sh, a] becomes
/// /bin/sh with argv [/bin/sh, -x, /s.sh, a]. Returns (program path,
/// ELF bytes, argv).
fn load_executable(path: &str, argv: Vec<String>) -> Result<(String, Vec<u8>, Vec<String>, Option<crate::mounts::Meta>), i64> {
    let cred = current_cred();
    let check = |path: &str| -> Result<(Vec<u8>, crate::mounts::Meta), i64> {
        let abs = crate::filesystem::normalize(&with_current_files(|f| f.cwd.clone()), path);
        let (_, meta) = crate::mounts::stat(&abs)?;
        if meta.kind == crate::mounts::FileType::Dir {
            return Err(EACCES);
        }
        crate::perm::check(&meta, &cred, crate::perm::EXEC)?; // not executable (for us)
        Ok((vfs::read_whole_file(path)?, meta))
    };
    let (bytes, meta) = check(path)?;
    if crate::elf::is_elf(&bytes) {
        return Ok((path.to_string(), bytes, argv, Some(meta)));
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
    let (interp_bytes, _) = check(interp)?;
    if !crate::elf::is_elf(&interp_bytes) {
        return Err(ENOEXEC); // an interpreter that's a script itself isn't supported
    }
    let mut new_argv = vec![interp.to_string()];
    if let Some(a) = arg.filter(|a| !a.is_empty()) {
        new_argv.push(a.to_string());
    }
    new_argv.push(path.to_string());
    new_argv.extend(argv.into_iter().skip(1));
    // (setuid/setgid bits on scripts are ignored, as on Linux.)
    Ok((interp.to_string(), interp_bytes, new_argv, None))
}

/// execve(path, argv, envp): replace the current program. On success it
/// doesn't return: the syscall "returns" into the new program's entry point.
pub fn execve(frame: &mut SyscallFrame, path: u64, argv: u64, envp: u64) -> i64 {
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
    let old_p4 = {
        let mut table = TABLE.lock();
        let p = table.get_mut(&current_pid()).unwrap();
        p.files.close_on_exec();
        p.name = basename(&path);
        p.argv = argv.clone();
        p.exe = crate::filesystem::normalize(&p.files.cwd, &path);
        p.brk = HEAP_BASE; // the new program starts with an empty heap
        p.maps.clear();
        p.fs_base = 0;
        p.gs_base = 0;
        let borrowed = core::mem::replace(&mut p.borrowed_vm, false);
        // Handlers are gone with the old program: back to the default action
        // (ignored signals stay ignored; the mask and pending signals stay).
        for a in p.actions.iter_mut() {
            if a.handler != SIG_IGN {
                *a = SigAction::DEFAULT;
            }
        }
        (core::mem::replace(&mut p.p4, image.p4), borrowed)
    };
    let (old_p4, borrowed) = old_p4;
    apply_exec_ids(setuid, setgid);
    let (_, flags) = Cr3::read();
    unsafe { Cr3::write(image.p4, flags) };
    crate::fpu::reset();
    set_fs_gs(0, 0);
    if borrowed {
        // A vfork child: the old address space is the parent's. Let it go on.
        wake(vfork_key(current_pid()));
    } else {
        let mut alloc = FRAME_ALLOCATOR.lock();
        memory::free_address_space(old_p4, alloc.as_mut().unwrap());
    }

    *frame = SyscallFrame::new_user(image.entry, image.stack_pointer);
    0 // becomes rax at the new program's entry point
}

/// wait4(pid, wstatus, options, rusage): pid > 0 is one child, -1 any,
/// 0 any in our process group, < -1 any in group -pid. Options: WNOHANG,
/// WUNTRACED (also report stopped children), WCONTINUED (and continued).
pub fn sys_wait4(pid: u64, status_ptr: u64, options: u64) -> i64 {
    const WNOHANG: u64 = 1;
    const WUNTRACED: u64 = 2;
    const WCONTINUED: u64 = 8;
    let pid = pid as i64 as i32 as i64;
    let status_out = if status_ptr != 0 {
        match unsafe { user_slice_mut(status_ptr, 4) } {
            Some(s) => Some(s),
            None => return -EFAULT,
        }
    } else {
        None
    };
    loop {
        if options & WNOHANG == 0 && signal_pending() {
            return -EINTR;
        }
        let found = {
            let mut table = TABLE.lock();
            let cur = current_pid();
            let my_pgid = table.get(&cur).map(|p| p.pgid).unwrap_or(0);
            let matches = |p: &Process| {
                p.ppid == cur
                    && p.pid != cur
                    && match pid {
                        -1 => true,
                        0 => p.pgid == my_pgid,
                        n if n < -1 => p.pgid == (-n) as u64,
                        n => p.pid as i64 == n,
                    }
            };
            if !table.values().any(|p| matches(p)) {
                return -ECHILD;
            }
            if let Some(zpid) = table.values().find(|p| matches(p) && matches!(p.state, State::Zombie(_))).map(|p| p.pid) {
                let p = table.remove(&zpid).unwrap();
                let State::Zombie(reason) = p.state else { unreachable!() };
                drop(table);
                free_process(p);
                Some((zpid, reason.wait_status() as u32))
            } else {
                // A stop (status 0x7f) or continue (0xffff) not yet reported?
                let wanted = |e: u32| (e & 0xff == 0x7f && options & WUNTRACED != 0) || (e == 0xffff && options & WCONTINUED != 0);
                let event = table.values().find(|p| matches(p) && p.wait_event.is_some_and(wanted)).map(|p| p.pid);
                match event {
                    Some(epid) => {
                        let p = table.get_mut(&epid).unwrap();
                        Some((epid, p.wait_event.take().unwrap()))
                    }
                    None if options & WNOHANG != 0 => return 0,
                    None if loud_pending_locked(&table, cur) => return -EINTR,
                    None => {
                        table.get_mut(&cur).unwrap().state = State::Waiting;
                        None
                    }
                }
            }
        };
        match found {
            Some((pid, status)) => {
                if let Some(out) = status_out {
                    out.copy_from_slice(&status.to_le_bytes());
                }
                return pid as i64;
            }
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
    let c = current_cred();
    let ids = usermode::Ids { uid: c.ruid, euid: c.euid, gid: c.rgid, egid: c.egid };
    let image = usermode::build_image(bytes, argv, envp, ids)?;
    let frame = SyscallFrame::new_user(image.entry, image.stack_pointer);
    let (kernel_stack, kernel_rsp) = new_kernel_stack(&frame);
    let pid = NEXT_PID.fetch_add(1, Ordering::SeqCst);
    let cwd = crate::filesystem::CWD.lock().clone();
    let name = argv.first().map(|a| basename(a)).unwrap_or_else(|| "program".to_string());
    let process = Process {
        pid,
        ppid: current_pid(),
        name,
        state: State::Ready,
        kernel_rsp,
        kernel_stack: Some(kernel_stack),
        p4: image.p4,
        files: Files::new(&cwd),
        brk: HEAP_BASE,
        pgid: pid, // leader of a new process group
        pending: 0,
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
        cpu_ticks: 0,
        running_since: 0,
        cred: current_cred(), // programs the kernel starts run as root
        fpu: Box::new(crate::fpu::FpuState::initial()),
        fs_base: 0,
        gs_base: 0,
        maps: Vec::new(),
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
    // to the kernel), until no children remain.
    loop {
        match wait(-1, false) {
            Ok(Some((p, reason))) if p == pid => {
                status = Some(reason);
                // Like a terminal hanging up: whatever it left running gets
                // SIGHUP, so e.g. `sleep 100 &` can't keep the console busy.
                let mut table = TABLE.lock();
                let left: Vec<u64> = table.keys().copied().filter(|&p| p != 0).collect();
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
        (p.p4, p.brk)
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
        while page_addr < old_end {
            let page = Page::<Size4KiB>::containing_address(VirtAddr::new(page_addr));
            if let Ok((frame, flush)) = mapper.unmap(page) {
                flush.flush();
                unsafe { frames.deallocate_frame(frame) };
            }
            page_addr += 4096;
        }
    }
    drop(alloc);

    TABLE.lock().get_mut(&current_pid()).unwrap().brk = addr;
    addr as i64
}

// ---- signals, process groups, time ----

pub const SIGHUP: i32 = 1;
pub const SIGINT: i32 = 2;
pub const SIGQUIT: i32 = 3;
pub const SIGKILL: i32 = 9;
pub const SIGSEGV: i32 = 11;
pub const SIGPIPE: i32 = 13;
pub const SIGALRM: i32 = 14;
pub const SIGCHLD: i32 = 17;
pub const SIGCONT: i32 = 18;
pub const SIGSTOP: i32 = 19;
pub const SIGTSTP: i32 = 20;
pub const SIGTTIN: i32 = 21;
pub const SIGTTOU: i32 = 22;
const MAX_SIGNAL: i32 = 64;

pub const SIG_DFL: u64 = 0;
pub const SIG_IGN: u64 = 1;

// sigaction flags (Linux values)
const SA_NOCLDSTOP: u64 = 1;
const SA_RESTORER: u64 = 0x0400_0000;
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

/// Length of a timer tick: the PIT's default rate is 1193182 Hz / 65536.
pub const TICK_NS: u64 = 54_925_439;

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
    TICKS.fetch_add(1, Ordering::Relaxed);
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
    sig != SIGKILL
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
        signal_locked(table, ppid, SIGCHLD);
    }
}

/// Send `sig` to one process. Returns false if there's no such process.
fn signal_locked(table: &mut BTreeMap<u64, Box<Process>>, pid: u64, sig: i32) -> bool {
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
            let wake = sig == SIGKILL || p.blocked & bit(sig) == 0;
            if wake && matches!(p.state, State::Waiting | State::Sleeping | State::Blocked(_)) {
                p.state = State::Ready; // so it notices
            }
            if sig == SIGKILL && p.state == State::Stopped {
                p.state = State::Ready;
            }
        }
    }
    if continued {
        notify_parent_locked(table, pid, true);
    }
    true
}

fn signal_group_locked(table: &mut BTreeMap<u64, Box<Process>>, pgid: u64, sig: i32) -> usize {
    let members: Vec<u64> = table.values().filter(|p| p.pgid == pgid && p.pid != 0).map(|p| p.pid).collect();
    members.iter().filter(|&&pid| signal_locked(table, pid, sig)).count()
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
    let (sig, act, old_mask) = {
        let mut table = TABLE.lock();
        let p = table.get_mut(&current_pid()).unwrap();
        // signal_pending dropped everything quieter: the lowest deliverable
        // signal is the one to act on.
        let sig = lowest(p.pending & !p.blocked);
        p.pending &= !bit(sig);
        let act = p.actions[(sig - 1) as usize];
        if act.handler == SIG_DFL {
            drop(table);
            exit_current(ExitReason::Signaled(sig));
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
        (sig, act, old_mask)
    };
    if matches!(syscall, Some((num, ret)) if ret == -EINTR && restartable(num)) && act.flags & SA_RESTART != 0 {
        restart(frame);
    }
    // No way back from the handler without a restorer (libc always gives one).
    if act.flags & SA_RESTORER == 0 || crate::signal::setup_frame(frame, sig, act.handler, act.restorer, old_mask).is_err() {
        exit_current(ExitReason::Signaled(SIGSEGV));
    }
}

/// rt_sigreturn(): back from a handler. Restores the registers and signal
/// mask saved in the frame on the user stack. Returns the restored rax.
pub fn sys_rt_sigreturn(frame: &mut SyscallFrame) -> i64 {
    match crate::signal::restore_frame(frame) {
        Ok(mask) => {
            let mut table = TABLE.lock();
            table.get_mut(&current_pid()).unwrap().blocked = mask & !UNBLOCKABLE;
            frame.rax as i64
        }
        Err(()) => exit_current(ExitReason::Signaled(SIGSEGV)),
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
    let targets: Vec<u64> = if pid > 0 {
        table.keys().copied().filter(|&p| p == pid as u64).collect()
    } else if pid == 0 {
        let pgid = table.get(&cur).map(|p| p.pgid).unwrap_or(0);
        table.values().filter(|p| p.pgid == pgid && p.pid != 0).map(|p| p.pid).collect()
    } else if pid == -1 {
        table.keys().copied().filter(|&p| p != 0 && p != cur).collect()
    } else {
        table.values().filter(|p| p.pgid == (-pid) as u64 && p.pid != 0).map(|p| p.pid).collect()
    };
    if targets.is_empty() {
        return -ESRCH;
    }
    let permitted: Vec<u64> = targets.iter().copied().filter(|t| table.get(t).is_some_and(|p| allowed(p))).collect();
    if permitted.is_empty() {
        return -EPERM;
    }
    for t in permitted {
        signal_locked(&mut table, t, sig);
    }
    0
}

/// tgkill(tgid, tid, sig) and tkill(tid, sig): one thread is one process here.
pub fn sys_tkill(tid: u64, sig: u64) -> i64 {
    if tid as i64 <= 0 {
        return -EINVAL;
    }
    sys_kill(tid, sig)
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
        p.actions[(sig - 1) as usize] = new;
        // Setting a signal to be ignored drops it if it's pending (POSIX).
        if discarded(p, sig) {
            p.pending &= !bit(sig);
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
    // Rounded to the nearest second (ticks are ~55 ms), but at least 1.
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
        let now = if clock == 0 { crate::rtc::now_ns() } else { ticks() * TICK_NS };
        total = total.saturating_sub(now);
        return sleep_ns(total, 0); // nothing "remains" of an absolute time
    }
    sleep_ns(total, rem)
}

/// Sleep `total` nanoseconds (rounded up to ticks); EINTR (with the time
/// left in `rem`, if given) when a signal interrupts it.
fn sleep_ns(total: u64, rem: u64) -> i64 {
    let wake_at = ticks() + total.div_ceil(TICK_NS);
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
        1 | 4 | 6 | 7 => ticks() * TICK_NS,
        _ => return -EINVAL,
    };
    let Some(out) = (unsafe { user_slice_mut(tp, 16) }) else { return -EFAULT };
    out[0..8].copy_from_slice(&((ns / 1_000_000_000) as i64).to_le_bytes());
    out[8..16].copy_from_slice(&((ns % 1_000_000_000) as i64).to_le_bytes());
    0
}

// ---- blocking on events ----

/// Keys to wake up, collected until the scheduler next runs. A queue (with
/// its own lock) instead of waking directly, so it's safe to call from
/// anywhere, including while the process table is locked (e.g. a pipe end
/// closed as a process exits).
static WAKE_QUEUE: Mutex<Vec<u64>> = Mutex::new(Vec::new());

/// Wake every process blocked on `key`.
pub fn wake(key: u64) {
    let mut queue = WAKE_QUEUE.lock();
    if !queue.contains(&key) {
        queue.push(key);
    }
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
            let running = if p.pid == cur { now.saturating_sub(p.running_since) } else { 0 };
            let zombie = matches!(p.state, State::Zombie(_));
            ProcInfo {
                pid: p.pid,
                ppid: p.ppid,
                pgid: p.pgid,
                name: p.name.clone(),
                argv: p.argv.clone(),
                exe: p.exe.clone(),
                cwd: p.files.cwd.clone(),
                state: state_letter(p.state),
                start_tick: p.start_tick,
                cpu_ticks: p.cpu_ticks + running,
                pages: if p.kernel_stack.is_some() && !zombie { memory::count_user_pages(p.p4) } else { 0 },
                heap: p.brk.saturating_sub(HEAP_BASE),
                fds: p.files.describe(),
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

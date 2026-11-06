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

/// Is any process other than the current one ready to run?
pub fn others_ready() -> bool {
    let cur = current_pid();
    TABLE.lock().values().any(|p| p.pid != cur && p.state == State::Ready)
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
    interrupts::without_interrupts(|| loop {
        let switch = {
            let mut table = TABLE.lock();
            let cur = current_pid();
            let next = table
                .range(cur + 1..)
                .chain(table.range(..cur))
                .find(|(_, p)| p.state == State::Ready)
                .map(|(&pid, _)| pid);

            match next {
                None => {
                    if table.get(&cur).is_some_and(|p| p.state == State::Running) {
                        return;
                    }
                    None
                }
                Some(next) => {
                    let current = table.get_mut(&cur).expect("current process missing");
                    if current.state == State::Running {
                        current.state = State::Ready;
                    }
                    let save: *mut u64 = &mut current.kernel_rsp;

                    let target = table.get_mut(&next).unwrap();
                    target.state = State::Running;
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
            p.files = Files::new("/"); // closes everything
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
    if p.kernel_stack.is_some() {
        let mut alloc = FRAME_ALLOCATOR.lock();
        memory::free_address_space(p.p4, alloc.as_mut().unwrap());
    }
}

/// Wait for a child of the current process. `pid` is a specific child or -1
/// for any. Returns Ok(None) if `nohang` and no child has exited yet.
pub fn wait(pid: i64, nohang: bool) -> Result<Option<(u64, ExitReason)>, i64> {
    loop {
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
    let (p4, files, name, ppid, brk) = {
        let table = TABLE.lock();
        if table.len() >= MAX_PROCESSES {
            return -EAGAIN;
        }
        let cur = table.get(&current_pid()).unwrap();
        (cur.p4, cur.files.clone(), cur.name.clone(), cur.pid, cur.brk)
    };

    let child_p4 = {
        let mut alloc = FRAME_ALLOCATOR.lock();
        match memory::copy_address_space(p4, alloc.as_mut().unwrap()) {
            Ok(p4) => p4,
            Err(_) => return -ENOMEM,
        }
    };

    let mut child_frame = *frame;
    child_frame.rax = 0; // fork returns 0 in the child
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
    };
    match add_process(child) {
        Ok(pid) => pid as i64,
        Err(e) => {
            let mut alloc = FRAME_ALLOCATOR.lock();
            memory::free_address_space(child_p4, alloc.as_mut().unwrap());
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
    let bytes = match vfs::read_whole_file(&path) {
        Ok(b) => b,
        Err(e) => return -e,
    };
    // Only real executables: a text file must not be run as raw code.
    // (Flat binaries can only be started by the kernel shell, for tests.)
    if !crate::elf::is_elf(&bytes) {
        return -ENOEXEC;
    }

    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let envp_refs: Vec<&str> = envp.iter().map(String::as_str).collect();
    let image = match usermode::build_image(&bytes, &argv_refs, &envp_refs) {
        Ok(image) => image,
        Err(e) => return -load_error_errno(e),
    };

    // Point of no return: switch to the new address space, free the old one.
    let old_p4 = {
        let mut table = TABLE.lock();
        let p = table.get_mut(&current_pid()).unwrap();
        p.name = basename(&path);
        p.brk = HEAP_BASE; // the new program starts with an empty heap
        core::mem::replace(&mut p.p4, image.p4)
    };
    let (_, flags) = Cr3::read();
    unsafe { Cr3::write(image.p4, flags) };
    {
        let mut alloc = FRAME_ALLOCATOR.lock();
        memory::free_address_space(old_p4, alloc.as_mut().unwrap());
    }

    *frame = SyscallFrame::new_user(image.entry, image.stack_pointer);
    0 // becomes rax at the new program's entry point
}

/// wait4(pid, wstatus, options, rusage)
pub fn sys_wait4(pid: u64, status_ptr: u64, options: u64) -> i64 {
    let pid = pid as i64;
    // No process groups yet: 0 and < -1 mean "any child", like -1.
    let filter = if pid > 0 { pid } else { -1 };
    let status_out = if status_ptr != 0 {
        match unsafe { user_slice_mut(status_ptr, 4) } {
            Some(s) => Some(s),
            None => return -EFAULT,
        }
    } else {
        None
    };
    match wait(filter, options & WNOHANG != 0) {
        Ok(Some((pid, reason))) => {
            if let Some(out) = status_out {
                out.copy_from_slice(&reason.wait_status().to_le_bytes());
            }
            pid as i64
        }
        Ok(None) => 0,
        Err(e) => -e,
    }
}

// ---- used by the kernel shell ----

/// Start a program as a child of the kernel, with the kernel shell's
/// working directory. Returns its pid.
pub fn spawn(bytes: &[u8], argv: &[&str], envp: &[&str]) -> Result<u64, &'static str> {
    let image = usermode::build_image(bytes, argv, envp)?;
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
    let pid = spawn(bytes, argv, envp)?;

    let mut status = None;
    // Reap the program, then any orphans it left behind (they were handed
    // to the kernel), until no children remain.
    loop {
        match wait(-1, false) {
            Ok(Some((p, reason))) if p == pid => status = Some(reason),
            Ok(_) => {}
            Err(_) => break, // ECHILD: no children left
        }
    }
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

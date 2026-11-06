// Copyright (c) 2026 tre4surehunter9
//! Signal frames: how a handler is called and how it returns.
//!
//! To run a handler, the kernel saves the interrupted registers on the
//! program's stack and changes the saved rip to the handler. The layout is
//! Linux's x86_64 `rt_sigframe`, so C libraries work unchanged:
//!
//! ```text
//!   rsp ->  return address (the restorer: `mov rax, 15; syscall`)
//!           ucontext  (flags, link, stack, mcontext = the registers, mask)
//!           siginfo   (signal number, code, ...)
//! ```
//!
//! The handler is called as handler(sig, &siginfo, &ucontext). When it
//! returns, the restorer calls rt_sigreturn with rsp pointing at the
//! ucontext, and the kernel puts the registers (and the mask) back.
//! Above the frame is the FPU/SSE state (fxsave format), pointed to by the
//! mcontext's fpstate field; the handler starts with a clean FPU.

use crate::fpu::FpuState;
use crate::syscall::{user_slice, user_slice_mut, SyscallFrame};

/// Size of the fxsave area.
const FP_SIZE: u64 = 512;

const UCONTEXT: usize = 304;
const SIGINFO: usize = 128;
const MCONTEXT: usize = 40; // offset of uc_mcontext in ucontext
const SIGMASK: usize = 296; // offset of uc_sigmask
/// The red zone: 128 bytes below rsp that code may use without moving rsp.
const RED_ZONE: u64 = 128;

/// Registers in the order of struct sigcontext.
fn regs(f: &mut SyscallFrame) -> [&mut u64; 18] {
    [
        &mut f.r8, &mut f.r9, &mut f.r10, &mut f.r11, &mut f.r12, &mut f.r13, &mut f.r14, &mut f.r15, &mut f.rdi,
        &mut f.rsi, &mut f.rbp, &mut f.rbx, &mut f.rdx, &mut f.rax, &mut f.rcx, &mut f.rsp, &mut f.rip, &mut f.rflags,
    ]
}

/// Set up `frame` to call `handler(sig, info, uc)` on the user stack.
pub fn setup_frame(frame: &mut SyscallFrame, sig: i32, handler: u64, restorer: u64, old_mask: u64) -> Result<(), ()> {
    let size = (8 + UCONTEXT + SIGINFO) as u64;
    // The FPU area (64-byte aligned, as Linux does), then the frame below it.
    let fp_at = frame.rsp.checked_sub(RED_ZONE + FP_SIZE).ok_or(())? & !63;
    let sp = fp_at.checked_sub(size).ok_or(())?;
    // As after a `call`: rsp + 8 is 16-byte aligned at the handler's start.
    let base = (sp & !15) - 8;
    let buf = unsafe { user_slice_mut(base, fp_at + FP_SIZE - base) }.ok_or(())?;
    buf.fill(0);
    let mut fpu = FpuState::initial();
    crate::fpu::save(&mut fpu);
    let fp_off = (fp_at - base) as usize;
    buf[fp_off..fp_off + FP_SIZE as usize].copy_from_slice(&fpu.0);
    let put = |buf: &mut [u8], off: usize, v: u64| buf[off..off + 8].copy_from_slice(&v.to_le_bytes());
    put(buf, 0, restorer); // the handler's return address
    let uc = 8;
    buf[uc + 24..uc + 28].copy_from_slice(&2i32.to_le_bytes()); // uc_stack.ss_flags = SS_DISABLE
    let mc = uc + MCONTEXT;
    let mut saved = *frame;
    for (i, r) in regs(&mut saved).into_iter().enumerate() {
        put(buf, mc + i * 8, *r);
    }
    buf[mc + 144..mc + 146].copy_from_slice(&(frame.cs as u16).to_le_bytes());
    buf[mc + 150..mc + 152].copy_from_slice(&(frame.ss as u16).to_le_bytes());
    put(buf, mc + 168, old_mask); // oldmask
    put(buf, mc + 184, fp_at); // fpstate
    put(buf, uc + SIGMASK, old_mask);
    let info = uc + UCONTEXT;
    buf[info..info + 4].copy_from_slice(&sig.to_le_bytes()); // si_signo (si_code 0 = SI_USER)

    frame.rip = handler;
    frame.rsp = base;
    frame.rdi = sig as u64;
    frame.rsi = base + info as u64;
    frame.rdx = base + uc as u64;
    frame.rax = 0;
    frame.rflags &= !(0x100 | 0x400); // clear TF and DF, as the ABI expects
    crate::fpu::reset();
    Ok(())
}

/// rt_sigreturn: put back the registers saved by `setup_frame`. rsp points
/// at the ucontext (the handler's `ret` popped the return address). Returns
/// the mask to restore. The program may have changed the saved registers
/// (that's allowed), but not to anything that would give it privileges.
pub fn restore_frame(frame: &mut SyscallFrame) -> Result<u64, ()> {
    let buf = unsafe { user_slice(frame.rsp, UCONTEXT as u64) }.ok_or(())?;
    let get = |off: usize| u64::from_le_bytes(buf[off..off + 8].try_into().unwrap());
    let (cs, ss) = (frame.cs, frame.ss); // always ring 3 user segments
    for (i, r) in regs(frame).into_iter().enumerate() {
        *r = get(MCONTEXT + i * 8);
    }
    frame.cs = cs;
    frame.ss = ss;
    // Only the arithmetic flags, TF, DF and AC may come from user memory;
    // interrupts stay on and IOPL stays 0.
    const USER_FLAGS: u64 = 0x40DD5;
    frame.rflags = (frame.rflags & USER_FLAGS) | 0x202;
    let mask = get(SIGMASK);
    match get(MCONTEXT + 184) {
        0 => crate::fpu::reset(),
        fp_at => {
            let saved = unsafe { user_slice(fp_at, FP_SIZE) }.ok_or(())?;
            let mut fpu = FpuState::initial();
            fpu.0.copy_from_slice(saved);
            fpu.sanitize();
            crate::fpu::restore(&fpu);
        }
    }
    Ok(mask)
}

// Copyright (c) 2026 tre4surehunter9
//! Floating-point, SSE and AVX registers for user programs.
//!
//! C programs (musl) use SSE registers for floating point, and for fast
//! memcpy/strlen; optimised code uses AVX (256-bit YMM registers) when the
//! CPU has it. The kernel itself is built without SSE, so the registers
//! always hold the running program's values: they're saved at every task
//! switch and restored for the next one.
//!
//! With XSAVE (and AVX) on the CPU: the OS turns it on (CR4.OSXSAVE), XCR0
//! says which state it saves (x87, SSE, AVX), and xsave/xrstor move the
//! lot (832 bytes). Programs check this (xgetbv) before using AVX: without
//! it, AVX instructions are illegal (SIGILL), though CPUID lists them.
//! Without XSAVE: fxsave/fxrstor (x87 and SSE, 512 bytes).

use core::arch::asm;
use core::sync::atomic::{AtomicBool, Ordering};

/// Room for the state: fxsave's 512 bytes, the XSAVE header (64), AVX's
/// upper halves of the YMM registers (256), and spare. 64-byte aligned
/// (XSAVE's rule).
pub const STATE_SIZE: usize = 1024;

#[repr(C, align(64))]
#[derive(Clone)]
pub struct FpuState(pub [u8; STATE_SIZE]);

/// MXCSR's default: all exceptions masked, round to nearest.
const MXCSR_DEFAULT: u32 = 0x1f80;
/// Reserved MXCSR bits make fxrstor fault, so values from user memory are
/// masked (0xffbf is the architectural default mask).
const MXCSR_MASK: u32 = 0xffbf;
/// The state XSAVE handles here: x87 (bit 0), SSE (1), AVX (2).
pub const XFEATURES: u64 = 0b111;
/// Where the XSAVE header starts (XSTATE_BV, then XCOMP_BV and reserved).
const XSAVE_HEADER: usize = 512;

/// XSAVE (with AVX) is on.
static XSAVE: AtomicBool = AtomicBool::new(false);

/// Whether xsave/xrstor are in use (AVX available to programs).
pub fn xsave_on() -> bool {
    XSAVE.load(Ordering::Relaxed)
}

impl FpuState {
    /// The state a new program starts with (as after fninit; AVX zeroed:
    /// XSTATE_BV 0 means "the initial state" for every component).
    pub fn initial() -> Self {
        let mut s = FpuState([0; STATE_SIZE]);
        s.0[0..2].copy_from_slice(&0x037fu16.to_le_bytes()); // x87 control word
        s.0[24..28].copy_from_slice(&MXCSR_DEFAULT.to_le_bytes());
        s
    }

    /// Make a state from user memory (a signal frame) safe to load:
    /// reserved MXCSR bits off, and the XSAVE header only naming state this
    /// kernel saves (else xrstor faults).
    pub fn sanitize(&mut self) {
        let mxcsr = u32::from_le_bytes(self.0[24..28].try_into().unwrap()) & MXCSR_MASK;
        self.0[24..28].copy_from_slice(&mxcsr.to_le_bytes());
        let bv = u64::from_le_bytes(self.0[XSAVE_HEADER..XSAVE_HEADER + 8].try_into().unwrap()) & XFEATURES;
        self.0[XSAVE_HEADER..XSAVE_HEADER + 8].copy_from_slice(&bv.to_le_bytes());
        self.0[XSAVE_HEADER + 8..XSAVE_HEADER + 64].fill(0); // XCOMP_BV (standard form) and reserved
    }
}

/// Turn on SSE for ring 3 (no x87 emulation, fxsave/fxrstor enabled), and
/// XSAVE with AVX if the CPU has them. (Every core: each has its own CR4
/// and XCR0.)
pub fn init() {
    use x86_64::registers::control::{Cr0, Cr0Flags, Cr4, Cr4Flags};
    unsafe {
        let mut cr0 = Cr0::read();
        cr0.remove(Cr0Flags::EMULATE_COPROCESSOR | Cr0Flags::TASK_SWITCHED);
        cr0.insert(Cr0Flags::MONITOR_COPROCESSOR);
        Cr0::write(cr0);
        let mut cr4 = Cr4::read();
        cr4.insert(Cr4Flags::OSFXSR | Cr4Flags::OSXMMEXCPT_ENABLE);
        // XSAVE (CPUID.1:ECX bit 26) and AVX (bit 28): on.
        let ecx = core::arch::x86_64::__cpuid(1).ecx;
        let avx = ecx & (1 << 26) != 0 && ecx & (1 << 28) != 0;
        if avx {
            cr4.insert(Cr4Flags::OSXSAVE);
        }
        Cr4::write(cr4);
        if avx {
            let x = XFEATURES;
            asm!("xsetbv", in("ecx") 0u32, in("eax") x as u32, in("edx") (x >> 32) as u32, options(nostack));
            XSAVE.store(true, Ordering::Relaxed);
        }
        asm!("fninit");
    }
}

/// Save the CPU's current FPU/SSE/AVX registers into `s`.
pub fn save(s: &mut FpuState) {
    unsafe {
        if xsave_on() {
            let x = XFEATURES;
            asm!("xsave64 [{}]", in(reg) s.0.as_mut_ptr(), in("eax") x as u32, in("edx") (x >> 32) as u32, options(nostack));
        } else {
            asm!("fxsave64 [{}]", in(reg) s.0.as_mut_ptr(), options(nostack));
        }
    }
}

/// Load the FPU/SSE/AVX registers from `s`.
pub fn restore(s: &FpuState) {
    unsafe {
        if xsave_on() {
            let x = XFEATURES;
            asm!("xrstor64 [{}]", in(reg) s.0.as_ptr(), in("eax") x as u32, in("edx") (x >> 32) as u32, options(nostack));
        } else {
            asm!("fxrstor64 [{}]", in(reg) s.0.as_ptr(), options(nostack));
        }
    }
}

/// Reset the registers to the initial state (execve, signal handlers).
pub fn reset() {
    restore(&FpuState::initial());
}

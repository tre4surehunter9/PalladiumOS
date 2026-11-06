// Copyright (c) 2026 tre4surehunter9
//! Floating-point and SSE registers for user programs.
//!
//! C programs (musl) use SSE registers for floating point, and for fast
//! memcpy/strlen. The kernel itself is built without SSE, so the registers
//! always hold the running program's values: they're saved at every task
//! switch (fxsave, 512 bytes per process) and restored for the next one.

use core::arch::asm;

/// The fxsave area: x87 + SSE state, 16-byte aligned.
#[repr(C, align(16))]
#[derive(Clone)]
pub struct FpuState(pub [u8; 512]);

/// MXCSR's default: all exceptions masked, round to nearest.
const MXCSR_DEFAULT: u32 = 0x1f80;
/// Reserved MXCSR bits make fxrstor fault, so values from user memory are
/// masked (0xffbf is the architectural default mask).
const MXCSR_MASK: u32 = 0xffbf;

impl FpuState {
    /// The state a new program starts with (as after fninit).
    pub fn initial() -> Self {
        let mut s = FpuState([0; 512]);
        s.0[0..2].copy_from_slice(&0x037fu16.to_le_bytes()); // x87 control word
        s.0[24..28].copy_from_slice(&MXCSR_DEFAULT.to_le_bytes());
        s
    }

    /// Make a state from user memory (a signal frame) safe to load.
    pub fn sanitize(&mut self) {
        let mxcsr = u32::from_le_bytes(self.0[24..28].try_into().unwrap()) & MXCSR_MASK;
        self.0[24..28].copy_from_slice(&mxcsr.to_le_bytes());
    }
}

/// Turn on SSE for ring 3: no x87 emulation, and fxsave/fxrstor enabled.
pub fn init() {
    use x86_64::registers::control::{Cr0, Cr0Flags, Cr4, Cr4Flags};
    unsafe {
        let mut cr0 = Cr0::read();
        cr0.remove(Cr0Flags::EMULATE_COPROCESSOR | Cr0Flags::TASK_SWITCHED);
        cr0.insert(Cr0Flags::MONITOR_COPROCESSOR);
        Cr0::write(cr0);
        let mut cr4 = Cr4::read();
        cr4.insert(Cr4Flags::OSFXSR | Cr4Flags::OSXMMEXCPT_ENABLE);
        Cr4::write(cr4);
        asm!("fninit");
    }
}

/// Save the CPU's current FPU/SSE registers into `s`.
pub fn save(s: &mut FpuState) {
    unsafe { asm!("fxsave64 [{}]", in(reg) s.0.as_mut_ptr(), options(nostack)) };
}

/// Load the FPU/SSE registers from `s`.
pub fn restore(s: &FpuState) {
    unsafe { asm!("fxrstor64 [{}]", in(reg) s.0.as_ptr(), options(nostack)) };
}

/// Reset the registers to the initial state (execve, signal handlers).
pub fn reset() {
    restore(&FpuState::initial());
}

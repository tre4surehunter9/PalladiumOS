// Copyright (c) 2026 tre4surehunter9

use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame};
use crate::println;
use lazy_static::lazy_static;

lazy_static! {
    static ref IDT: InterruptDescriptorTable = {
    let mut idt = InterruptDescriptorTable::new();
    unsafe {
        // Exceptions programs cause: they become signals (exception_dispatch).
        // int3 may be used from ring 3 (debuggers' breakpoints).
        idt.divide_error.set_handler_addr(x86_64::VirtAddr::new(exc_divide_entry as *const () as u64));
        idt.breakpoint.set_handler_addr(x86_64::VirtAddr::new(exc_breakpoint_entry as *const () as u64))
            .set_privilege_level(x86_64::PrivilegeLevel::Ring3);
        idt.invalid_opcode.set_handler_addr(x86_64::VirtAddr::new(exc_opcode_entry as *const () as u64));
        idt.general_protection_fault.set_handler_addr(x86_64::VirtAddr::new(exc_gp_entry as *const () as u64));
        idt.page_fault.set_handler_addr(x86_64::VirtAddr::new(exc_page_entry as *const () as u64));
        idt.x87_floating_point.set_handler_addr(x86_64::VirtAddr::new(exc_x87_entry as *const () as u64));
        idt.simd_floating_point.set_handler_addr(x86_64::VirtAddr::new(exc_simd_entry as *const () as u64));
    }
    unsafe {
        idt.double_fault.set_handler_fn(double_fault_handler)
        .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
    }
    unsafe {
        idt[InterruptIndex::Timer.as_usize()]
            .set_handler_addr(x86_64::VirtAddr::new(timer_entry as *const () as u64));
    }
    idt[InterruptIndex::Keyboard.as_usize()]
        .set_handler_fn(keyboard_interrupt_handler);
    // PCI cards (the network card) on the lines QEMU routes them to.
    idt[(PIC_1_OFFSET + 9) as usize].set_handler_fn(pci_irq9);
    idt[(PIC_1_OFFSET + 10) as usize].set_handler_fn(pci_irq10);
    idt[(PIC_1_OFFSET + 11) as usize].set_handler_fn(pci_irq11);
    idt[(PIC_1_OFFSET + 12) as usize].set_handler_fn(mouse_irq12);
    idt[crate::smp::SPURIOUS_VECTOR as usize].set_handler_fn(spurious_interrupt);
    idt[crate::smp::TLB_VECTOR as usize].set_handler_fn(tlb_interrupt);
    unsafe {
        idt[crate::smp::AP_TIMER_VECTOR as usize]
            .set_handler_addr(x86_64::VirtAddr::new(ap_timer_entry as *const () as u64));
    }

    crate::syscall::init_syscalls(&mut idt);

    idt

   };
}

use crate::hlt_loop;


// The timer saves every register in a SyscallFrame (like a syscall), so a
// signal handler can be started for a program interrupted in the middle of
// its work: the frame is what the program resumes with. It returns through
// the same code as syscalls.
core::arch::global_asm!(
    ".global timer_entry",
    "timer_entry:",
    "push rax", "push rcx", "push rdx", "push rsi", "push rdi", "push r8", "push r9", "push r10",
    "push r11", "push rbx", "push rbp", "push r12", "push r13", "push r14", "push r15",
    "cld",
    "mov rdi, rsp",
    "call timer_dispatch",
    "jmp syscall_return",
);

unsafe extern "C" {
    fn timer_entry();
}

#[unsafe(no_mangle)]
extern "C" fn timer_dispatch(frame: &mut crate::syscall::SyscallFrame) {
    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Timer.as_u8());
    }
    // Time goes on even while another core has the kernel.
    crate::process::tick();
    // The kernel: taken here, given back on the way out (syscall_return,
    // when going back to a program; below otherwise).
    let took = crate::smp::lock_kernel();
    crate::process::sample_load_if_due();
    crate::xhci::poll(); // USB keyboards
    // Preemptive multitasking: only when we interrupted a user program, never
    // in the middle of kernel code (which may hold locks). Signals (Ctrl-C,
    // alarms) reach busy programs here too.
    if crate::usermode::from_user(frame.cs) {
        crate::net::poll_if_due();
        crate::process::preempt();
        crate::process::handle_signals(frame, None);
    } else if took {
        crate::smp::unlock_kernel(); // it interrupted the idle loop
    }
}

// The other cores' timer (their local APIC's), like the PIT's for core 0:
// the same frame, and back through syscall_return.
core::arch::global_asm!(
    ".global ap_timer_entry",
    "ap_timer_entry:",
    "push rax", "push rcx", "push rdx", "push rsi", "push rdi", "push r8", "push r9", "push r10",
    "push r11", "push rbx", "push rbp", "push r12", "push r13", "push r14", "push r15",
    "cld",
    "mov rdi, rsp",
    "call ap_timer_dispatch",
    "jmp syscall_return",
);

unsafe extern "C" {
    fn ap_timer_entry();
}

#[unsafe(no_mangle)]
extern "C" fn ap_timer_dispatch(frame: &mut crate::syscall::SyscallFrame) {
    crate::smp::lapic_eoi();
    let took = crate::smp::lock_kernel();
    if crate::usermode::from_user(frame.cs) {
        // A program ran its time: let another have this core.
        crate::process::preempt();
        crate::process::handle_signals(frame, None);
    } else if took {
        crate::smp::unlock_kernel();
    }
}

extern "x86-interrupt" fn keyboard_interrupt_handler(
    _stack_frame: InterruptStackFrame
) {
    use x86_64::instructions::port::Port;
    let _k = crate::smp::enter();

    let mut port = Port::new(0x60);
    let scancode: u8 = unsafe { port.read() };
    // Ctrl-C becomes SIGINT (and isn't passed on as a key).
    if !crate::tty::intercept_scancode(scancode) {
        crate::task::keyboard::add_scancode(scancode);
    }

    unsafe {
        PICS.lock()
        .notify_end_of_interrupt(InterruptIndex::Keyboard.as_u8());
    }
}


pub fn init_idt() {
    IDT.load();
}

/// The same IDT on another CPU core.
pub fn load_idt() {
    IDT.load();
}

/// A local APIC's spurious interrupt: nothing to do (and no EOI).
extern "x86-interrupt" fn spurious_interrupt(_f: InterruptStackFrame) {}

/// Another core changed an address space this core is running: flush the
/// TLB (smp::shootdown). No kernel lock: that core holds it, waiting.
extern "x86-interrupt" fn tlb_interrupt(_f: InterruptStackFrame) {
    crate::smp::tlb_interrupt();
}

/// Let pending interrupts in for a moment. System calls run with interrupts
/// off, so long work inside one (reading many disk sectors, filling many
/// pages) would hold off the keyboard; its controller buffers only a few
/// keys, and the rest would be lost. Called between units of such work,
/// never with a lock an interrupt handler takes (PICS, the screen) held.
/// Does nothing if interrupts are on already.
pub fn let_in() {
    if !x86_64::instructions::interrupts::are_enabled() {
        x86_64::instructions::interrupts::enable();
        core::hint::spin_loop(); // (sti takes effect after one more instruction)
        x86_64::instructions::interrupts::disable();
    }
}

/// A PCI interrupt: if it's the network card's line, tell the network
/// stack (which acknowledges the card); then end the interrupt.
fn pci_irq(line: u8) {
    let _k = crate::smp::enter();
    if crate::net::card_irq() == line {
        crate::net::interrupt();
    }
    unsafe { PICS.lock().notify_end_of_interrupt(PIC_1_OFFSET + line) };
}

extern "x86-interrupt" fn pci_irq9(_f: InterruptStackFrame) {
    pci_irq(9);
}
extern "x86-interrupt" fn pci_irq10(_f: InterruptStackFrame) {
    pci_irq(10);
}
extern "x86-interrupt" fn pci_irq11(_f: InterruptStackFrame) {
    pci_irq(11);
}

/// Turn off the CPU's local APIC, so interrupts come straight from the 8259
/// PICs (the timer, keyboard, network card): the classic PC setup that the
/// kernel's interrupt code uses. A BIOS leaves the APIC passing PIC
/// interrupts through, but UEFI firmware (OVMF) leaves it blocking them,
/// and nothing would ever interrupt.
pub fn disable_local_apic() {
    use x86_64::registers::model_specific::Msr;
    const IA32_APIC_BASE: u32 = 0x1B;
    const GLOBAL_ENABLE: u64 = 1 << 11;
    const X2APIC: u64 = 1 << 10;
    let mut msr = Msr::new(IA32_APIC_BASE);
    unsafe {
        let base = msr.read();
        if base & GLOBAL_ENABLE != 0 {
            // Both bits together: firmware may leave the APIC in x2APIC
            // mode, and "x2APIC on, APIC off" is an invalid state (#GP).
            msr.write(base & !(GLOBAL_ENABLE | X2APIC));
        }
    }
}

/// The PIT's divisor: 1193182 Hz / 11932 = 100 Hz, a tick every 10 ms
/// (process::TICK_NS), fine enough for sound and sleeps.
pub const PIT_DIVISOR: u16 = 11932;

/// Program the PIT's channel 0 for the timer interrupt: a square wave at
/// 100 Hz (PIT_DIVISOR). (A BIOS leaves it at 18.2 Hz; UEFI firmware may not
/// have it running at all.)
pub fn start_timer() {
    use x86_64::instructions::port::Port;
    unsafe {
        Port::<u8>::new(0x43).write(0x36); // channel 0, low then high byte, mode 3
        Port::<u8>::new(0x40).write(PIT_DIVISOR as u8);
        Port::<u8>::new(0x40).write((PIT_DIVISOR >> 8) as u8);
    }
}

/// Let interrupt line `irq` through the PICs (and the cascade, for 8-15).
/// The PS/2 mouse (IRQ 12): a byte from it.
extern "x86-interrupt" fn mouse_irq12(_f: InterruptStackFrame) {
    let _k = crate::smp::enter();
    let byte = unsafe { x86_64::instructions::port::Port::<u8>::new(0x60).read() };
    crate::mouse::interrupt(byte);
    unsafe { PICS.lock().notify_end_of_interrupt(PIC_1_OFFSET + 12) };
}

pub fn unmask_irq(irq: u8) {
    if !(9..=12).contains(&irq) {
        return; // no handler for other lines
    }
    x86_64::instructions::interrupts::without_interrupts(|| unsafe {
        let mut pics = PICS.lock();
        let [m1, m2] = pics.read_masks();
        pics.write_masks(m1 & !(1 << 2), m2 & !(1 << (irq - 8)));
    });
}

pub extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame, _error_code: u64) -> !
{
    panic!("EXCEPTION: DOUBLE FAULT\n{:#?}", stack_frame);
}

// The exceptions a program can cause (divide error, breakpoint, invalid
// opcode, general protection, page fault, x87 and SSE errors) save every
// register in a SyscallFrame, as the timer does: a fault becomes a signal
// whose handler gets the program's registers (and may go back to them).
// Some exceptions push an error code: swapped with rax, its slot becomes
// rax's in the frame. They return through syscall_return.
macro_rules! exception_entry {
    ($name:literal, $vector:literal, err) => {
        core::arch::global_asm!(
            concat!(".global ", $name), concat!($name, ":"),
            "xchg rax, [rsp]",
            "push rcx", "push rdx", "push rsi", "push rdi", "push r8", "push r9", "push r10",
            "push r11", "push rbx", "push rbp", "push r12", "push r13", "push r14", "push r15",
            "cld", "mov rdi, rsp", "mov rsi, rax", concat!("mov rdx, ", $vector),
            "call exception_dispatch",
            "jmp syscall_return",
        );
    };
    ($name:literal, $vector:literal) => {
        core::arch::global_asm!(
            concat!(".global ", $name), concat!($name, ":"),
            "push rax",
            "push rcx", "push rdx", "push rsi", "push rdi", "push r8", "push r9", "push r10",
            "push r11", "push rbx", "push rbp", "push r12", "push r13", "push r14", "push r15",
            "cld", "mov rdi, rsp", "xor esi, esi", concat!("mov rdx, ", $vector),
            "call exception_dispatch",
            "jmp syscall_return",
        );
    };
}
exception_entry!("exc_divide_entry", 0);
exception_entry!("exc_breakpoint_entry", 3);
exception_entry!("exc_opcode_entry", 6);
exception_entry!("exc_gp_entry", 13, err);
exception_entry!("exc_page_entry", 14, err);
exception_entry!("exc_x87_entry", 16);
exception_entry!("exc_simd_entry", 19);

unsafe extern "C" {
    fn exc_divide_entry();
    fn exc_breakpoint_entry();
    fn exc_opcode_entry();
    fn exc_gp_entry();
    fn exc_page_entry();
    fn exc_x87_entry();
    fn exc_simd_entry();
}

/// A program's fault: its handler, if it has one; else it dies (and the
/// kernel log says so, as Linux's "segfault at").
fn user_fault(frame: &mut crate::syscall::SyscallFrame, sig: i32, info: crate::process::SigInfo, what: core::fmt::Arguments) {
    if crate::process::fault_signal(frame, sig, info) {
        return;
    }
    println!("{} (rip {:#x})", what, frame.rip);
    // A call through a null (or tiny) function pointer: the return address
    // on top of the stack says where from.
    let from = if frame.rip < 0x1000 && crate::memory::is_user_range(frame.rsp, 8, false) {
        Some(unsafe { core::ptr::read_volatile(frame.rsp as *const u64) })
    } else {
        None
    };
    match from {
        Some(ret) => crate::boot_warn!("pid {}: {} ip {:#x} sp {:#x} cpu {} (called from {:#x})",
            crate::process::current_pid(), what, frame.rip, frame.rsp, crate::smp::cpu_index(), ret),
        None => crate::boot_warn!("pid {}: {} ip {:#x} sp {:#x} cpu {}", crate::process::current_pid(), what, frame.rip,
            frame.rsp, crate::smp::cpu_index()),
    }
    crate::process::kill_current(sig);
}

/// The SIGFPE code for an SSE exception: the first unmasked flag set in
/// MXCSR (x87: in its status word).
fn fpe_code(flags: u32, masks: u32) -> i32 {
    use crate::process::{FPE_FLTDIV, FPE_FLTINV, FPE_FLTOVF, FPE_FLTRES, FPE_FLTUND};
    let raised = flags & !masks & 0x3f;
    if raised & 0x01 != 0 { FPE_FLTINV }
    else if raised & 0x04 != 0 { FPE_FLTDIV }
    else if raised & 0x08 != 0 { FPE_FLTOVF }
    else if raised & 0x12 != 0 { FPE_FLTUND }
    else if raised & 0x20 != 0 { FPE_FLTRES }
    else { FPE_FLTINV }
}

#[unsafe(no_mangle)]
extern "C" fn exception_dispatch(frame: &mut crate::syscall::SyscallFrame, error: u64, vector: u64) {
    use crate::process::{SigInfo, SEGV_ACCERR, SEGV_MAPERR, SI_KERNEL};
    use x86_64::registers::control::Cr2;
    let addr = if vector == 14 { Cr2::read().as_u64() } else { 0 };
    let took = crate::smp::lock_kernel();
    let user = crate::usermode::from_user(frame.cs);
    match vector {
        14 => {
            let present = error & 1 != 0;
            let write = error & 2 != 0;
            // A write to a page shared copy-on-write after fork: copy it,
            // retry. (Also when the kernel writes for a program; it normally
            // breaks the sharing first, in is_user_range.)
            if present && write && addr < crate::memory::USER_SPACE_END && crate::memory::break_cow_current(addr) {
                return finish(took, user);
            }
            if user {
                // Another thread on another core got there first (mapped the
                // page, or broke its copy-on-write): just retry.
                if addr < crate::memory::USER_SPACE_END && crate::memory::fault_already_handled(addr, error) {
                    return finish(took, user);
                }
                // The stack growing, or mmap'd memory touched for the first
                // time: give it a page, retry.
                if !present && (crate::usermode::handle_stack_fault(addr) || crate::mmap::fault_in(addr)) {
                    return finish(took, user);
                }
                let code = if present || crate::mmap::mapped(addr) { SEGV_ACCERR } else { SEGV_MAPERR };
                user_fault(frame, crate::process::SIGSEGV, SigInfo::fault(code, addr),
                    format_args!("segfault at {:#x} error {:#x}", addr, error));
                return finish(took, user);
            }
            crate::serial_println!(
                "EXCEPTION: PAGE FAULT at {:#x}, error {:#x}, cpu {}, pid {}, rip {:#x}, rsp {:#x}",
                addr, error, crate::smp::cpu_index(), crate::process::current_pid(), frame.rip, frame.rsp
            );
            println!("EXCEPTION: PAGE FAULT");
            println!("Accessed Address: {:#x}", addr);
            println!("Error Code: {:#x}, rip {:#x}", error, frame.rip);
            hlt_loop();
        }
        13 if user => user_fault(frame, crate::process::SIGSEGV, SigInfo { code: SI_KERNEL, ..SigInfo::default() },
            format_args!("general protection fault")),
        6 if user => user_fault(frame, crate::process::SIGILL, SigInfo::fault(crate::process::ILL_ILLOPN, frame.rip),
            format_args!("illegal instruction")),
        0 if user => user_fault(frame, crate::process::SIGFPE, SigInfo::fault(crate::process::FPE_INTDIV, frame.rip),
            format_args!("divide error")),
        16 | 19 if user => {
            let code = if vector == 19 {
                let mut mxcsr = 0u32;
                unsafe { core::arch::asm!("stmxcsr [{}]", in(reg) &mut mxcsr, options(nostack)) };
                fpe_code(mxcsr, (mxcsr >> 7) & 0x3f)
            } else {
                let (mut sw, mut cw) = (0u16, 0u16);
                unsafe { core::arch::asm!("fnstsw [{}]", "fnstcw [{}]", in(reg) &mut sw, in(reg) &mut cw, options(nostack)) };
                fpe_code(sw as u32, cw as u32)
            };
            user_fault(frame, crate::process::SIGFPE, SigInfo::fault(code, frame.rip), format_args!("floating-point error"));
        }
        3 if user => user_fault(frame, crate::process::SIGTRAP, SigInfo { code: SI_KERNEL, ..SigInfo::default() },
            format_args!("breakpoint (int3)")),
        3 => println!("EXCEPTION: BREAKPOINT at {:#x}", frame.rip),
        _ => panic!("EXCEPTION {} (error {:#x}) in the kernel at rip {:#x}, rsp {:#x}", vector, error, frame.rip, frame.rsp),
    }
    finish(took, user)
}

/// Back to where the exception happened: a program lets the big kernel lock
/// go on the way (syscall_return); kernel code keeps what it had.
fn finish(took: bool, user: bool) {
    if took && !user {
        crate::smp::unlock_kernel();
    }
}

// Signal numbers (Linux), used for "killed by signal" exit statuses.

use crate::gdt;

use pic8259::ChainedPics;
use spin;

pub const PIC_1_OFFSET: u8 = 32;
pub const PIC_2_OFFSET: u8 = PIC_1_OFFSET + 8;

pub static PICS: spin::Mutex<ChainedPics> =
    spin::Mutex::new(unsafe { ChainedPics::new(PIC_1_OFFSET, PIC_2_OFFSET ) });

    #[derive(Debug, Clone, Copy)]
    #[repr(u8)]
    pub enum InterruptIndex {
        Timer = PIC_1_OFFSET,
        Keyboard,
    }

    impl InterruptIndex {
        fn as_u8(self) -> u8 {
            self as u8
        }

        fn as_usize(self) -> usize {
            usize::from(self.as_u8())
        }
    }




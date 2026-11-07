// Copyright (c) 2026 tre4surehunter9

use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame};
use crate::println;
use crate::print;
use lazy_static::lazy_static;

lazy_static! {
    static ref IDT: InterruptDescriptorTable = {
    let mut idt = InterruptDescriptorTable::new();
    idt.breakpoint.set_handler_fn(breakpoint_handler);
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
    idt.page_fault.set_handler_fn(page_fault_handler);
    idt.general_protection_fault.set_handler_fn(general_protection_fault_handler);
    idt.invalid_opcode.set_handler_fn(invalid_opcode_handler);

    crate::syscall::init_syscalls(&mut idt);

    idt

   };
}

use x86_64::structures::idt::PageFaultErrorCode;
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
    crate::process::tick();
    // Preemptive multitasking: only when we interrupted a user program, never
    // in the middle of kernel code (which may hold locks). Signals (Ctrl-C,
    // alarms) reach busy programs here too.
    if crate::usermode::from_user(frame.cs) {
        crate::net::poll_if_due();
        crate::process::preempt();
        crate::process::handle_signals(frame, None);
    }
}

extern "x86-interrupt" fn keyboard_interrupt_handler(
    _stack_frame: InterruptStackFrame
) {
    use x86_64::instructions::port::Port;

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

/// Let interrupt line `irq` through the PICs (and the cascade, for 8-15).
pub fn unmask_irq(irq: u8) {
    if !(9..=11).contains(&irq) {
        return; // no handler for other lines
    }
    x86_64::instructions::interrupts::without_interrupts(|| unsafe {
        let mut pics = PICS.lock();
        let [m1, m2] = pics.read_masks();
        pics.write_masks(m1 & !(1 << 2), m2 & !(1 << (irq - 8)));
    });
}

pub extern "x86-interrupt" fn breakpoint_handler(
    stack_frame: InterruptStackFrame)
{
    println!("EXCEPTION: BREAKPOINT\n{:#?}", stack_frame);
}

pub extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame, _error_code: u64) -> !
{
    panic!("EXCEPTION: DOUBLE FAULT\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    use x86_64::registers::control::Cr2;

    let addr = Cr2::read().as_u64();
    // A write to a page shared copy-on-write after fork: copy it, retry.
    // (Also when the kernel writes for a program; it normally breaks the
    // sharing first, in is_user_range.)
    if error_code.contains(PageFaultErrorCode::PROTECTION_VIOLATION | PageFaultErrorCode::CAUSED_BY_WRITE)
        && addr < crate::memory::USER_SPACE_END
        && crate::memory::break_cow_current(addr)
    {
        return;
    }
    if crate::usermode::from_user(stack_frame.code_segment) {
        // The stack growing into a page it hasn't used yet: map it, retry.
        if !error_code.contains(PageFaultErrorCode::PROTECTION_VIOLATION)
            && crate::usermode::handle_stack_fault(addr)
        {
            return;
        }
        println!("Segmentation fault (address {:?}, rip {:?})",
                 Cr2::read(), stack_frame.instruction_pointer);
        crate::process::kill_current(SIGSEGV);
    }

    println!("EXCEPTION: PAGE FAULT");
    println!("Accessed Address: {:?}", Cr2::read());
    println!("Error Code: {:?}", error_code);
    println!("{:#?}", stack_frame);
    hlt_loop();
}

extern "x86-interrupt" fn general_protection_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    if crate::usermode::from_user(stack_frame.code_segment) {
        println!("General protection fault (rip {:?})", stack_frame.instruction_pointer);
        crate::process::kill_current(SIGSEGV);
    }
    panic!("EXCEPTION: GENERAL PROTECTION FAULT (error {:#x})\n{:#?}", error_code, stack_frame);
}

extern "x86-interrupt" fn invalid_opcode_handler(stack_frame: InterruptStackFrame) {
    if crate::usermode::from_user(stack_frame.code_segment) {
        println!("Illegal instruction (rip {:?})", stack_frame.instruction_pointer);
        crate::process::kill_current(SIGILL);
    }
    panic!("EXCEPTION: INVALID OPCODE\n{:#?}", stack_frame);
}

// Signal numbers (Linux), used for "killed by signal" exit statuses.
const SIGILL: i32 = 4;
const SIGSEGV: i32 = 11;

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




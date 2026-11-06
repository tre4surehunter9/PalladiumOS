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
    idt[InterruptIndex::Timer.as_usize()]
        .set_handler_fn(timer_interrupt_handler);
    idt[InterruptIndex::Keyboard.as_usize()]
        .set_handler_fn(keyboard_interrupt_handler);
    idt.page_fault.set_handler_fn(page_fault_handler);
    idt.general_protection_fault.set_handler_fn(general_protection_fault_handler);
    idt.invalid_opcode.set_handler_fn(invalid_opcode_handler);

    crate::syscall::init_syscalls(&mut idt);

    idt

   };
}

use x86_64::structures::idt::PageFaultErrorCode;
use crate::hlt_loop;


extern "x86-interrupt" fn timer_interrupt_handler(
    stack_frame: InterruptStackFrame)
{
    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Timer.as_u8());
    }
    crate::process::tick();
    // Preemptive multitasking: only when we interrupted a user program, never
    // in the middle of kernel code (which may hold locks). Signals (Ctrl-C)
    // reach busy programs here too.
    if crate::usermode::from_user(stack_frame.code_segment) {
        crate::process::handle_signals();
        crate::process::preempt();
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

    if crate::usermode::from_user(stack_frame.code_segment) {
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




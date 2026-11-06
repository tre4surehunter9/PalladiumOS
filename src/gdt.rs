// Copyright (c) 2026 tre4surehunter9
use x86_64::VirtAddr;
use x86_64::structures::tss::TaskStateSegment;
use lazy_static::lazy_static;
use x86_64::structures::gdt::{GlobalDescriptorTable, Descriptor};

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;

const STACK_SIZE: usize = 4096 * 5;
static mut DOUBLE_FAULT_STACK: [u8; STACK_SIZE] = [0; STACK_SIZE];
/// Kernel stack used for interrupts from ring 3 before any process exists.
static mut BOOT_RING0_STACK: [u8; STACK_SIZE] = [0; STACK_SIZE];

/// The TSS lives in a static (not a lazy_static) because the scheduler
/// rewrites RSP0 on every switch: the CPU loads RSP0 when an interrupt or
/// syscall arrives from ring 3, so it must point at the running process's
/// kernel stack.
static mut TSS: TaskStateSegment = TaskStateSegment::new();

fn tss() -> &'static TaskStateSegment {
    unsafe {
        let tss = &mut *(&raw mut TSS);
        if tss.privilege_stack_table[0].is_null() {
            tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] =
                VirtAddr::from_ptr(&raw const DOUBLE_FAULT_STACK) + STACK_SIZE;
            tss.privilege_stack_table[0] = VirtAddr::from_ptr(&raw const BOOT_RING0_STACK) + STACK_SIZE;
        }
        &*(&raw const TSS)
    }
}

/// Set the stack the CPU switches to on interrupts/syscalls from ring 3.
pub fn set_kernel_stack(top: VirtAddr) {
    unsafe {
        // The TSS is packed, so this field isn't 8-byte aligned.
        core::ptr::write_unaligned(&raw mut (*(&raw mut TSS)).privilege_stack_table[0], top);
    }
}

pub fn init() {
    use x86_64::instructions::tables::load_tss;
    use x86_64::instructions::segmentation::{CS, DS, SS, Segment};

    GDT.0.load();
    unsafe {
        CS::set_reg(GDT.1.code_selector);
        SS::set_reg(GDT.1.data_selector);
        load_tss(GDT.1.tss_selector);
    }
}

use crate::gdt;
use crate::interrupts::double_fault_handler;
use x86_64::structures::idt::InterruptDescriptorTable;
use crate::interrupts::breakpoint_handler;

lazy_static! {
    static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();
        idt.breakpoint.set_handler_fn(breakpoint_handler);
        unsafe {
            idt.double_fault.set_handler_fn(double_fault_handler)
            .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX); // new
        }

        idt
    };
}

use x86_64::structures::gdt::SegmentSelector;

lazy_static! {
    static ref GDT: (GlobalDescriptorTable, Selectors) = {
        let mut gdt = GlobalDescriptorTable::new();
        let code_selector = gdt.add_entry(Descriptor::kernel_code_segment());
        let data_selector = gdt.add_entry(Descriptor::kernel_data_segment());
        let user_code_selector = gdt.add_entry(Descriptor::user_code_segment());
        let user_data_selector = gdt.add_entry(Descriptor::user_data_segment());
        let tss_selector = gdt.add_entry(Descriptor::tss_segment(tss()));
        (gdt, Selectors {
            code_selector,
         data_selector,
         user_code_selector,
         user_data_selector,
         tss_selector,
        })
    };
}

struct Selectors {
    code_selector: SegmentSelector,
    data_selector: SegmentSelector,
    user_code_selector: SegmentSelector,
    user_data_selector: SegmentSelector,
    tss_selector: SegmentSelector,
}

pub fn user_selectors() -> (SegmentSelector, SegmentSelector) {
    (GDT.1.user_code_selector, GDT.1.user_data_selector)
}

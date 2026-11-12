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

/// Set the stack this core switches to on interrupts/syscalls from ring 3:
/// its TSS's RSP0 (interrupts) and its PerCpu (the `syscall` entry, which
/// doesn't use the TSS).
pub fn set_kernel_stack(top: VirtAddr) {
    let cpu = crate::smp::cpu_index();
    unsafe {
        let tss = match cpu {
            0 => &raw mut TSS,
            _ => crate::smp::PERCPU[cpu].tss.load(core::sync::atomic::Ordering::Relaxed) as *mut TaskStateSegment,
        };
        // The TSS is packed, so this field isn't 8-byte aligned.
        core::ptr::write_unaligned(&raw mut (*tss).privilege_stack_table[0], top);
    }
    crate::smp::PERCPU[cpu].kernel_rsp.store(top.as_u64(), core::sync::atomic::Ordering::Relaxed);
}

/// The current RSP0 (the kernel stack for entries from ring 3).
pub fn kernel_stack_top() -> u64 {
    tss();
    unsafe { core::ptr::read_unaligned(&raw const (*(&raw const TSS)).privilege_stack_table[0]).as_u64() }
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

/// Another CPU core's GDT and TSS: the same layout as the first core's (so
/// the selectors are the same everywhere), with a TSS of its own (a TSS
/// can't be loaded by two cores) and its own double-fault stack.
pub fn init_ap(cpu: usize) {
    use alloc::boxed::Box;
    use x86_64::instructions::segmentation::{CS, DS, ES, SS, Segment};
    use x86_64::instructions::tables::load_tss;
    let df_stack: &'static mut [u8] = Box::leak(alloc::vec![0u8; STACK_SIZE].into_boxed_slice());
    let ring0_stack: &'static mut [u8] = Box::leak(alloc::vec![0u8; STACK_SIZE].into_boxed_slice());
    let tss: &'static mut TaskStateSegment = Box::leak(Box::new(TaskStateSegment::new()));
    tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = VirtAddr::from_ptr(df_stack.as_ptr()) + STACK_SIZE as u64;
    tss.privilege_stack_table[0] = VirtAddr::from_ptr(ring0_stack.as_ptr()) + STACK_SIZE as u64;
    let tss: &'static TaskStateSegment = tss;
    crate::smp::PERCPU[cpu].tss.store(tss as *const _ as u64, core::sync::atomic::Ordering::SeqCst);
    crate::smp::PERCPU[cpu].kernel_rsp.store(VirtAddr::from_ptr(ring0_stack.as_ptr()).as_u64() + STACK_SIZE as u64, core::sync::atomic::Ordering::SeqCst);
    let gdt: &'static mut GlobalDescriptorTable = Box::leak(Box::new(GlobalDescriptorTable::new()));
    let code = gdt.add_entry(Descriptor::kernel_code_segment());
    let data = gdt.add_entry(Descriptor::kernel_data_segment());
    gdt.add_entry(Descriptor::user_code_segment());
    gdt.add_entry(Descriptor::user_data_segment());
    let tss_sel = gdt.add_entry(Descriptor::tss_segment(tss));
    debug_assert!(code == GDT.1.code_selector && tss_sel == GDT.1.tss_selector);
    let gdt: &'static GlobalDescriptorTable = gdt;
    gdt.load();
    // From now on this core knows which it is (smp::cpu_index).
    crate::smp::PERCPU[cpu].gdt_base.store(x86_64::instructions::tables::sgdt().base.as_u64(), core::sync::atomic::Ordering::SeqCst);
    unsafe {
        CS::set_reg(code);
        SS::set_reg(data);
        DS::set_reg(data);
        ES::set_reg(data);
        load_tss(tss_sel);
    }
}

pub fn user_selectors() -> (SegmentSelector, SegmentSelector) {
    (GDT.1.user_code_selector, GDT.1.user_data_selector)
}

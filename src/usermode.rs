use core::arch::asm;
use x86_64::VirtAddr;

pub fn enter_usermode(entry_point: VirtAddr, user_stack: VirtAddr) -> ! {
    let (code_sel, data_sel) = crate::gdt::user_selectors();

    // RPL 3 = "requested privilege level 3" — the low 2 bits of the selector
    let cs: u64 = (code_sel.0 as u64) | 3;
    let ss: u64 = (data_sel.0 as u64) | 3;

    unsafe {
        asm!(
            "push {ss}",
            "push {rsp}",
            "push {rflags}",
            "push {cs}",
            "push {rip}",
            "iretq",
            ss     = in(reg) ss,
            rsp    = in(reg) user_stack.as_u64(),
            rflags = in(reg) 0x200u64, // interrupts enabled
            cs     = in(reg) cs,
            rip    = in(reg) entry_point.as_u64(),
            options(noreturn)
        );
    }
}

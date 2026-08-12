use core::arch::asm;
use x86_64::VirtAddr;

use x86_64::structures::paging::Page;
use crate::memory::{self, MAPPER, FRAME_ALLOCATOR};

pub fn launch_program(bytes: &[u8]) -> ! {
    let mut mapper_guard = MAPPER.lock();
    let mut alloc_guard = FRAME_ALLOCATOR.lock();
    let mapper = mapper_guard.as_mut().expect("MAPPER not initialized");
    let frame_allocator = alloc_guard.as_mut().expect("FRAME_ALLOCATOR not initialized");

    let user_page = Page::containing_address(VirtAddr::new(0x_6666_6666_0000));
    memory::map_user_page(mapper, frame_allocator, user_page);
    memory::load_payload_bytes(user_page.start_address(), bytes);

    let user_stack_page = Page::containing_address(VirtAddr::new(0x_5555_5555_0000));
    memory::map_user_page(mapper, frame_allocator, user_stack_page);

    drop(mapper_guard);
    drop(alloc_guard);

    enter_usermode(user_page.start_address(), user_stack_page.start_address() + 4096u64);
}

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

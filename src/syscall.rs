use core::arch::global_asm;

global_asm!(
".global syscall_entry",
"syscall_entry:",
"mov r10, rdi",       // stash original arg1 (r10/r11 are free scratch regs)
"mov r11, rsi",       // stash original arg2
"mov rdi, rax",       // dispatch(num, arg1, arg2): arg0 = syscall number
"mov rsi, r10",       //                            arg1 = original arg1
"mov rdx, r11",       //                            arg2 = original arg2
"sub rsp, 8",         // align stack to 16 bytes before `call`, per SysV ABI
"call syscall_dispatch",
"add rsp, 8",         // undo the alignment adjustment
"iretq",
);

#[unsafe(no_mangle)]
extern "C" fn syscall_dispatch(num: u64, arg1: u64, arg2: u64) -> u64 {
    match num {
        0 => sys_exit(arg1),
        1 => sys_write(arg1, arg2),
        _ => u64::MAX, // unknown syscall number
    }
}

fn sys_write(ptr: u64, len: u64) -> u64 {
    let slice = unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) };
    for &byte in slice {
        crate::print!("{}", byte as char);
    }
    len
}

fn sys_exit(_code: u64) -> ! {
    crate::println!("\n[process exited]");
    crate::hlt_loop();
}

use x86_64::structures::idt::InterruptDescriptorTable;
use x86_64::{PrivilegeLevel, VirtAddr};

unsafe extern "C" {
    fn syscall_entry();
}

pub fn init_syscalls(idt: &mut InterruptDescriptorTable) {
    unsafe {
        idt[0x80]
        .set_handler_addr(VirtAddr::new(syscall_entry as u64))
        .set_privilege_level(PrivilegeLevel::Ring3);
    }
}

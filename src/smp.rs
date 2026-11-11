// Copyright (c) 2026 tre4surehunter9
//! More than one CPU core (SMP): finding the cores and starting them.
//!
//! The firmware starts one core, the bootstrap processor (BSP), which runs
//! everything so far. The others, application processors (APs), wait in
//! real mode until the BSP sends them INIT and two "startup" IPIs (SIPI)
//! through its local APIC: then each starts at a page below 1 MiB, in
//! 16-bit real mode. There, a trampoline (below) takes it through 32-bit
//! protected mode into 64-bit long mode with the kernel's page tables, and
//! jumps to `ap_main` on a stack of its own.
//!
//! The cores are found in ACPI's MADT ("APIC") table, which the firmware
//! leaves in memory (the bootloader passes where its root, the RSDP, is).
//!
//! The BSP keeps taking interrupts from the 8259 PICs, as before: its local
//! APIC is switched on in "virtual wire" mode, which passes them through
//! (LINT0 as ExtINT). With only one core, nothing here changes anything.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use x86_64::instructions::port::Port;

/// Cores running (the BSP and the APs that started).
static ONLINE: AtomicUsize = AtomicUsize::new(1);
/// Where the local APIC's registers are mapped (the same physical address
/// for every core; each core sees its own APIC there).
static LAPIC: AtomicU64 = AtomicU64::new(0);

/// How many CPU cores run.
pub fn cores() -> usize {
    ONLINE.load(Ordering::SeqCst)
}

// Local APIC registers (offsets).
const LAPIC_ID: usize = 0x20;
const LAPIC_TPR: usize = 0x80;
const LAPIC_SVR: usize = 0xF0;
const LAPIC_ICR_LOW: usize = 0x300;
const LAPIC_ICR_HIGH: usize = 0x310;
const LAPIC_LVT_TIMER: usize = 0x320;
const LAPIC_LINT0: usize = 0x350;
const LAPIC_LINT1: usize = 0x360;
const LAPIC_LVT_ERROR: usize = 0x370;
/// The spurious-interrupt vector (an IDT entry that does nothing).
pub const SPURIOUS_VECTOR: u8 = 0xFF;

fn lapic_read(reg: usize) -> u32 {
    unsafe { core::ptr::read_volatile((LAPIC.load(Ordering::Relaxed) as usize + reg) as *const u32) }
}

fn lapic_write(reg: usize, value: u32) {
    unsafe { core::ptr::write_volatile((LAPIC.load(Ordering::Relaxed) as usize + reg) as *mut u32, value) }
}

/// This core's local APIC on: software-enabled, nothing masked by priority,
/// its own timer and error interrupts off.
fn lapic_enable_here() {
    lapic_write(LAPIC_TPR, 0);
    lapic_write(LAPIC_LVT_TIMER, 1 << 16);
    lapic_write(LAPIC_LVT_ERROR, 1 << 16);
    lapic_write(LAPIC_SVR, 0x100 | SPURIOUS_VECTOR as u32);
}

/// About `us` microseconds (port 0x80 writes take about one each).
fn udelay(us: u32) {
    let mut port = Port::<u8>::new(0x80);
    for _ in 0..us {
        unsafe { port.write(0) };
    }
}

/// Send an inter-processor interrupt (ICR low word `low`) to the core with
/// local APIC ID `apic`, and wait until it's delivered.
fn send_ipi(apic: u8, low: u32) {
    lapic_write(LAPIC_ICR_HIGH, (apic as u32) << 24);
    lapic_write(LAPIC_ICR_LOW, low);
    for _ in 0..100_000 {
        if lapic_read(LAPIC_ICR_LOW) & (1 << 12) == 0 {
            break;
        }
        core::hint::spin_loop();
    }
}

/// `len` bytes of physical memory (ACPI tables), mapped if they aren't.
fn phys(addr: u64, len: usize) -> &'static [u8] {
    unsafe { core::slice::from_raw_parts(crate::memory::map_mmio(addr, len), len) }
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes(b[i..i + 4].try_into().unwrap())
}

fn u64_at(b: &[u8], i: usize) -> u64 {
    u64::from_le_bytes(b[i..i + 8].try_into().unwrap())
}

/// The MADT: the local APIC's physical address and each usable core's APIC
/// ID (the BSP's among them).
fn read_madt(rsdp: u64) -> Option<(u64, Vec<u8>)> {
    let r = phys(rsdp, 36);
    if &r[0..8] != b"RSD PTR " {
        return None;
    }
    // ACPI 2+: the XSDT (64-bit pointers); ACPI 1: the RSDT (32-bit).
    let (root, wide) = if r[15] >= 2 && u64_at(r, 24) != 0 { (u64_at(r, 24), true) } else { (u32_at(r, 16) as u64, false) };
    let len = u32_at(phys(root, 36), 4) as usize;
    let root_table = phys(root, len);
    let step = if wide { 8 } else { 4 };
    let madt = (36..len).step_by(step).find_map(|i| {
        let addr = if wide { u64_at(root_table, i) } else { u32_at(root_table, i) as u64 };
        (&phys(addr, 4)[..] == b"APIC").then_some(addr)
    })?;
    let len = u32_at(phys(madt, 36), 4) as usize;
    let t = phys(madt, len);
    let mut lapic = u32_at(t, 36) as u64;
    let mut ids = Vec::new();
    let mut i = 44;
    while i + 2 <= len {
        let (kind, entry_len) = (t[i], t[i + 1] as usize);
        if entry_len < 2 || i + entry_len > len {
            break;
        }
        match kind {
            // A processor's local APIC: enabled, or able to be.
            0 if entry_len >= 8 && u32_at(t, i + 4) & 3 != 0 => ids.push(t[i + 3]),
            // A 64-bit local APIC address.
            5 if entry_len >= 12 => lapic = u64_at(t, i + 4),
            _ => {}
        }
        i += entry_len;
    }
    Some((lapic, ids))
}

// The trampoline: copied to a page below 1 MiB, where a starting core runs
// it. Its addresses are relative to its start (the page isn't known until
// boot): what's needed absolute is patched in (gdtr, farjmp), and EBX holds
// the page's address from 32-bit mode on. The data at the end (the kernel's
// page table, the core's stack, the entry point and its argument) is
// written by start_cores for each core in turn.
core::arch::global_asm!(
    ".global ap_tramp_start",
    ".global ap_tramp_end",
    ".global ap_gdtr",
    ".global ap_farjmp",
    ".global ap_pm32",
    ".global ap_gdt",
    ".global ap_cr3",
    ".global ap_stack",
    ".global ap_entry",
    ".global ap_arg",
    ".code16",
    "ap_tramp_start:",
    "    cli",
    "    cld",
    "    movw %cs, %ax",
    "    movw %ax, %ds",
    "    xorl %ebx, %ebx",
    "    movw %ax, %bx",
    "    shll $4, %ebx",
    "    lgdtl (ap_gdtr - ap_tramp_start)",
    "    movl %cr0, %eax",
    "    orl $1, %eax",
    "    movl %eax, %cr0",
    "    ljmpl *(ap_farjmp - ap_tramp_start)",
    ".code32",
    "ap_pm32:",
    "    movw $0x10, %ax",
    "    movw %ax, %ds",
    "    movw %ax, %es",
    "    movw %ax, %ss",
    "    leal 4080(%ebx), %esp", // a stack (the page's end) for the far return below
    "    movl %cr4, %eax",
    "    orl $0x20, %eax", // PAE
    "    movl %eax, %cr4",
    "    movl (ap_cr3 - ap_tramp_start)(%ebx), %eax",
    "    movl %eax, %cr3",
    "    movl $0xC0000080, %ecx", // EFER
    "    rdmsr",
    "    orl $0x901, %eax", // SCE, LME, NXE (the kernel's pages use NX)
    "    wrmsr",
    "    movl %cr0, %eax",
    "    orl $0x80010000, %eax", // paging, write-protect
    "    movl %eax, %cr0",
    "    leal (ap_lm64 - ap_tramp_start)(%ebx), %eax",
    "    pushl $0x18",
    "    pushl %eax",
    "    lret",
    ".code64",
    "ap_lm64:",
    "    movl %ebx, %ebx",
    "    movq (ap_stack - ap_tramp_start)(%rbx), %rsp",
    "    movq (ap_arg - ap_tramp_start)(%rbx), %rdi",
    "    movq (ap_entry - ap_tramp_start)(%rbx), %rax",
    "    jmp *%rax",
    ".balign 8",
    "ap_gdt:",
    "    .quad 0",
    "    .quad 0x00CF9A000000FFFF", // 0x08: 32-bit code
    "    .quad 0x00CF92000000FFFF", // 0x10: data
    "    .quad 0x00AF9A000000FFFF", // 0x18: 64-bit code
    "ap_gdtr:",
    "    .word 31",
    "    .long 0",
    "ap_farjmp:",
    "    .long 0",
    "    .word 0x08",
    ".balign 8",
    "ap_cr3: .quad 0",
    "ap_stack: .quad 0",
    "ap_entry: .quad 0",
    "ap_arg: .quad 0",
    "ap_tramp_end:",
    options(att_syntax)
);

unsafe extern "C" {
    static ap_tramp_start: u8;
    static ap_tramp_end: u8;
    static ap_gdtr: u8;
    static ap_farjmp: u8;
    static ap_pm32: u8;
    static ap_gdt: u8;
    static ap_cr3: u8;
    static ap_stack: u8;
    static ap_entry: u8;
    static ap_arg: u8;
}

/// Where `sym` is in the trampoline (bytes from its start).
fn offset(sym: *const u8) -> usize {
    sym as usize - (&raw const ap_tramp_start) as usize
}

/// Each core's kernel stack.
const AP_STACK: usize = 64 * 1024;

/// Find the CPU cores and start the others. Returns how many run.
pub fn init(rsdp: Option<u64>) -> usize {
    let Some((lapic_phys, ids)) = rsdp.and_then(read_madt) else { return 1 };
    if ids.len() <= 1 {
        return 1;
    }
    let Some(page) = crate::memory::FRAME_ALLOCATOR.lock().as_ref().and_then(|a| a.low_page()) else { return 1 };
    let cr3 = crate::memory::kernel_p4().start_address().as_u64();
    if cr3 >= 1 << 32 {
        return 1; // 32-bit code loads it
    }
    LAPIC.store(crate::memory::map_mmio(lapic_phys, 4096) as u64, Ordering::SeqCst);

    // The BSP's APIC on, passing the PICs' interrupts through.
    let bsp = x86_64::instructions::interrupts::without_interrupts(|| {
        use x86_64::registers::model_specific::Msr;
        let mut base = Msr::new(0x1B);
        unsafe {
            let v = base.read();
            base.write((v & !(1 << 10)) | (1 << 11)); // xAPIC mode, enabled
        }
        lapic_enable_here();
        lapic_write(LAPIC_LINT0, 0x700); // ExtINT: the 8259s
        lapic_write(LAPIC_LINT1, 0x400); // NMI
        (lapic_read(LAPIC_ID) >> 24) as u8
    });

    // The trampoline in place, patched for its page.
    unsafe {
        let start = &raw const ap_tramp_start;
        let len = (&raw const ap_tramp_end) as usize - start as usize;
        let dst = crate::memory::phys_to_virt(page) as *mut u8;
        core::ptr::copy_nonoverlapping(start, dst, len);
        let at = |sym: *const u8| dst.add(offset(sym));
        (at(&raw const ap_gdtr).add(2) as *mut u32).write_unaligned((page as usize + offset(&raw const ap_gdt)) as u32);
        (at(&raw const ap_farjmp) as *mut u32).write_unaligned((page as usize + offset(&raw const ap_pm32)) as u32);
        (at(&raw const ap_cr3) as *mut u64).write_unaligned(cr3);
        (at(&raw const ap_entry) as *mut u64).write_unaligned(ap_main as *const () as u64);
    }
    crate::memory::identity_map(page, true);

    let dst = crate::memory::phys_to_virt(page) as *mut u8;
    for (cpu, &id) in ids.iter().filter(|&&id| id != bsp).enumerate() {
        let cpu = cpu + 1;
        let stack = alloc::boxed::Box::leak(alloc::vec![0u8; AP_STACK].into_boxed_slice());
        let top = (stack.as_ptr() as u64 + AP_STACK as u64) & !0xF;
        unsafe {
            (dst.add(offset(&raw const ap_stack)) as *mut u64).write_volatile(top);
            (dst.add(offset(&raw const ap_arg)) as *mut u64).write_volatile(cpu as u64);
        }
        let before = ONLINE.load(Ordering::SeqCst);
        // INIT, then the startup IPI (the page's number is the vector);
        // a second one if the first didn't take, as Intel recommends.
        send_ipi(id, 0x4500);
        udelay(10_000);
        for _ in 0..2 {
            send_ipi(id, 0x4600 | (page >> 12) as u32);
            udelay(200);
            if ONLINE.load(Ordering::SeqCst) != before {
                break;
            }
        }
        // It reads the stack and argument before saying it's up.
        for _ in 0..200 {
            if ONLINE.load(Ordering::SeqCst) != before {
                break;
            }
            udelay(1000);
        }
    }
    crate::memory::identity_map(page, false);
    cores()
}

/// Where a started core comes, in 64-bit mode on its own stack, with the
/// trampoline's temporary GDT.
extern "C" fn ap_main(cpu: u64) -> ! {
    crate::gdt::init_ap();
    crate::interrupts::load_idt();
    lapic_enable_here();
    ONLINE.fetch_add(1, Ordering::SeqCst);
    let _ = cpu;
    // Nothing for it to do yet: it waits, with interrupts off.
    loop {
        x86_64::instructions::interrupts::disable();
        x86_64::instructions::hlt();
    }
}

// Copyright (c) 2026 tre4surehunter9
use crate::println;
use alloc::vec::Vec;

pub fn process_command(input: &str) {
    let input = input.trim();
    if input.is_empty() {
        return;
    }

    let parts: Vec<&str> = input.splitn(2, ' ').collect();
    let command = parts[0];
    let args = if parts.len() > 1 { parts[1] } else { "" };

    match command {
        "help" => cmd_help(),
        "echo" => cmd_echo(args),
        "clear" => cmd_clear(),
        "about" => cmd_about(),
        "reboot" => cmd_reboot(),
        "panic" => cmd_panic(),
        _ => {
            println!("Unknown command: '{}'. Type 'help' for a list of commands.", command);
        }
    }
}


fn cmd_help() {
    println!("Available commands:");
    println!("  help        - Show this help message");
    println!("  echo <text> - Print text to the screen");
    println!("  clear       - Clear the screen");
    println!("  reboot      - Reboot the system");
    println!("  panic       - Causes a system panic");
    println!("  about       - Show information about this kernel");
}

fn cmd_echo(args: &str) {
    println!("{}", args);
}

fn cmd_clear() {
   crate::vga_buffer::clear_screen();
}

fn cmd_about() {
    println!("FrostDOS v0.2.2 - A kernel in Rust");
    println!("Based on Philipp Oppermann's 'Writing an OS in Rust'");
    println!("https://os.phil-opp.com/");
}

pub fn print_welcome() {
    println!("FrostDOS v0.2.2");
    println!("----------------------------------------");
}

fn cmd_reboot() {
    use x86_64::instructions::port::Port;
    println!("Rebooting...");
    unsafe {
        let mut port: Port<u8> = Port::new(0x64);
        port.write(0xFE_u8);
    }

    loop {
        x86_64::instructions::hlt();
    }
}

fn cmd_panic() {
    crate::vga_buffer::clear_screen();
    panic!("User triggered kernel panic")
}

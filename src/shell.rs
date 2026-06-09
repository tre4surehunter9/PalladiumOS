// Copyright (c) 2026 tre4surehunter9
use crate::println;
use alloc::vec::Vec;
use crate::filesystem;
use crate::alloc::string::ToString;


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
        "ls"     => cmd_ls(args),
        "cat"    => cmd_cat(args),
        "write"  => cmd_write(args),
        "mkdir"  => cmd_mkdir(args),
        "rm"     => cmd_rm(args),
        "cd"     => cmd_cd(args),
        "pwd"    => cmd_pwd(),
        "edit" => cmd_edit(args),
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
    println!("  ls [path]           - List directory");
    println!("  cat <file>          - Print file contents");
    println!("  write <file> <text> - Write text to file");
    println!("  mkdir <dir>         - Create directory");
    println!("  rm <file>           - Remove file or empty directory");
    println!("  cd <dir>            - Change directory");
    println!("  pwd                 - Print working directory");
    println!("  edit <file>         - Open file in the text editor");
}


fn cmd_echo(args: &str) {
    println!("{}", args);
}

fn cmd_clear() {
   crate::vga_buffer::clear_screen();
}

fn cmd_about() {
    println!("PalladiumOS v0.3.2 - A kernel in Rust");
    println!("Based on Philipp Oppermann's 'Writing an OS in Rust'");
    println!("https://os.phil-opp.com/");
}

pub fn print_welcome() {
    println!("------------------");
    println!("PalladiumOS v0.3.2");
    println!("------------------");
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

fn cmd_ls(args: &str) {
    let path = if args.is_empty() {
        filesystem::CWD.lock().clone()
    } else {
        filesystem::resolve_path(args)
    };

    let entries = filesystem::list_dir(&path);
    if entries.is_empty() {
        println!("(empty)");
        return;
    }
    for (name, is_dir) in entries {
        if is_dir {
            println!("{}/", name);
        } else {
            println!("{}", name);
        }
    }
}

fn cmd_cat(args: &str) {
    if args.is_empty() {
        println!("Usage: cat <file>");
        return;
    }
    let path = filesystem::resolve_path(args);
    match filesystem::read_file(&path) {
        Ok(contents) => println!("{}", contents),
        Err(e)       => println!("cat: {}", e),
    }
}

fn cmd_write(args: &str) {
    let parts: Vec<&str> = args.splitn(2, ' ').collect();
    if parts.len() < 2 {
        println!("Usage: write <file> <contents>");
        return;
    }
    let path = filesystem::resolve_path(parts[0]);
    match filesystem::write_file(&path, parts[1]) {
        Ok(())  => println!("Written."),
        Err(e)  => println!("write: {}", e),
    }
}

fn cmd_mkdir(args: &str) {
    if args.is_empty() {
        println!("Usage: mkdir <dir>");
        return;
    }
    let path = filesystem::resolve_path(args);
    match filesystem::make_dir(&path) {
        Ok(())  => {},
        Err(e)  => println!("mkdir: {}", e),
    }
}

fn cmd_rm(args: &str) {
    if args.is_empty() {
        println!("Usage: rm <file>");
        return;
    }
    let path = filesystem::resolve_path(args);
    match filesystem::remove(&path) {
        Ok(())  => {},
        Err(e)  => println!("rm: {}", e),
    }
}

fn cmd_cd(args: &str) {
    if args.is_empty() {
        *filesystem::CWD.lock() = "/".to_string();
        return;
    }

    let target = if args == ".." {
        let cwd = filesystem::CWD.lock().clone();
        if cwd == "/" {
            "/".to_string()
        } else {
            let parent = cwd.rsplitn(2, '/').nth(1).unwrap_or("/");
            if parent.is_empty() { "/".to_string() } else { parent.to_string() }
        }
    } else {
        filesystem::resolve_path(args)
    };

    if filesystem::is_dir(&target) {
        *filesystem::CWD.lock() = target;
    } else {
        println!("cd: not a directory: {}", args);
    }
}

fn cmd_pwd() {
    let cwd = filesystem::CWD.lock().clone();
    println!("{}", cwd);
}

fn cmd_edit(args: &str) {
    if args.is_empty() {
        println!("Usage: edit <filename>");
        return;
    }
    let path = crate::filesystem::resolve_path(args);
    crate::editor::open(&path);
}

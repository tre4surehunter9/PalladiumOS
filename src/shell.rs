// Copyright (c) 2026 tre4surehunter9
use crate::println;
use alloc::vec::Vec;
use crate::filesystem;
use crate::alloc::string::ToString;


pub fn process_command(input: &str) -> bool {
    let input = input.trim();
    if input.is_empty() {
        return false;
    }

    let parts: Vec<&str> = input.splitn(2, ' ').collect();
    let command = parts[0];
    let args = if parts.len() > 1 { parts[1] } else { "" };

    match command {
        "help"   => { cmd_help();   true }
        "echo"   => { cmd_echo(args); true }
        "clear"  => { cmd_clear();  true }
        "about"  => { cmd_about();  true }
        "reboot" => { cmd_reboot(); true }
        "edit"   => cmd_edit(args),
        "ls"     => cmd_ls(args),
        "cat"    => cmd_cat(args),
        "mkdir"  => cmd_mkdir(args),
        "rm"     => cmd_rm(args),
        "cd"     => cmd_cd(args),
        "pwd"    => { cmd_pwd(); true }
        "run"    => cmd_run(args),
        _ => {
            println!("Unknown command: '{}'. Type 'help'.", command);
            false
        }
    }
}


fn cmd_help() {
    println!("Available commands:");
    println!("  help                - Show this help message");
    println!("  echo <text>         - Print text to the screen");
    println!("  clear               - Clear the screen");
    println!("  reboot              - Reboot the system");
    println!("  panic               - Causes a system panic");
    println!("  about               - Show information about this kernel");
    println!("  edit <file>         - Open file in the text editor");
    println!("  run <file>          - Run a script file");
    println!("  ls [path]           - List directory");
    println!("  cat <file>          - Print file contents");
    println!("  mkdir <dir>         - Create directory");
    println!("  rm <file>           - Remove file or empty directory");
    println!("  cd <dir>            - Change directory");
    println!("  pwd                 - Print working directory");
}


fn cmd_echo(args: &str) {
    println!("{}", args);
}

fn cmd_clear() {
    crate::vga_buffer::clear_screen();
}

fn cmd_about() {
    println!("PalladiumOS v0.4.0 - A kernel in Rust");
    println!("Based on Philipp Oppermann's 'Writing an OS in Rust'");
    println!("https://github.com/tre4surehunter9/PalladiumOS/");
}

pub fn print_welcome() {
    println!("------------------");
    println!("PalladiumOS v0.4.0");
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

fn cmd_ls(args: &str) -> bool {
    let path = if args.is_empty() {
        filesystem::CWD.lock().clone()
    } else {
        filesystem::resolve_path(args)
    };

    let entries = filesystem::list_dir(&path);
    if entries.is_empty() {
        println!("(empty)");
    } else {
        for (name, is_dir) in entries {
            if is_dir { println!("{}/", name); }
            else       { println!("{}", name); }
        }
    }
    true
}

fn cmd_cat(args: &str) -> bool {
    if args.is_empty() {
        println!("Usage: cat <file>");
        return false;
    }
    let path = filesystem::resolve_path(args);
    match filesystem::read_file(&path) {
        Ok(contents) => { println!("{}", contents); true }
        Err(e)       => { println!("cat: {}", e);   false }
    }
}

fn cmd_mkdir(args: &str) -> bool {
    if args.is_empty() {
        println!("Usage: mkdir <dir>");
        return false;
    }
    let path = filesystem::resolve_path(args);
    match filesystem::make_dir(&path) {
        Ok(())  => true,
        Err(e)  => { println!("mkdir: {}", e); false }
    }
}

fn cmd_rm(args: &str) -> bool {
    if args.is_empty() {
        println!("Usage: rm <file>");
        return false;
    }
    let path = filesystem::resolve_path(args);
    match filesystem::remove(&path) {
        Ok(())  => true,
        Err(e)  => { println!("rm: {}", e); false }
    }
}

fn cmd_cd(args: &str) -> bool {
    if args.is_empty() {
        *filesystem::CWD.lock() = "/".to_string();
        return true;
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
        true
    } else {
        println!("cd: not a directory: {}", args);
        false
    }
}

fn cmd_pwd() {
    let cwd = filesystem::CWD.lock().clone();
    println!("{}", cwd);
}


fn cmd_edit(args: &str) -> bool {
    if args.is_empty() {
        println!("Usage: edit <filename>");
        return false;
    }
    let path = crate::filesystem::resolve_path(args);
    crate::editor::open(&path);
    true
}

fn cmd_run(args: &str) -> bool {
    if args.is_empty() {
        println!("Usage: run <script>");
        return false;
    }

    let path = filesystem::resolve_path(args);

    let contents = match filesystem::read_file(&path) {
        Ok(c) => c,
        Err(e) => {
            println!("run: {}", e);
            return false;
        }
    };

    println!("Running {}...", args);

    for (line_num, line) in contents.lines().enumerate() {
        let line = line.trim();

        if line.is_empty() { continue; }
        if line.starts_with('#') { continue; }

        println!("> {}", line);

        if !process_command(line) {
            println!("Script stopped at line {} due to error.", line_num + 1);
            return false;
        }
    }

    println!("Script completed.");
    true
}

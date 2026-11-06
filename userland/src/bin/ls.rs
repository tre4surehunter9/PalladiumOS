// Copyright (c) 2026 tre4surehunter9
//! ls: list directory contents.
//!   -a  include entries starting with '.' (including . and ..)
//!   -l  long format: type, size and name
#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use userlib::{close, eprintln, for_each_entry, open, print, println, stat, strerror, O_DIRECTORY, O_RDONLY, DT_DIR};

userlib::entry!(main);

/// "2026-11-06 07:02" (or blank padding for files without a time).
fn when(t: i64) -> alloc::string::String {
    if t <= 0 {
        return alloc::string::String::from("                ");
    }
    let (y, mo, d, h, mi, _) = userlib::civil_time(t as u64);
    alloc::format!("{:04}-{:02}-{:02} {:02}:{:02}", y, mo, d, h, mi)
}

/// "drwxr-xr-x" for a mode.
fn mode_string(mode: u32) -> alloc::string::String {
    let kind = match mode & userlib::S_IFMT {
        userlib::S_IFDIR => 'd',
        userlib::S_IFLNK => 'l',
        userlib::S_IFCHR => 'c',
        userlib::S_IFIFO => 'p',
        _ => '-',
    };
    let mut s = alloc::string::String::new();
    s.push(kind);
    for shift in [6, 3, 0] {
        let bits = (mode >> shift) & 7;
        s.push(if bits & 4 != 0 { 'r' } else { '-' });
        s.push(if bits & 2 != 0 { 'w' } else { '-' });
        s.push(if bits & 1 != 0 { 'x' } else { '-' });
    }
    s
}

struct Options {
    all: bool,
    long: bool,
}

/// Join dir and name into `buf` and return it as &str (for stat in -l mode).
fn join<'a>(buf: &'a mut [u8; 512], dir: &str, name: &str) -> &'a str {
    let mut len = 0;
    for part in [dir.as_bytes(), b"/", name.as_bytes()] {
        let n = part.len().min(buf.len() - len);
        buf[len..len + n].copy_from_slice(&part[..n]);
        len += n;
    }
    core::str::from_utf8(&buf[..len]).unwrap_or("")
}

fn list(path: &str, opts: &Options) -> i32 {
    // A file argument is just printed, like POSIX ls. With -l a symlink is
    // shown itself; without, it's followed (so a link to a directory lists it).
    let found = if opts.long { userlib::lstat(path) } else { stat(path) };
    match found {
        Ok(st) if !st.is_dir() => {
            if opts.long {
                let mut line =
                    alloc::format!("{} {:>2} {:>8}  {}  {}", mode_string(st.st_mode), st.st_nlink, st.st_size, when(st.st_mtime), path);
                if st.is_symlink() {
                    let mut target = [0u8; 1024];
                    if let Ok(t) = userlib::readlink(path, &mut target) {
                        line.push_str(" -> ");
                        line.push_str(t);
                    }
                }
                println!("{}", line);
            } else {
                println!("{}", path);
            }
            return 0;
        }
        Ok(_) => {}
        Err(e) => {
            eprintln!("ls: cannot access '{}': {}", path, strerror(-e));
            return 2;
        }
    }

    let fd = open(path, O_RDONLY | O_DIRECTORY);
    if fd < 0 {
        eprintln!("ls: cannot open '{}': {}", path, strerror(-fd));
        return 2;
    }
    let fd = fd as i32;
    // Collect, then sort by name like POSIX ls.
    let mut entries: Vec<(String, bool)> = Vec::new();
    let result = for_each_entry(fd, |entry| {
        if opts.all || !entry.name.starts_with('.') {
            entries.push((entry.name.to_string(), entry.d_type == DT_DIR));
        }
    });
    entries.sort();
    for (name, is_dir) in &entries {
        if opts.long {
            let mut buf = [0u8; 512];
            let full = join(&mut buf, path, name);
            match userlib::lstat(full) {
                Ok(st) => {
                    let mut line = alloc::format!(
                        "{} {:>2} {:>8}  {}  {}",
                        mode_string(st.st_mode),
                        st.st_nlink,
                        st.st_size,
                        when(st.st_mtime),
                        name
                    );
                    if st.is_symlink() {
                        let mut target = [0u8; 1024];
                        let mut again = [0u8; 512];
                        if let Ok(t) = userlib::readlink(join(&mut again, path, name), &mut target) {
                            line.push_str(" -> ");
                            line.push_str(t);
                        }
                    }
                    println!("{}", line);
                }
                Err(_) => println!("?????????? {}", name),
            }
        } else if *is_dir {
            println!("{}/", name);
        } else {
            println!("{}", name);
        }
    }
    close(fd); // closed only now: /proc/self/fd lists this one too
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("ls: {}: {}", path, strerror(-e));
            2
        }
    }
}

fn main(args: userlib::Args) -> i32 {
    let mut opts = Options { all: false, long: false };
    let mut paths = 0;
    for arg in args.skip(1) {
        if let Some(flags) = arg.strip_prefix('-').filter(|f| !f.is_empty()) {
            for c in flags.chars() {
                match c {
                    'a' => opts.all = true,
                    'l' => opts.long = true,
                    _ => {
                        eprintln!("ls: invalid option -- '{}'", c);
                        return 2;
                    }
                }
            }
        } else {
            paths += 1;
        }
    }

    if paths == 0 {
        return list(".", &opts);
    }
    let mut status = 0;
    for path in args.skip(1).filter(|a| !a.starts_with('-') || *a == "-") {
        // "dir:" headers only for directories, when listing several paths.
        if paths > 1 && stat(path).is_ok_and(|s| s.is_dir()) {
            print!("{}:\n", path);
        }
        status = status.max(list(path, &opts));
    }
    status
}

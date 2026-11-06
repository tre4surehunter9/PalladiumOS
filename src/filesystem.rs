// Copyright (c) 2026 tre4surehunter9

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use spin::Mutex;
use lazy_static::lazy_static;
use crate::fdfs::Fdfs;

lazy_static! {
    /// The global FDFS instance.
    pub static ref FS: Mutex<Fdfs> = Mutex::new(Fdfs::new());

    /// The current working directory as an absolute path.
    pub static ref CWD: Mutex<String> = Mutex::new("/".to_string());
}

/// Resolve a path relative to the CWD into a normalized absolute path.
pub fn resolve_path(path: &str) -> String {
    let cwd = CWD.lock().clone();
    normalize(&cwd, path)
}

/// Turn `path` (absolute, or relative to `cwd`) into a clean absolute path:
/// "." components and repeated slashes are dropped, and ".." removes the
/// previous component (".." at the root stays at the root, as in POSIX).
///
///   normalize("/a/b", "../c")   == "/a/c"
///   normalize("/", "./x//y/")   == "/x/y"
pub fn normalize(cwd: &str, path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    let full = if path.starts_with('/') { [path, ""] } else { [cwd, path] };
    for piece in full {
        for component in piece.split('/') {
            match component {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                name => parts.push(name),
            }
        }
    }
    if parts.is_empty() {
        return "/".to_string();
    }
    let mut out = String::new();
    for part in parts {
        out.push('/');
        out.push_str(part);
    }
    out
}

pub fn list_dir(path: &str) -> Vec<(String, bool)> {
    FS.lock().list_dir(path).unwrap_or_default()
}

pub fn read_file(path: &str) -> Result<String, &'static str> {
    FS.lock().read_file(path)
}

/// Read a whole file as bytes (programs aren't UTF-8).
pub fn read_bytes(path: &str) -> Result<Vec<u8>, &'static str> {
    let fs = FS.lock();
    let idx = fs.resolve(path).ok_or("File not found")?;
    let size = fs.file_size(idx).ok_or("Is a directory")?;
    let mut data = alloc::vec![0u8; size];
    fs.read_at(idx, 0, &mut data)?;
    Ok(data)
}

pub fn write_file(path: &str, contents: &str) -> Result<(), &'static str> {
    FS.lock().write_file(path, contents)
}

pub fn make_dir(path: &str) -> Result<(), &'static str> {
    FS.lock().make_dir(path)
}

pub fn remove(path: &str) -> Result<(), &'static str> {
    FS.lock().remove(path)
}

pub fn is_dir(path: &str) -> bool {
    FS.lock().is_dir(path)
}

pub fn init_default_files() {
    FS.lock().make_dir("/features").ok();
    FS.lock().make_dir("/scripts").ok();
    FS.lock().write_file("/scripts/test.psh",
                         "# PalladiumOS Test Script\necho This is a test of the PalladiumOS Scripting.\n").ok();
    FS.lock().write_file ("/features/features.txt",
                          "Features:\n Text Editor\n Filesystem\n Scripting\n Shell\n").ok();
    FS.lock().write_file ("/welcome.txt",
                          "Welcome to PalladiumOS! This is my personal OS project!\nYou can find it at https://github.com/tre4surehunter9/PalladiumOS/\n").ok();
}


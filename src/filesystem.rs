// Copyright (c) 2026 tre4surehunter9

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use spin::Mutex;
use lazy_static::lazy_static;
use crate::mounts::{self, FileType};

lazy_static! {
    /// The kernel shell's working directory, as an absolute path.
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
/// `real` (an absolute path) as seen from inside root directory `root`, or
/// None if it's outside it.
pub fn strip_root(root: &str, real: &str) -> Option<String> {
    if root == "/" {
        return Some(real.to_string());
    }
    if real == root {
        return Some("/".to_string());
    }
    real.strip_prefix(root).filter(|r| r.starts_with('/')).map(String::from)
}

/// The real path of `inside` (an absolute path inside root `root`).
pub fn with_root(root: &str, inside: &str) -> String {
    match (root, inside) {
        ("/", p) => p.to_string(),
        (r, "/") => r.to_string(),
        (r, p) => alloc::format!("{}{}", r, p),
    }
}

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

/// Kernel-side file helpers (for the kernel shell, the editor, installing
/// /bin and the tests). They go through the VFS like system calls do.
/// Errors are short messages.
fn message(errno: i64) -> &'static str {
    use crate::syscall::*;
    match errno {
        ENOENT => "File not found",
        ENOTDIR => "Not a directory",
        EISDIR => "Is a directory",
        EEXIST => "Already exists",
        ENOTEMPTY => "Directory is not empty",
        EBUSY => "Device or resource busy",
        EINVAL => "Invalid path",
        EFBIG => "File too large",
        30 => "Read-only filesystem",
        28 => "No space left on device",
        _ => "I/O error",
    }
}

/// (name, is_dir) for each entry of a directory (empty if it can't be read).
pub fn list_dir(path: &str) -> Vec<(String, bool)> {
    let Ok(loc) = mounts::resolve(path) else { return Vec::new() };
    mounts::with_fs(loc.mount, |fs| fs.readdir(loc.ino))
        .map(|entries| entries.into_iter().map(|(n, _, k)| (n, k == FileType::Dir)).collect())
        .unwrap_or_default()
}

/// Read a whole file as bytes (programs aren't UTF-8).
pub fn read_bytes(path: &str) -> Result<Vec<u8>, &'static str> {
    let (loc, meta) = mounts::stat(path).map_err(message)?;
    if meta.kind == FileType::Dir {
        return Err("Is a directory");
    }
    let mut data = alloc::vec![0u8; meta.size as usize];
    let n = mounts::with_fs(loc.mount, |fs| fs.read(loc.ino, 0, &mut data)).map_err(message)?;
    data.truncate(n);
    Ok(data)
}

pub fn read_file(path: &str) -> Result<String, &'static str> {
    String::from_utf8(read_bytes(path)?).map_err(|_| "File is not valid UTF-8")
}

/// Create or replace a file's contents.
pub fn write_bytes(path: &str, contents: &[u8]) -> Result<(), &'static str> {
    let loc = match mounts::stat(path) {
        Ok((_, meta)) if meta.kind == FileType::Dir => return Err("Is a directory"),
        Ok((loc, _)) => loc,
        Err(crate::syscall::ENOENT) => mounts::create(path, FileType::File, 0o644).map_err(message)?,
        Err(e) => return Err(message(e)),
    };
    mounts::with_fs(loc.mount, |fs| {
        fs.truncate(loc.ino, 0)?;
        let mut done = 0;
        while done < contents.len() {
            done += fs.write(loc.ino, done as u64, &contents[done..])?;
        }
        Ok(())
    })
    .map_err(message)
}

const OS_RELEASE: &str = concat!(
    "NAME=\"PalladiumOS\"\nPRETTY_NAME=\"PalladiumOS ", env!("CARGO_PKG_VERSION"), "\"\nID=palladiumos\n",
    "VERSION=\"", env!("CARGO_PKG_VERSION"), "\"\nVERSION_ID=", env!("CARGO_PKG_VERSION"), "\n",
    "HOME_URL=\"https://github.com/tre4surehunter9/PalladiumOS/\"\n",
);

/// The PalladiumOS logo (for fastfetch): element 46 of the periodic table.
const PALLADIUM_LOGO: &str = "\
$1+------------------+
$1|$2 46               $1|
$1|$2   ____      _    $1|
$1|$2  |  _ \\  __| |   $1|
$1|$2  | |_) |/ _` |   $1|
$1|$2  |  __/| (_| |   $1|
$1|$2  |_|    \\__,_|   $1|
$1|$2                  $1|
$1|$2    Palladium     $1|
$1|$2     106.42       $1|
$1+------------------+
";

pub fn write_file(path: &str, contents: &str) -> Result<(), &'static str> {
    write_bytes(path, contents.as_bytes())
}

pub fn make_dir(path: &str) -> Result<(), &'static str> {
    mounts::create(path, FileType::Dir, 0o755).map(|_| ()).map_err(message)
}

/// Remove a file or an empty directory.
pub fn remove(path: &str) -> Result<(), &'static str> {
    let is_dir = mounts::is_dir(path);
    mounts::remove(path, is_dir).map_err(message)
}

/// Set a file's permission bits.
pub fn chmod(path: &str, perm: u32) -> Result<(), &'static str> {
    let loc = mounts::resolve(path).map_err(message)?;
    mounts::with_fs(loc.mount, |fs| fs.chmod(loc.ino, perm)).map_err(message)
}

pub fn is_dir(path: &str) -> bool {
    mounts::is_dir(path)
}

/// The files a fresh system starts with (only created if missing, so a
/// disk keeps whatever the user changed).
pub fn init_default_files() {
    let defaults = [
        // Names for addresses (libc reads this before asking DNS).
        ("/etc/hosts", "127.0.0.1\tlocalhost\n10.0.2.2\tgateway\n"),
        // fastfetch's system-wide config: PalladiumOS's logo, a periodic
        // table tile for palladium.
        (
            "/etc/xdg/fastfetch/config.jsonc",
            "{\n  \"logo\": { \"type\": \"file\", \"source\": \"/etc/xdg/fastfetch/palladium.txt\",\n            \
             \"color\": { \"1\": \"cyan\", \"2\": \"white\" } },\n  \
             \"modules\": [\"title\", \"separator\", \"os\", \"host\", \"kernel\", \"uptime\", \"shell\", \
             \"terminal\", \"cpu\", \"memory\", \"swap\", \"disk\", \"localip\", \"locale\", \"break\", \"colors\"]\n}\n",
        ),
        ("/etc/xdg/fastfetch/palladium.txt", PALLADIUM_LOGO),
        // The machine's name, read at boot (`hostname NAME` changes it
        // until the next boot; write it here to keep it).
        ("/etc/hostname", "palladium\n"),
        // Login shells (SSH servers only let users with one of these in).
        ("/etc/shells", "/bin/sh\n/bin/dash\n/usr/bin/ash\n/bin/busybox\n"),
    ];
    let _ = make_dir("/etc");
    for dir in ["/etc/xdg", "/etc/xdg/fastfetch", "/etc/dropbear"] {
        let _ = make_dir(dir);
    }
    for (path, text) in defaults {
        if mounts::resolve(path).is_err() {
            let _ = write_file(path, text);
        }
    }
    // Dynamically linked programs name musl's dynamic linker by its usual
    // path; it's libc.so on the system disk.
    // (And glibc's, for Linux programs built against glibc.)
    let _ = make_dir("/lib");
    let _ = make_dir("/lib64");
    for (link, target) in [
        ("/lib/ld-musl-x86_64.so.1", "/usr/lib/libc.so"),
        ("/lib64/ld-linux-x86-64.so.2", "/usr/lib/ld-linux-x86-64.so.2"),
    ] {
        if mounts::lstat(link).is_err() {
            let _ = mounts::symlink(target, link);
        }
    }
    // Which OS this is, for programs like fastfetch (freedesktop format).
    // Written every boot: it describes the system, so an old disk gets the
    // current version.
    let _ = write_file("/etc/os-release", OS_RELEASE);
    init_users();
}

/// Set the owner of a file (kernel helper; no permission checks).
pub fn chown(path: &str, uid: u32, gid: u32) -> Result<(), &'static str> {
    let loc = mounts::resolve(path).map_err(message)?;
    mounts::with_fs(loc.mount, |fs| fs.chown(loc.ino, uid, gid)).map_err(message)
}

/// The accounts a fresh system starts with: root, and an ordinary user
/// (uid 1000). Neither has a password until one is set with `passwd`.
/// Only created if /etc/passwd is missing, so existing accounts are kept.
fn init_users() {
    if mounts::resolve("/etc/passwd").is_ok() {
        return;
    }
    let _ = make_dir("/etc");
    let _ = write_file("/etc/passwd", "root:x:0:0:root:/root:/bin/sh\nuser:x:1000:1000:User:/home/user:/bin/sh\n");
    let _ = write_file("/etc/group", "root:x:0:\nwheel:x:10:root,user\nuser:x:1000:\n");
    // Empty password fields: log in without a password (set one with passwd).
    let _ = write_file("/etc/shadow", "root::0:0:99999:7:::\nuser::0:0:99999:7:::\n");
    let _ = chmod("/etc/shadow", 0o600);
    let _ = make_dir("/root");
    let _ = chmod("/root", 0o700);
    let _ = make_dir("/home");
    let _ = make_dir("/home/user");
    let _ = chown("/home/user", 1000, 1000);
}

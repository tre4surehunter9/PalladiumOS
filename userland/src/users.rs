// Copyright (c) 2026 tre4surehunter9
//! User accounts: /etc/passwd, /etc/group and /etc/shadow, and passwords.
//!
//!   /etc/passwd  name:x:uid:gid:full name:home:shell   (readable by all)
//!   /etc/group   name:x:gid:member,member
//!   /etc/shadow  name:hash:...                          (root only)
//!
//! Password hashes are SHA-256 crypt, `$5$SALT$HASH`: the standard format
//! Linux and libc's crypt(3) use (so C programs like Dropbear and BusyBox
//! can check them). Hashes in PalladiumOS's older format, `$sha256$SALT$HASH`,
//! still work; setting the password again converts them. An empty hash
//! means "no password".

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::io::read_text;
use crate::*;

#[derive(Clone, Debug)]
pub struct User {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    pub gecos: String,
    pub home: String,
    pub shell: String,
}

#[derive(Clone, Debug)]
pub struct Group {
    pub name: String,
    pub gid: u32,
    pub members: Vec<String>,
}

pub fn users() -> Vec<User> {
    read_text("/etc/passwd")
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split(':').collect();
            if f.len() < 7 {
                return None;
            }
            Some(User {
                name: f[0].to_string(),
                uid: f[2].parse().ok()?,
                gid: f[3].parse().ok()?,
                gecos: f[4].to_string(),
                home: f[5].to_string(),
                shell: f[6].to_string(),
            })
        })
        .collect()
}

pub fn groups() -> Vec<Group> {
    read_text("/etc/group")
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split(':').collect();
            if f.len() < 4 {
                return None;
            }
            Some(Group {
                name: f[0].to_string(),
                gid: f[2].parse().ok()?,
                members: f[3].split(',').filter(|m| !m.is_empty()).map(String::from).collect(),
            })
        })
        .collect()
}

pub fn user_by_name(name: &str) -> Option<User> {
    users().into_iter().find(|u| u.name == name)
}

pub fn user_by_uid(uid: u32) -> Option<User> {
    users().into_iter().find(|u| u.uid == uid)
}

pub fn group_by_gid(gid: u32) -> Option<Group> {
    groups().into_iter().find(|g| g.gid == gid)
}

pub fn group_by_name(name: &str) -> Option<Group> {
    groups().into_iter().find(|g| g.name == name)
}

/// A user's name, or the number if there's no such user.
pub fn user_name(uid: u32) -> String {
    user_by_uid(uid).map(|u| u.name).unwrap_or_else(|| uid.to_string())
}

pub fn group_name(gid: u32) -> String {
    group_by_gid(gid).map(|g| g.name).unwrap_or_else(|| gid.to_string())
}

/// The supplementary groups listing `name` as a member.
pub fn groups_of(name: &str) -> Vec<u32> {
    groups().into_iter().filter(|g| g.members.iter().any(|m| m == name)).map(|g| g.gid).collect()
}

/// The password hash for `name` from /etc/shadow (None if unreadable or
/// missing; Some("") for no password).
pub fn shadow_hash(name: &str) -> Option<String> {
    let text = read_text("/etc/shadow")?;
    text.lines().find_map(|l| {
        let mut f = l.split(':');
        (f.next()? == name).then(|| f.next().unwrap_or("").to_string())
    })
}

/// Write a new /etc/shadow entry for `name`, keeping everything else.
pub fn set_shadow_hash(name: &str, hash: &str) -> Result<(), isize> {
    let text = read_text("/etc/shadow").ok_or(-EACCES)?;
    let mut found = false;
    let mut out = String::new();
    for line in text.lines() {
        let mut fields: Vec<String> = line.split(':').map(String::from).collect();
        if fields.first().map(String::as_str) == Some(name) {
            if fields.len() < 2 {
                fields.resize(2, String::new());
            }
            fields[1] = hash.to_string();
            found = true;
        }
        out.push_str(&fields.join(":"));
        out.push('\n');
    }
    if !found {
        out.push_str(&format!("{}:{}:0:0:99999:7:::\n", name, hash));
    }
    write_file_atomic("/etc/shadow", out.as_bytes(), 0o600)
}

/// Replace a file's contents: write PATH.tmp, then rename it over PATH, so
/// a crash never leaves a half-written file.
pub fn write_file_atomic(path: &str, data: &[u8], mode: u32) -> Result<(), isize> {
    let tmp = format!("{}.tmp", path);
    let fd = open(&tmp, O_WRONLY | O_CREAT | O_TRUNC);
    if fd < 0 {
        return Err(fd);
    }
    let r = write_all(fd as i32, data);
    close(fd as i32);
    r?;
    chmod(&tmp, mode);
    let r = rename(&tmp, path);
    if r < 0 { Err(r) } else { Ok(()) }
}

/// Add a line to /etc/passwd or /etc/group.
pub fn append_line(path: &str, line: &str) -> Result<(), isize> {
    let mut text = read_text(path).unwrap_or_default();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(line);
    text.push('\n');
    write_file_atomic(path, text.as_bytes(), 0o644)
}

/// Remove the lines for `name` from a colon-separated file.
pub fn remove_entry(path: &str, name: &str, mode: u32) -> Result<(), isize> {
    let text = read_text(path).ok_or(-ENOENT)?;
    let kept: String = text
        .lines()
        .filter(|l| l.split(':').next() != Some(name))
        .map(|l| format!("{}\n", l))
        .collect();
    write_file_atomic(path, kept.as_bytes(), mode)
}

// ---------------------------------------------------------------- hashing

/// SHA-256 (FIPS 180-4).
pub fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    let mut h: [u32; 8] =
        [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64) * 8;
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(chunk[i * 4..i * 4 + 4].try_into().unwrap());
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (x, v) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *x = x.wrapping_add(v);
        }
    }
    let mut out = [0u8; 32];
    for (i, v) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

const ROUNDS: usize = 5000;

fn derive(salt: &str, password: &str) -> String {
    let mut digest = sha256(format!("{}{}", salt, password).as_bytes());
    for _ in 1..ROUNDS {
        let mut input = Vec::with_capacity(32 + salt.len() + password.len());
        input.extend_from_slice(&digest);
        input.extend_from_slice(salt.as_bytes());
        input.extend_from_slice(password.as_bytes());
        digest = sha256(&input);
    }
    hex(&digest)
}

/// The characters crypt(3) uses for salts and hashes (its base 64).
const B64: &[u8; 64] = b"./0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// A 16-character salt from the kernel's random numbers.
fn salt() -> String {
    let mut bytes = [0u8; 16];
    if getrandom(&mut bytes) != 16 {
        // (No getrandom: the cycle counter and the clock will do.)
        let tsc = unsafe { core::arch::x86_64::_rdtsc() };
        let (s, ns) = monotonic_time();
        let seed = tsc ^ (s << 32) ^ ns ^ (getpid() as u64) << 48;
        bytes.copy_from_slice(&sha256(&seed.to_le_bytes())[..16]);
    }
    bytes.iter().map(|b| B64[(b & 63) as usize] as char).collect()
}

/// `len` bytes of `digest` repeated (SHA-crypt's P and S sequences).
fn repeat_to(digest: &[u8; 32], len: usize) -> Vec<u8> {
    digest.iter().copied().cycle().take(len).collect()
}

/// SHA-256 crypt (Ulrich Drepper's specification, as in glibc and musl):
/// the encoded hash for `password` with `salt` (at most 16 characters
/// used), and `rounds` (None: the default 5000, not written in the hash).
pub fn sha256_crypt(password: &[u8], salt: &[u8], rounds: Option<usize>) -> String {
    let rounds_part = rounds.map_or(String::new(), |r| format!("rounds={}$", r));
    let rounds = rounds.unwrap_or(ROUNDS);
    let salt = &salt[..salt.len().min(16)];
    let (p, s) = (password, salt);
    let b = sha256(&[p, s, p].concat());
    let mut a_in = [p, s].concat();
    a_in.extend(repeat_to(&b, p.len()));
    let mut n = p.len();
    while n > 0 {
        a_in.extend_from_slice(if n & 1 != 0 { &b } else { p });
        n >>= 1;
    }
    let a = sha256(&a_in);
    let p_seq = repeat_to(&sha256(&p.repeat(p.len())), p.len());
    let s_seq = repeat_to(&sha256(&s.repeat(16 + a[0] as usize)), s.len());
    let mut c = a;
    for i in 0..rounds {
        let mut input = Vec::with_capacity(32 + 2 * p_seq.len() + s_seq.len() + 32);
        if i & 1 != 0 { input.extend_from_slice(&p_seq) } else { input.extend_from_slice(&c) }
        if i % 3 != 0 {
            input.extend_from_slice(&s_seq);
        }
        if i % 7 != 0 {
            input.extend_from_slice(&p_seq);
        }
        if i & 1 != 0 { input.extend_from_slice(&c) } else { input.extend_from_slice(&p_seq) }
        c = sha256(&input);
    }
    // The result, in crypt's base 64 and byte order.
    let mut out = String::new();
    let mut put = |b2: u8, b1: u8, b0: u8, n: usize| {
        let mut w = (b2 as u32) << 16 | (b1 as u32) << 8 | b0 as u32;
        for _ in 0..n {
            out.push(B64[(w & 63) as usize] as char);
            w >>= 6;
        }
    };
    for k in 0..10 {
        let (i, j, l) = [(0, 10, 20), (21, 1, 11), (12, 22, 2), (3, 13, 23), (24, 4, 14), (15, 25, 5), (6, 16, 26), (27, 7, 17), (18, 28, 8), (9, 19, 29)][k];
        put(c[i], c[j], c[l], 4);
    }
    put(0, c[31], c[30], 3);
    format!("$5${}{}${}", rounds_part, String::from_utf8_lossy(salt), out)
}

/// Hash a password for /etc/shadow (an empty password gives "").
pub fn hash_password(password: &str) -> String {
    if password.is_empty() {
        return String::new();
    }
    sha256_crypt(password.as_bytes(), salt().as_bytes(), None)
}

/// Compare without stopping at the first difference.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Does `password` match a hash from /etc/shadow?
pub fn check_password(hash: &str, password: &str) -> bool {
    if hash.is_empty() {
        return true; // no password set
    }
    if let Some(rest) = hash.strip_prefix("$5$") {
        // $5$[rounds=N$]SALT$HASH
        let (rounds, rest) = match rest.strip_prefix("rounds=").and_then(|r| r.split_once('$')) {
            Some((n, r)) => (Some(n.parse::<usize>().unwrap_or(ROUNDS).clamp(1000, 999_999_999)), r),
            None => (None, rest),
        };
        let Some((salt, _)) = rest.split_once('$') else { return false };
        return same(&sha256_crypt(password.as_bytes(), salt.as_bytes(), rounds), hash);
    }
    let mut parts = hash.split('$');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        // PalladiumOS's older format
        (Some(""), Some("sha256"), Some(salt), Some(expected)) => same(&derive(salt, password), expected),
        _ => false, // locked ("!" or "*") or an unknown format
    }
}

/// Read a password from the terminal without echoing it.
pub fn read_password(prompt: &str) -> Option<String> {
    print!("{}", prompt);
    let saved = tcgetattr(STDIN).ok();
    if let Some(mut t) = saved {
        t.c_lflag &= !ECHO;
        tcsetattr(STDIN, &t);
    }
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    let ok = loop {
        let n = read(STDIN, &mut byte);
        if n <= 0 {
            break !line.is_empty();
        }
        if byte[0] == b'\n' {
            break true;
        }
        line.push(byte[0]);
    };
    if let Some(t) = saved {
        tcsetattr(STDIN, &t);
    }
    println!();
    ok.then(|| String::from_utf8_lossy(&line).into_owned())
}

/// Switch the process to `user`: groups, gid, then uid (in that order:
/// after setuid, a non-root process couldn't change its groups any more).
pub fn become_user(user: &User) -> Result<(), isize> {
    let mut groups = groups_of(&user.name);
    if !groups.contains(&user.gid) {
        groups.push(user.gid);
    }
    let r = setgroups(&groups);
    if r < 0 {
        return Err(r);
    }
    let r = setresgid(user.gid, user.gid, user.gid);
    if r < 0 {
        return Err(r);
    }
    let r = setresuid(user.uid, user.uid, user.uid);
    if r < 0 { Err(r) } else { Ok(()) }
}

/// The environment for a login shell of `user`.
pub fn login_env(user: &User) -> Vec<String> {
    alloc::vec![
        format!("HOME={}", user.home),
        format!("USER={}", user.name),
        format!("LOGNAME={}", user.name),
        format!("SHELL={}", user.shell),
        String::from("PATH=/bin:/usr/bin"),
        // The console speaks ANSI, and every system (including the other
        // end of an SSH connection) knows the name.
        String::from("TERM=ansi"),
    ]
}

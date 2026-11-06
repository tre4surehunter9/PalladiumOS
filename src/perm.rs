// Copyright (c) 2026 tre4surehunter9
//! Permission checks (POSIX rules).
//!
//! Each file has an owner, a group and nine permission bits: read, write
//! and execute for the owner, the group and everyone else. A process uses
//! the owner bits if it's the owner, the group bits if the file's group is
//! one of its groups, otherwise the "other" bits. Root (effective uid 0)
//! may read and write anything, but needs at least one execute bit to run a
//! file. For a directory, "execute" means "search": using it in a path.

use crate::mounts::{FileType, FsResult, Meta};
use crate::process::Cred;
use crate::syscall::{EACCES, EPERM};

pub const READ: u32 = 4;
pub const WRITE: u32 = 2;
pub const EXEC: u32 = 1;

pub const S_ISUID: u32 = 0o4000;
pub const S_ISGID: u32 = 0o2000;
pub const S_ISVTX: u32 = 0o1000; // sticky

/// May `cred` do `want` (READ|WRITE|EXEC bits) to this file?
pub fn allowed(meta: &Meta, cred: &Cred, want: u32) -> bool {
    if cred.euid == 0 {
        // Root: everything, except running a file with no execute bit.
        return want & EXEC == 0 || meta.kind == FileType::Dir || meta.perm & 0o111 != 0;
    }
    let bits = if cred.euid == meta.uid {
        meta.perm >> 6
    } else if cred.in_group(meta.gid) {
        meta.perm >> 3
    } else {
        meta.perm
    } & 7;
    bits & want == want
}

pub fn check(meta: &Meta, cred: &Cred, want: u32) -> FsResult<()> {
    if allowed(meta, cred, want) { Ok(()) } else { Err(EACCES) }
}

/// Only the owner (or root) may change a file's mode or times.
pub fn check_owner(meta: &Meta, cred: &Cred) -> FsResult<()> {
    if cred.euid == 0 || cred.euid == meta.uid { Ok(()) } else { Err(EPERM) }
}

/// In a sticky directory (like /tmp), only the file's owner, the
/// directory's owner or root may remove or rename the file.
pub fn check_sticky(dir: &Meta, target: &Meta, cred: &Cred) -> FsResult<()> {
    if dir.perm & S_ISVTX == 0 || cred.euid == 0 || cred.euid == dir.uid || cred.euid == target.uid {
        Ok(())
    } else {
        Err(EPERM)
    }
}

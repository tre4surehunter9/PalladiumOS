// Copyright (c) 2026 tre4surehunter9
//! Minimal ELF64 parser for loading static x86_64 user programs.
//!
//! Only what the loader needs: the entry point and the PT_LOAD segments.
//! Everything is bounds-checked, because the bytes come from user programs.

use alloc::vec::Vec;
use crate::memory::USER_SPACE_END;

pub const PF_X: u32 = 1;
pub const PF_W: u32 = 2;
pub const PF_R: u32 = 4;

const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PT_INTERP: u32 = 3;

const ET_EXEC: u16 = 2;
const EM_X86_64: u16 = 62;
const PHDR_SIZE: usize = 56;

/// Lowest address a segment may use (keeps the null page unmapped).
const USER_SPACE_START: u64 = 0x1000;
/// Upper limit on the memory of all segments combined.
const MAX_IMAGE_SIZE: u64 = 16 * 1024 * 1024;

pub struct Segment<'a> {
    pub vaddr: u64,
    pub memsz: u64,
    /// The first `filesz` bytes; the rest (up to memsz) is zero-filled (.bss).
    pub data: &'a [u8],
    pub flags: u32,
}

pub struct Elf<'a> {
    pub entry: u64,
    pub segments: Vec<Segment<'a>>,
}

pub fn is_elf(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x7fELF")
}

fn u16_at(b: &[u8], off: usize) -> Result<u16, &'static str> {
    let s = b.get(off..off + 2).ok_or("truncated ELF")?;
    Ok(u16::from_le_bytes([s[0], s[1]]))
}

fn u32_at(b: &[u8], off: usize) -> Result<u32, &'static str> {
    let s = b.get(off..off + 4).ok_or("truncated ELF")?;
    Ok(u32::from_le_bytes(s.try_into().unwrap()))
}

fn u64_at(b: &[u8], off: usize) -> Result<u64, &'static str> {
    let s = b.get(off..off + 8).ok_or("truncated ELF")?;
    Ok(u64::from_le_bytes(s.try_into().unwrap()))
}

pub fn parse(b: &[u8]) -> Result<Elf<'_>, &'static str> {
    if !is_elf(b) {
        return Err("not an ELF file");
    }
    if b.len() < 64 {
        return Err("truncated ELF");
    }
    if b[4] != 2 {
        return Err("not a 64-bit ELF");
    }
    if b[5] != 1 {
        return Err("not a little-endian ELF");
    }
    if u16_at(b, 16)? != ET_EXEC {
        return Err("not a static executable (ET_EXEC)");
    }
    if u16_at(b, 18)? != EM_X86_64 {
        return Err("not an x86_64 ELF");
    }

    let entry = u64_at(b, 24)?;
    let phoff = u64_at(b, 32)? as usize;
    let phentsize = u16_at(b, 54)? as usize;
    let phnum = u16_at(b, 56)? as usize;
    if phentsize != PHDR_SIZE {
        return Err("unexpected program header size");
    }

    let mut segments = Vec::new();
    let mut total: u64 = 0;
    for i in 0..phnum {
        let ph = phoff
            .checked_add(i.checked_mul(PHDR_SIZE).ok_or("bad program headers")?)
            .ok_or("bad program headers")?;
        let p_type = u32_at(b, ph)?;
        match p_type {
            PT_INTERP | PT_DYNAMIC => return Err("dynamically linked programs are not supported"),
            PT_LOAD => {}
            _ => continue,
        }

        let flags = u32_at(b, ph + 4)?;
        let offset = u64_at(b, ph + 8)?;
        let vaddr = u64_at(b, ph + 16)?;
        let filesz = u64_at(b, ph + 32)?;
        let memsz = u64_at(b, ph + 40)?;

        if memsz == 0 {
            continue;
        }
        if filesz > memsz {
            return Err("segment file size larger than memory size");
        }
        let end = vaddr.checked_add(memsz).ok_or("segment address overflow")?;
        if vaddr < USER_SPACE_START || end > USER_SPACE_END {
            return Err("segment outside user space");
        }
        total = total.checked_add(memsz).ok_or("segment too large")?;
        if total > MAX_IMAGE_SIZE {
            return Err("program too large");
        }
        let start = offset as usize;
        let data = start
            .checked_add(filesz as usize)
            .and_then(|stop| b.get(start..stop))
            .ok_or("segment data outside file")?;

        segments.push(Segment { vaddr, memsz, data, flags });
    }

    if segments.is_empty() {
        return Err("no loadable segments");
    }
    let entry_ok = segments.iter().any(|s| {
        s.flags & PF_X != 0 && entry >= s.vaddr && entry < s.vaddr + s.memsz
    });
    if !entry_ok {
        return Err("entry point is not in an executable segment");
    }

    Ok(Elf { entry, segments })
}

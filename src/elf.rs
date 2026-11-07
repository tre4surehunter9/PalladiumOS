// Copyright (c) 2026 tre4surehunter9
//! Minimal ELF64 parser for loading x86_64 user programs.
//!
//! Only what the loader needs: the entry point, the PT_LOAD segments, and
//! the program interpreter (PT_INTERP) of a dynamically linked program,
//! which the kernel loads too (at INTERP_BASE) and starts instead.
//! Everything is bounds-checked, because the bytes come from user programs.

use alloc::string::String;
use alloc::vec::Vec;
use crate::memory::USER_SPACE_END;

pub const PF_X: u32 = 1;
pub const PF_W: u32 = 2;
pub const PF_R: u32 = 4;

const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PT_INTERP: u32 = 3;

const ET_EXEC: u16 = 2;
const ET_DYN: u16 = 3;
/// Where position-independent programs (static-PIE, or dynamically linked
/// PIE) are loaded.
pub const PIE_BASE: u64 = 0x80_0000_0000;
/// Where a program's interpreter (the dynamic linker) is loaded: above the
/// mmap area, below the stack.
pub const INTERP_BASE: u64 = 0x7f00_0000_0000;
const EM_X86_64: u16 = 62;
const PHDR_SIZE: usize = 56;

/// Lowest address a segment may use (keeps the null page unmapped).
const USER_SPACE_START: u64 = 0x1000;
/// Upper limit on the memory of all segments combined.
const MAX_IMAGE_SIZE: u64 = 1024 * 1024 * 1024;

pub struct Segment {
    pub vaddr: u64,
    pub memsz: u64,
    /// Where its first `filesz` bytes are in the file; the rest (up to
    /// memsz) is zero-filled (.bss).
    pub offset: u64,
    pub filesz: u64,
    pub flags: u32,
}

/// A parsed ELF file. The parser only needs the start of the file (the
/// headers, see HEAD_SIZE); segments say where their data is in the file.
pub struct Elf {
    pub entry: u64,
    pub segments: Vec<Segment>,
    /// Where the program headers are in memory (0 if not loaded), their
    /// count and size: for the aux vector (libc finds TLS etc. there).
    pub phdr: u64,
    pub phnum: u64,
    pub phent: u64,
    /// PT_INTERP: the dynamic linker to load with it (e.g.
    /// /lib/ld-musl-x86_64.so.1).
    pub interp: Option<String>,
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

/// A program: ET_EXEC where it says, ET_DYN at PIE_BASE.
/// How much of the start of a file the parser gets: enough for the ELF
/// header, the program headers and PT_INTERP in any normal program.
pub const HEAD_SIZE: usize = 64 * 1024;

/// `b`: the start of the file; `file_size`: the whole file's size.
pub fn parse(b: &[u8], file_size: u64) -> Result<Elf, &'static str> {
    parse_with(b, file_size, PIE_BASE)
}

/// A program's interpreter: a shared object (ET_DYN), loaded at INTERP_BASE.
pub fn parse_interp(b: &[u8], file_size: u64) -> Result<Elf, &'static str> {
    if b.len() >= 18 && u16_at(b, 16)? != ET_DYN {
        return Err("the program interpreter isn't a shared object");
    }
    let elf = parse_with(b, file_size, INTERP_BASE)?;
    if elf.interp.is_some() {
        return Err("the program interpreter has an interpreter itself");
    }
    Ok(elf)
}

fn parse_with(b: &[u8], file_size: u64, dyn_base: u64) -> Result<Elf, &'static str> {
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
    // ET_EXEC is loaded where it says; ET_DYN (position-independent: it,
    // or its dynamic linker, relocates it) at `dyn_base`.
    let bias = match u16_at(b, 16)? {
        ET_EXEC => 0,
        ET_DYN => dyn_base,
        _ => return Err("not an executable"),
    };
    if u16_at(b, 18)? != EM_X86_64 {
        return Err("not an x86_64 ELF");
    }

    let entry = u64_at(b, 24)?.wrapping_add(bias);
    let phoff = u64_at(b, 32)? as usize;
    let phentsize = u16_at(b, 54)? as usize;
    let phnum = u16_at(b, 56)? as usize;
    if phentsize != PHDR_SIZE {
        return Err("unexpected program header size");
    }

    let mut segments = Vec::new();
    let mut total: u64 = 0;
    let mut phdr = 0;
    let mut interp = None;
    for i in 0..phnum {
        let ph = phoff
            .checked_add(i.checked_mul(PHDR_SIZE).ok_or("bad program headers")?)
            .ok_or("bad program headers")?;
        let p_type = u32_at(b, ph)?;
        match p_type {
            PT_INTERP => {
                let offset = u64_at(b, ph + 8)? as usize;
                let size = u64_at(b, ph + 32)? as usize;
                let bytes = offset.checked_add(size).and_then(|end| b.get(offset..end)).ok_or("bad PT_INTERP")?;
                let path = bytes.split(|&c| c == 0).next().unwrap_or(&[]);
                if path.is_empty() || path.len() > 255 {
                    return Err("bad PT_INTERP");
                }
                interp = Some(String::from_utf8_lossy(path).into_owned());
                continue;
            }
            PT_DYNAMIC => continue, // relocations: the program or its dynamic linker applies them
            PT_LOAD => {}
            _ => continue,
        }

        let flags = u32_at(b, ph + 4)?;
        let offset = u64_at(b, ph + 8)?;
        let vaddr = u64_at(b, ph + 16)?.checked_add(bias).ok_or("segment address overflow")?;
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
        if offset.checked_add(filesz).is_none_or(|stop| stop > file_size) {
            return Err("segment data outside file");
        }

        if (offset..offset + filesz).contains(&(phoff as u64)) && phoff as u64 + (phnum * PHDR_SIZE) as u64 <= offset + filesz {
            phdr = vaddr + (phoff as u64 - offset);
        }
        segments.push(Segment { vaddr, memsz, offset, filesz, flags });
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

    Ok(Elf { entry, segments, phdr, phnum: phnum as u64, phent: PHDR_SIZE as u64, interp })
}

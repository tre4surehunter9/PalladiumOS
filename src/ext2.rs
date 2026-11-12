// Copyright (c) 2026 tre4surehunter9
//! ext2: the classic Unix filesystem, readable and writable by Linux.
//!
//! Layout: the disk is split into block groups. Each group has a block
//! bitmap, an inode bitmap and an inode table; the superblock (at byte
//! 1024) and the group descriptor table say where everything is. A file
//! is an inode: its metadata plus 15 block pointers (12 direct, then a
//! single, double and triple indirect block). A directory is a file of
//! variable-length entries (inode, record length, name).
//!
//! Supported: any block size, 128/256-byte inodes, the `filetype` feature,
//! sparse superblocks and large files. Disks with other incompatible
//! features (ext3 journals, ext4 extents) are refused; unknown read-only
//! features mount read-only. Directory hash indexes (`dir_index`) aren't
//! used: when a directory is modified its index flag is cleared, so Linux
//! falls back to a linear scan.
//!
//! Writes go to the disk by the end of each operation (a cache keeps recently
//! used blocks in memory to speed up reads). Within one write or truncate,
//! the allocation bookkeeping (block bitmaps, group descriptors, superblock)
//! is updated in memory and written once at the end, not once per block,
//! and new data blocks aren't zeroed on disk first when their contents are
//! written anyway.

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::block::{BlockDevice, SECTOR_SIZE};
use crate::mounts::{FileSystem, FileType, FsResult, FsStats, Ino, Meta};
use crate::syscall::{EEXIST, EFBIG, EINVAL, EIO, EISDIR, ENOENT, ENOTDIR, ENOTEMPTY, EPERM};

const EROFS: i64 = 30;
const ENOSPC: i64 = 28;
const ENAMETOOLONG: i64 = 36;

const MAGIC: u16 = 0xEF53;
const ROOT_INO: Ino = 2;

const INCOMPAT_FILETYPE: u32 = 0x0002;
const RO_COMPAT_SPARSE_SUPER: u32 = 0x0001;
const RO_COMPAT_LARGE_FILE: u32 = 0x0002;

const S_IFMT: u16 = 0xF000;
const S_IFDIR: u16 = 0x4000;
const S_IFREG: u16 = 0x8000;
const S_IFLNK: u16 = 0xA000;
const S_IFSOCK: u16 = 0xC000;
/// Symlink targets shorter than this live in the inode itself.
const FAST_SYMLINK_MAX: usize = 60;
const INDEX_FL: u32 = 0x1000;

const FT_REG: u8 = 1;
const FT_DIR: u8 = 2;
const FT_SOCK: u8 = 6;
const FT_SYMLINK: u8 = 7;

const CACHE_BLOCKS: usize = 1024;

#[derive(Clone, Copy)]
struct Group {
    block_bitmap: u32,
    inode_bitmap: u32,
    inode_table: u32,
    free_blocks: u16,
    free_inodes: u16,
    used_dirs: u16,
}

/// An inode's raw bytes, with accessors for the fields we use.
struct Inode {
    raw: Vec<u8>,
}

fn rd16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn rd32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn wr16(b: &mut [u8], o: usize, v: u16) {
    b[o..o + 2].copy_from_slice(&v.to_le_bytes());
}
fn wr32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

impl Inode {
    fn mode(&self) -> u16 {
        rd16(&self.raw, 0)
    }
    fn is_dir(&self) -> bool {
        self.mode() & S_IFMT == S_IFDIR
    }
    fn is_symlink(&self) -> bool {
        self.mode() & S_IFMT == S_IFLNK
    }
    fn sectors(&self) -> u32 {
        rd32(&self.raw, 28)
    }
    /// A symlink whose target is stored in the block pointers (no blocks).
    fn is_fast_symlink(&self) -> bool {
        self.is_symlink() && self.sectors() == 0
    }
    fn size(&self) -> u64 {
        let high = if self.mode() & S_IFMT == S_IFREG { rd32(&self.raw, 108) as u64 } else { 0 };
        rd32(&self.raw, 4) as u64 | high << 32
    }
    fn set_size(&mut self, size: u64) {
        wr32(&mut self.raw, 4, size as u32);
        if self.mode() & S_IFMT == S_IFREG {
            wr32(&mut self.raw, 108, (size >> 32) as u32);
        }
    }
    fn links(&self) -> u16 {
        rd16(&self.raw, 26)
    }
    fn set_links(&mut self, n: u16) {
        wr16(&mut self.raw, 26, n);
    }
    fn block(&self, i: usize) -> u32 {
        rd32(&self.raw, 40 + 4 * i)
    }
    fn set_block(&mut self, i: usize, v: u32) {
        wr32(&mut self.raw, 40 + 4 * i, v);
    }
    /// i_blocks counts 512-byte sectors.
    fn add_sectors(&mut self, delta: i64) {
        let v = (rd32(&self.raw, 28) as i64 + delta).max(0);
        wr32(&mut self.raw, 28, v as u32);
    }
    fn mtime(&self) -> u32 {
        rd32(&self.raw, 16)
    }
    fn touch(&mut self) {
        let now = crate::rtc::now() as u32;
        wr32(&mut self.raw, 12, now); // ctime
        wr32(&mut self.raw, 16, now); // mtime
    }
    /// Owner: low 16 bits at offset 2, high 16 bits at 120 (Linux layout).
    fn uid(&self) -> u32 {
        rd16(&self.raw, 2) as u32 | (rd16(&self.raw, 120) as u32) << 16
    }
    fn gid(&self) -> u32 {
        rd16(&self.raw, 24) as u32 | (rd16(&self.raw, 122) as u32) << 16
    }
    fn set_owner(&mut self, uid: u32, gid: u32) {
        wr16(&mut self.raw, 2, uid as u16);
        wr16(&mut self.raw, 120, (uid >> 16) as u16);
        wr16(&mut self.raw, 24, gid as u16);
        wr16(&mut self.raw, 122, (gid >> 16) as u16);
    }
    fn flags(&self) -> u32 {
        rd32(&self.raw, 32)
    }
    fn set_flags(&mut self, f: u32) {
        wr32(&mut self.raw, 32, f);
    }
}

pub struct Ext2 {
    dev: Box<dyn BlockDevice>,
    sb: Vec<u8>,
    groups: Vec<Group>,
    block_size: usize,
    first_data_block: u32,
    blocks_count: u32,
    blocks_per_group: u32,
    inodes_per_group: u32,
    inode_size: usize,
    first_ino: u32,
    filetype: bool,
    read_only: bool,
    cache: BTreeMap<u64, Vec<u8>>,
    /// Cached block numbers, oldest first (evicted in that order).
    cache_order: VecDeque<u64>,
    /// During a batch (see begin_batch): blocks waiting to be written
    /// (bitmaps, new blocks still all zeros). Never evicted; reads see them.
    dirty: BTreeMap<u64, Vec<u8>>,
    batching: bool,
    dirty_groups: BTreeSet<usize>,
    sb_dirty: bool,
    /// Where to start looking for a free block (the one after the last
    /// allocated): files get consecutive blocks, and the search doesn't
    /// rescan the full part of the disk every time.
    alloc_hint: u32,
    /// How many open files refer to each inode.
    open: BTreeMap<Ino, usize>,
    /// Inodes with no links left that are still open: freed on last close.
    orphans: BTreeSet<Ino>,
}

impl Ext2 {
    /// Mount the ext2 filesystem on `dev`.
    pub fn mount(mut dev: Box<dyn BlockDevice>) -> Result<Ext2, &'static str> {
        let mut sb = vec![0u8; 1024];
        dev.read(2, &mut sb).map_err(|_| "can't read the superblock")?;
        if rd16(&sb, 56) != MAGIC {
            return Err("not an ext2 filesystem");
        }
        let rev = rd32(&sb, 76);
        let (inode_size, first_ino, incompat, ro_compat) = if rev >= 1 {
            (rd16(&sb, 88) as usize, rd32(&sb, 84), rd32(&sb, 96), rd32(&sb, 100))
        } else {
            (128, 11, 0, 0)
        };
        if incompat & !INCOMPAT_FILETYPE != 0 {
            return Err("unsupported filesystem features (ext3/ext4?)");
        }
        let read_only = ro_compat & !(RO_COMPAT_SPARSE_SUPER | RO_COMPAT_LARGE_FILE) != 0;
        let log = rd32(&sb, 24);
        if log > 6 {
            return Err("bad block size");
        }
        let block_size = 1024usize << log;
        let blocks_count = rd32(&sb, 4);
        let first_data_block = rd32(&sb, 20);
        let blocks_per_group = rd32(&sb, 32);
        let inodes_per_group = rd32(&sb, 40);
        if blocks_per_group == 0 || inodes_per_group == 0 || inode_size < 128 || inode_size > block_size {
            return Err("bad superblock");
        }
        let group_count = (blocks_count - first_data_block).div_ceil(blocks_per_group) as usize;

        let mut fs = Ext2 {
            dev,
            sb,
            groups: Vec::new(),
            block_size,
            first_data_block,
            blocks_count,
            blocks_per_group,
            inodes_per_group,
            inode_size,
            first_ino,
            filetype: incompat & INCOMPAT_FILETYPE != 0,
            read_only,
            cache: BTreeMap::new(),
            cache_order: VecDeque::new(),
            dirty: BTreeMap::new(),
            batching: false,
            dirty_groups: BTreeSet::new(),
            sb_dirty: false,
            alloc_hint: 0,
            open: BTreeMap::new(),
            orphans: BTreeSet::new(),
        };
        let gdt_bytes = group_count * 32;
        let gdt_blocks = gdt_bytes.div_ceil(block_size);
        let mut table = Vec::with_capacity(gdt_blocks * block_size);
        for i in 0..gdt_blocks {
            table.extend(fs.read_block(fs.gdt_block() + i as u64).map_err(|_| "can't read group descriptors")?);
        }
        for g in 0..group_count {
            let d = &table[g * 32..g * 32 + 32];
            fs.groups.push(Group {
                block_bitmap: rd32(d, 0),
                inode_bitmap: rd32(d, 4),
                inode_table: rd32(d, 8),
                free_blocks: rd16(d, 12),
                free_inodes: rd16(d, 14),
                used_dirs: rd16(d, 16),
            });
        }
        Ok(fs)
    }

    /// (free blocks, free inodes), from the superblock.
    pub fn free_counts(&self) -> (u32, u32) {
        (rd32(&self.sb, 12), rd32(&self.sb, 16))
    }

    /// Refuse every change from now on (for the system disk at /usr).
    pub fn make_read_only(&mut self) {
        self.read_only = true;
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    fn gdt_block(&self) -> u64 {
        self.first_data_block as u64 + 1
    }

    // ---- blocks ----

    fn read_block(&mut self, n: u64) -> FsResult<Vec<u8>> {
        if let Some(b) = self.dirty.get(&n) {
            return Ok(b.clone());
        }
        if let Some(b) = self.cache.get(&n) {
            return Ok(b.clone());
        }
        let mut buf = vec![0u8; self.block_size];
        self.dev.read(n * (self.block_size / SECTOR_SIZE) as u64, &mut buf).map_err(|e| {
            crate::boot_warn!("ext2: reading block {} failed: {}", n, e);
            EIO
        })?;
        self.cache_put(n, buf.clone());
        Ok(buf)
    }

    fn write_block(&mut self, n: u64, data: &[u8]) -> FsResult<()> {
        if self.read_only {
            return Err(EROFS);
        }
        self.dev.write(n * (self.block_size / SECTOR_SIZE) as u64, data).map_err(|e| {
            crate::boot_warn!("ext2: writing block {} failed: {}", n, e);
            EIO
        })?;
        // (Its real contents are on disk now: a pending zero-fill is moot.)
        self.dirty.remove(&n);
        self.cache_put(n, data.to_vec());
        Ok(())
    }

    fn cache_put(&mut self, n: u64, data: Vec<u8>) {
        if self.cache.insert(n, data).is_none() {
            self.cache_order.push_back(n);
            while self.cache.len() > CACHE_BLOCKS {
                match self.cache_order.pop_front() {
                    Some(victim) => {
                        self.cache.remove(&victim);
                    }
                    None => break,
                }
            }
        }
    }

    /// Start a batch: bookkeeping changes wait in memory until end_batch.
    fn begin_batch(&mut self) {
        self.batching = true;
    }

    /// Write what the batch changed: bitmaps and zeroed blocks, the group
    /// descriptors, and the superblock (each once).
    fn end_batch(&mut self) -> FsResult<()> {
        self.batching = false;
        let mut result = Ok(());
        for (n, data) in core::mem::take(&mut self.dirty) {
            if let Err(e) = self.write_block(n, &data) {
                result = Err(e);
            }
        }
        for g in core::mem::take(&mut self.dirty_groups) {
            if let Err(e) = self.write_group(g) {
                result = Err(e);
            }
        }
        if core::mem::take(&mut self.sb_dirty) {
            if let Err(e) = self.write_superblock() {
                result = Err(e);
            }
        }
        result
    }

    /// A bookkeeping block changed: written now, or at the end of the batch.
    fn put_meta(&mut self, n: u64, data: Vec<u8>) -> FsResult<()> {
        if self.batching {
            self.cache_put(n, data.clone());
            self.dirty.insert(n, data);
            Ok(())
        } else {
            self.write_block(n, &data)
        }
    }

    fn group_changed(&mut self, g: usize) -> FsResult<()> {
        if self.batching {
            self.dirty_groups.insert(g);
            Ok(())
        } else {
            self.write_group(g)
        }
    }

    fn sb_changed(&mut self) -> FsResult<()> {
        if self.batching {
            self.sb_dirty = true;
            Ok(())
        } else {
            self.write_superblock()
        }
    }

    fn write_superblock(&mut self) -> FsResult<()> {
        if self.read_only {
            return Err(EROFS);
        }
        let sb = self.sb.clone();
        self.dev.write(2, &sb).map_err(|_| EIO)
    }

    fn write_group(&mut self, g: usize) -> FsResult<()> {
        let byte = g * 32;
        let block = self.gdt_block() + (byte / self.block_size) as u64;
        let off = byte % self.block_size;
        let mut data = self.read_block(block)?;
        let grp = self.groups[g];
        wr16(&mut data, off + 12, grp.free_blocks);
        wr16(&mut data, off + 14, grp.free_inodes);
        wr16(&mut data, off + 16, grp.used_dirs);
        self.write_block(block, &data)
    }

    fn alloc_block(&mut self) -> FsResult<u32> {
        if self.read_only {
            return Err(EROFS);
        }
        // Start where the last allocation left off, and go round once.
        let groups = self.groups.len();
        let hint = self.alloc_hint.max(self.first_data_block).min(self.blocks_count.saturating_sub(1));
        let start_g = ((hint - self.first_data_block) / self.blocks_per_group) as usize;
        let start_bit = (hint - self.first_data_block) % self.blocks_per_group;
        for k in 0..=groups {
            let g = (start_g + k) % groups;
            if self.groups[g].free_blocks == 0 {
                continue;
            }
            let bitmap_block = self.groups[g].block_bitmap as u64;
            let mut bitmap = self.read_block(bitmap_block)?;
            // (In the first group, from the hint; the second pass over it,
            // at the end, covers what's before the hint.)
            let from = if k == 0 { start_bit } else { 0 };
            let mut bit = from;
            while bit < self.blocks_per_group {
                let (byte, mask) = ((bit / 8) as usize, 1u8 << (bit % 8));
                if bit % 8 == 0 && bitmap[byte] == 0xff {
                    bit += 8; // a full byte: skip it
                    continue;
                }
                let block = self.first_data_block + g as u32 * self.blocks_per_group + bit;
                if block >= self.blocks_count {
                    break;
                }
                if bitmap[byte] & mask == 0 {
                    bitmap[byte] |= mask;
                    self.put_meta(bitmap_block, bitmap)?;
                    self.groups[g].free_blocks -= 1;
                    self.group_changed(g)?;
                    let free = rd32(&self.sb, 12).saturating_sub(1);
                    wr32(&mut self.sb, 12, free);
                    self.sb_changed()?;
                    // A new block reads as zeros. In a batch the zeros wait
                    // in memory: usually the block's contents are written
                    // right away, and then they never need to be.
                    let zero = vec![0u8; self.block_size];
                    if self.batching {
                        self.cache_put(block as u64, zero.clone());
                        self.dirty.insert(block as u64, zero);
                    } else {
                        self.write_block(block as u64, &zero)?;
                    }
                    self.alloc_hint = block + 1;
                    return Ok(block);
                }
                bit += 1;
            }
        }
        Err(ENOSPC)
    }

    fn free_block(&mut self, block: u32) -> FsResult<()> {
        if block < self.first_data_block || block >= self.blocks_count {
            return Err(EIO);
        }
        let rel = block - self.first_data_block;
        let g = (rel / self.blocks_per_group) as usize;
        let bit = rel % self.blocks_per_group;
        let bitmap_block = self.groups[g].block_bitmap as u64;
        let mut bitmap = self.read_block(bitmap_block)?;
        bitmap[(bit / 8) as usize] &= !(1u8 << (bit % 8));
        self.put_meta(bitmap_block, bitmap)?;
        // (Drop any copy of it, including a zero-fill still waiting.)
        if self.cache.remove(&(block as u64)).is_some() {
            self.cache_order.retain(|&b| b != block as u64);
        }
        self.dirty.remove(&(block as u64));
        self.groups[g].free_blocks += 1;
        self.group_changed(g)?;
        let free = rd32(&self.sb, 12) + 1;
        wr32(&mut self.sb, 12, free);
        self.sb_changed()
    }

    // ---- inodes ----

    fn inode_location(&self, ino: Ino) -> FsResult<(u64, usize)> {
        if ino == 0 || ino > self.groups.len() as u64 * self.inodes_per_group as u64 {
            return Err(ENOENT);
        }
        let index = (ino - 1) as u32;
        let g = (index / self.inodes_per_group) as usize;
        let byte = (index % self.inodes_per_group) as usize * self.inode_size;
        Ok((self.groups[g].inode_table as u64 + (byte / self.block_size) as u64, byte % self.block_size))
    }

    fn inode(&mut self, ino: Ino) -> FsResult<Inode> {
        let (block, off) = self.inode_location(ino)?;
        let data = self.read_block(block)?;
        Ok(Inode { raw: data[off..off + self.inode_size].to_vec() })
    }

    fn write_inode(&mut self, ino: Ino, inode: &Inode) -> FsResult<()> {
        let (block, off) = self.inode_location(ino)?;
        let mut data = self.read_block(block)?;
        data[off..off + self.inode_size].copy_from_slice(&inode.raw);
        self.write_block(block, &data)
    }

    fn alloc_inode(&mut self, is_dir: bool) -> FsResult<Ino> {
        if self.read_only {
            return Err(EROFS);
        }
        for g in 0..self.groups.len() {
            if self.groups[g].free_inodes == 0 {
                continue;
            }
            let bitmap_block = self.groups[g].inode_bitmap as u64;
            let mut bitmap = self.read_block(bitmap_block)?;
            for bit in 0..self.inodes_per_group {
                let ino = g as u32 * self.inodes_per_group + bit + 1;
                let (byte, mask) = ((bit / 8) as usize, 1u8 << (bit % 8));
                if ino < self.first_ino || bitmap[byte] & mask != 0 {
                    continue;
                }
                bitmap[byte] |= mask;
                self.write_block(bitmap_block, &bitmap)?;
                self.groups[g].free_inodes -= 1;
                if is_dir {
                    self.groups[g].used_dirs += 1;
                }
                self.write_group(g)?;
                let free = rd32(&self.sb, 16).saturating_sub(1);
                wr32(&mut self.sb, 16, free);
                self.write_superblock()?;
                return Ok(ino as Ino);
            }
        }
        Err(ENOSPC)
    }

    fn free_inode(&mut self, ino: Ino, is_dir: bool) -> FsResult<()> {
        let index = (ino - 1) as u32;
        let g = (index / self.inodes_per_group) as usize;
        let bit = index % self.inodes_per_group;
        let bitmap_block = self.groups[g].inode_bitmap as u64;
        let mut bitmap = self.read_block(bitmap_block)?;
        bitmap[(bit / 8) as usize] &= !(1u8 << (bit % 8));
        self.write_block(bitmap_block, &bitmap)?;
        self.groups[g].free_inodes += 1;
        if is_dir {
            self.groups[g].used_dirs = self.groups[g].used_dirs.saturating_sub(1);
        }
        self.write_group(g)?;
        let free = rd32(&self.sb, 16) + 1;
        wr32(&mut self.sb, 16, free);
        self.write_superblock()
    }

    // ---- file blocks ----

    fn per_block(&self) -> u64 {
        (self.block_size / 4) as u64
    }

    /// The disk block holding block `fb` of a file (0 = a hole). With
    /// `alloc`, missing blocks (and indirect blocks) are allocated.
    fn bmap(&mut self, inode: &mut Inode, fb: u64, alloc: bool) -> FsResult<u32> {
        let per = self.per_block();
        if fb < 12 {
            let mut b = inode.block(fb as usize);
            if b == 0 && alloc {
                b = self.alloc_block()?;
                inode.set_block(fb as usize, b);
                inode.add_sectors((self.block_size / SECTOR_SIZE) as i64);
            }
            return Ok(b);
        }
        let mut rel = fb - 12;
        let mut depth = 1u32;
        let mut span = per;
        while rel >= span {
            rel -= span;
            depth += 1;
            if depth > 3 {
                return Err(EFBIG);
            }
            span *= per;
        }
        let slot = 11 + depth as usize; // 12, 13 or 14
        let mut cur = inode.block(slot);
        if cur == 0 {
            if !alloc {
                return Ok(0);
            }
            cur = self.alloc_block()?;
            inode.set_block(slot, cur);
            inode.add_sectors((self.block_size / SECTOR_SIZE) as i64);
        }
        for level in (0..depth).rev() {
            let index = ((rel / per.pow(level)) % per) as usize;
            let mut data = self.read_block(cur as u64)?;
            let mut next = rd32(&data, index * 4);
            if next == 0 {
                if !alloc {
                    return Ok(0);
                }
                next = self.alloc_block()?;
                wr32(&mut data, index * 4, next);
                self.write_block(cur as u64, &data)?;
                inode.add_sectors((self.block_size / SECTOR_SIZE) as i64);
            }
            cur = next;
        }
        Ok(cur)
    }

    /// Free a block tree (an indirect block of `depth` levels, or a data
    /// block when depth is 0). Returns how many blocks were freed.
    fn free_tree(&mut self, block: u32, depth: u32) -> FsResult<u64> {
        let mut freed = 0;
        if depth > 0 {
            let data = self.read_block(block as u64)?;
            for i in 0..self.per_block() as usize {
                let child = rd32(&data, i * 4);
                if child != 0 {
                    freed += self.free_tree(child, depth - 1)?;
                }
            }
        }
        self.free_block(block)?;
        Ok(freed + 1)
    }

    /// Free blocks from file block `from` on. Whole indirect trees are freed
    /// when the cut is at 0; otherwise just the data blocks past it.
    /// Free a file's blocks from `from` on (batched: the bitmaps and counts
    /// are written once, at the end).
    fn free_blocks_from(&mut self, inode: &mut Inode, from: u64) -> FsResult<()> {
        if self.batching {
            return self.free_blocks_from_inner(inode, from);
        }
        self.begin_batch();
        let r = self.free_blocks_from_inner(inode, from);
        let flushed = self.end_batch();
        r.and(flushed)
    }

    fn free_blocks_from_inner(&mut self, inode: &mut Inode, from: u64) -> FsResult<()> {
        if inode.is_fast_symlink() {
            // The "block pointers" are the target text, not blocks.
            if from == 0 {
                inode.raw[40..100].fill(0);
            }
            return Ok(());
        }
        let sectors = (self.block_size / SECTOR_SIZE) as i64;
        if from == 0 {
            for i in 0..15 {
                let b = inode.block(i);
                if b != 0 {
                    let depth = if i < 12 { 0 } else { i as u32 - 11 };
                    let n = self.free_tree(b, depth)?;
                    inode.add_sectors(-(n as i64) * sectors);
                    inode.set_block(i, 0);
                }
            }
            return Ok(());
        }
        let end = inode.size().div_ceil(self.block_size as u64);
        for fb in from..end {
            if fb < 12 {
                let b = inode.block(fb as usize);
                if b != 0 {
                    self.free_block(b)?;
                    inode.set_block(fb as usize, 0);
                    inode.add_sectors(-sectors);
                }
            } else if self.bmap(inode, fb, false)? != 0 {
                self.unmap_indirect(inode, fb)?;
                inode.add_sectors(-sectors);
            }
        }
        Ok(())
    }

    /// Free the data block at file block `fb` (>= 12) and clear its pointer.
    /// (Empty indirect blocks are kept; they're freed with the whole file.)
    fn unmap_indirect(&mut self, inode: &mut Inode, fb: u64) -> FsResult<()> {
        let per = self.per_block();
        let mut rel = fb - 12;
        let mut depth = 1u32;
        let mut span = per;
        while rel >= span {
            rel -= span;
            depth += 1;
            span *= per;
        }
        let mut cur = inode.block(11 + depth as usize);
        for level in (0..depth).rev() {
            let index = ((rel / per.pow(level)) % per) as usize;
            let mut data = self.read_block(cur as u64)?;
            let next = rd32(&data, index * 4);
            if level == 0 {
                wr32(&mut data, index * 4, 0);
                self.write_block(cur as u64, &data)?;
                return self.free_block(next);
            }
            cur = next;
        }
        Ok(())
    }

    // ---- directories ----

    /// Every entry in a directory: (name, inode, type, block index, offset).
    fn entries(&mut self, dir: &mut Inode) -> FsResult<Vec<(String, Ino, u8, u64, usize)>> {
        let mut out = Vec::new();
        let blocks = dir.size().div_ceil(self.block_size as u64);
        for fb in 0..blocks {
            let phys = self.bmap(dir, fb, false)?;
            if phys == 0 {
                continue;
            }
            let data = self.read_block(phys as u64)?;
            let mut off = 0;
            while off + 8 <= self.block_size {
                let ino = rd32(&data, off);
                let rec_len = rd16(&data, off + 4) as usize;
                let name_len = data[off + 6] as usize;
                if rec_len < 8 || off + rec_len > self.block_size {
                    return Err(EIO); // corrupt directory
                }
                if ino != 0 && off + 8 + name_len <= self.block_size {
                    let name = String::from_utf8_lossy(&data[off + 8..off + 8 + name_len]).into_owned();
                    out.push((name, ino as Ino, data[off + 7], fb, off));
                }
                off += rec_len;
            }
        }
        Ok(out)
    }

    fn file_type_byte(&self, kind: FileType) -> u8 {
        if !self.filetype {
            return 0;
        }
        match kind {
            FileType::Dir => FT_DIR,
            FileType::Symlink => FT_SYMLINK,
            FileType::Socket => FT_SOCK,
            _ => FT_REG,
        }
    }

    fn kind_of(inode: &Inode) -> FileType {
        match inode.mode() & S_IFMT {
            S_IFDIR => FileType::Dir,
            S_IFLNK => FileType::Symlink,
            S_IFSOCK => FileType::Socket,
            _ => FileType::File,
        }
    }

    /// Point a directory's ".." at `parent`.
    fn set_dotdot(&mut self, dir_ino: Ino, parent: Ino) -> FsResult<()> {
        let mut dir = self.inode(dir_ino)?;
        let (_, _, _, fb, off) = self.entries(&mut dir)?.into_iter().find(|e| e.0 == "..").ok_or(EIO)?;
        let phys = self.bmap(&mut dir, fb, false)?;
        let mut data = self.read_block(phys as u64)?;
        wr32(&mut data, off, parent as u32);
        self.write_block(phys as u64, &data)
    }

    fn add_entry(&mut self, dir_ino: Ino, name: &str, ino: Ino, ftype: u8) -> FsResult<()> {
        let mut dir = self.inode(dir_ino)?;
        let need = (8 + name.len()).next_multiple_of(4);
        let blocks = dir.size().div_ceil(self.block_size as u64);
        let write_entry = |data: &mut [u8], off: usize, rec_len: usize| {
            wr32(data, off, ino as u32);
            wr16(data, off + 4, rec_len as u16);
            data[off + 6] = name.len() as u8;
            data[off + 7] = ftype;
            data[off + 8..off + 8 + name.len()].copy_from_slice(name.as_bytes());
        };
        for fb in 0..blocks {
            let phys = self.bmap(&mut dir, fb, false)?;
            if phys == 0 {
                continue;
            }
            let mut data = self.read_block(phys as u64)?;
            let mut off = 0;
            while off + 8 <= self.block_size {
                let cur_ino = rd32(&data, off);
                let rec_len = rd16(&data, off + 4) as usize;
                if rec_len < 8 {
                    return Err(EIO);
                }
                let used = if cur_ino == 0 { 0 } else { (8 + data[off + 6] as usize).next_multiple_of(4) };
                if rec_len - used >= need {
                    if cur_ino == 0 {
                        write_entry(&mut data, off, rec_len);
                    } else {
                        wr16(&mut data, off + 4, used as u16);
                        write_entry(&mut data, off + used, rec_len - used);
                    }
                    self.write_block(phys as u64, &data)?;
                    return self.directory_changed(dir_ino, dir);
                }
                off += rec_len;
            }
        }
        // No room: add a block to the directory.
        let phys = self.bmap(&mut dir, blocks, true)?;
        let mut data = vec![0u8; self.block_size];
        write_entry(&mut data, 0, self.block_size);
        self.write_block(phys as u64, &data)?;
        dir.set_size((blocks + 1) * self.block_size as u64);
        self.directory_changed(dir_ino, dir)
    }

    /// After changing a directory: update its times, and drop its hash
    /// index flag (we don't maintain the index, so Linux must not use it).
    fn directory_changed(&mut self, dir_ino: Ino, mut dir: Inode) -> FsResult<()> {
        dir.touch();
        dir.set_flags(dir.flags() & !INDEX_FL);
        self.write_inode(dir_ino, &dir)
    }

    fn remove_entry(&mut self, dir_ino: Ino, name: &str) -> FsResult<Ino> {
        let mut dir = self.inode(dir_ino)?;
        let (_, ino, _, fb, off) =
            self.entries(&mut dir)?.into_iter().find(|e| e.0 == name).ok_or(ENOENT)?;
        let phys = self.bmap(&mut dir, fb, false)?;
        let mut data = self.read_block(phys as u64)?;
        if off == 0 {
            wr32(&mut data, 0, 0); // first in its block: mark unused
        } else {
            // Merge into the previous entry.
            let mut prev = 0;
            loop {
                let len = rd16(&data, prev + 4) as usize;
                if prev + len == off {
                    break;
                }
                prev += len;
            }
            let merged = rd16(&data, prev + 4) + rd16(&data, off + 4);
            wr16(&mut data, prev + 4, merged);
        }
        self.write_block(phys as u64, &data)?;
        self.directory_changed(dir_ino, dir)?;
        Ok(ino)
    }

    /// Free an inode with no links left (unless it's still open).
    fn release_if_unused(&mut self, ino: Ino) -> FsResult<()> {
        if self.open.get(&ino).copied().unwrap_or(0) > 0 {
            self.orphans.insert(ino);
            return Ok(());
        }
        self.orphans.remove(&ino);
        let mut inode = self.inode(ino)?;
        let is_dir = inode.is_dir();
        self.free_blocks_from(&mut inode, 0)?;
        inode.set_size(0);
        inode.set_links(0);
        wr32(&mut inode.raw, 20, crate::rtc::now() as u32); // dtime: deleted
        self.write_inode(ino, &inode)?;
        self.free_inode(ino, is_dir)
    }
}

impl FileSystem for Ext2 {
    fn name(&self) -> &'static str {
        "ext2"
    }

    fn root(&self) -> Ino {
        ROOT_INO
    }

    fn lookup(&mut self, dir: Ino, name: &str) -> FsResult<Ino> {
        let mut d = self.inode(dir)?;
        if !d.is_dir() {
            return Err(ENOTDIR);
        }
        self.entries(&mut d)?.into_iter().find(|e| e.0 == name).map(|e| e.1).ok_or(ENOENT)
    }

    fn meta(&mut self, ino: Ino) -> FsResult<Meta> {
        let inode = self.inode(ino)?;
        let kind = Self::kind_of(&inode);
        Ok(Meta {
            ino,
            kind,
            size: inode.size(),
            perm: (inode.mode() & 0o7777) as u32,
            nlink: inode.links() as u32,
            mtime: inode.mtime() as u64,
            uid: inode.uid(),
            gid: inode.gid(),
        })
    }

    fn read(&mut self, ino: Ino, offset: u64, buf: &mut [u8]) -> FsResult<usize> {
        let mut inode = self.inode(ino)?;
        if inode.is_dir() {
            return Err(EISDIR);
        }
        let size = inode.size();
        if offset >= size {
            return Ok(0);
        }
        let total = buf.len().min((size - offset) as usize);
        let bs = self.block_size as u64;
        let mut done = 0;
        while done < total {
            let pos = offset + done as u64;
            let in_block = (pos % bs) as usize;
            let n = (self.block_size - in_block).min(total - done);
            let phys = self.bmap(&mut inode, pos / bs, false)?;
            if phys == 0 {
                buf[done..done + n].fill(0); // a hole
            } else {
                let data = self.read_block(phys as u64)?;
                buf[done..done + n].copy_from_slice(&data[in_block..in_block + n]);
            }
            done += n;
        }
        Ok(total)
    }

    fn write(&mut self, ino: Ino, offset: u64, data: &[u8]) -> FsResult<usize> {
        if self.read_only {
            return Err(EROFS);
        }
        let mut inode = self.inode(ino)?;
        if inode.is_dir() {
            return Err(EISDIR);
        }
        let bs = self.block_size as u64;
        let mut done = 0;
        let mut result = Ok(());
        self.begin_batch();
        while done < data.len() {
            let pos = offset + done as u64;
            let in_block = (pos % bs) as usize;
            let n = (self.block_size - in_block).min(data.len() - done);
            let phys = match self.bmap(&mut inode, pos / bs, true) {
                Ok(p) => p,
                Err(e) => {
                    result = Err(e);
                    break;
                }
            };
            let mut block = if n == self.block_size { vec![0u8; self.block_size] } else { self.read_block(phys as u64)? };
            block[in_block..in_block + n].copy_from_slice(&data[done..done + n]);
            if let Err(e) = self.write_block(phys as u64, &block) {
                result = Err(e);
                break;
            }
            done += n;
        }
        // The bookkeeping, then the inode (as before: by the end of the call).
        if let Err(e) = self.end_batch() {
            result = Err(e);
        }
        if offset + done as u64 > inode.size() {
            inode.set_size(offset + done as u64);
        }
        inode.touch();
        self.write_inode(ino, &inode)?;
        match result {
            Err(e) if done == 0 => Err(e),
            _ => Ok(done), // a short write (e.g. the disk filled up)
        }
    }

    fn truncate(&mut self, ino: Ino, len: u64) -> FsResult<()> {
        if self.read_only {
            return Err(EROFS);
        }
        let mut inode = self.inode(ino)?;
        if inode.is_dir() {
            return Err(EISDIR);
        }
        if len < inode.size() {
            let bs = self.block_size as u64;
            self.free_blocks_from(&mut inode, len.div_ceil(bs))?;
            // Zero the rest of the last block, so growing the file again
            // later shows zeros rather than old data.
            if len % bs != 0 {
                let phys = self.bmap(&mut inode, len / bs, false)?;
                if phys != 0 {
                    let mut block = self.read_block(phys as u64)?;
                    block[(len % bs) as usize..].fill(0);
                    self.write_block(phys as u64, &block)?;
                }
            }
        }
        inode.set_size(len);
        inode.touch();
        self.write_inode(ino, &inode)
    }

    fn readdir(&mut self, dir: Ino) -> FsResult<Vec<(String, Ino, FileType)>> {
        let mut d = self.inode(dir)?;
        if !d.is_dir() {
            return Err(ENOTDIR);
        }
        let entries = self.entries(&mut d)?;
        let mut out = Vec::new();
        for (name, ino, ftype, _, _) in entries {
            if name == "." || name == ".." {
                continue;
            }
            let kind = match ftype {
                FT_DIR => FileType::Dir,
                FT_REG => FileType::File,
                FT_SYMLINK => FileType::Symlink,
                FT_SOCK => FileType::Socket,
                _ => self.meta(ino).map(|m| m.kind).unwrap_or(FileType::File),
            };
            out.push((name, ino, kind));
        }
        Ok(out)
    }

    fn create(&mut self, dir: Ino, name: &str, kind: FileType, perm: u32) -> FsResult<Ino> {
        if self.read_only {
            return Err(EROFS);
        }
        if name.len() > 255 {
            return Err(ENAMETOOLONG);
        }
        if !matches!(kind, FileType::File | FileType::Dir) {
            return Err(EINVAL);
        }
        let mut parent = self.inode(dir)?;
        if !parent.is_dir() {
            return Err(ENOTDIR);
        }
        if self.entries(&mut parent)?.iter().any(|e| e.0 == name) {
            return Err(EEXIST);
        }
        let is_dir = kind == FileType::Dir;
        let ino = self.alloc_inode(is_dir)?;
        match self.init_new_inode(dir, name, kind, ino, perm & 0o7777, None) {
            Ok(()) => Ok(ino),
            Err(e) => {
                // Out of space partway: give back what was allocated.
                if let Ok(mut inode) = self.inode(ino) {
                    let _ = self.free_blocks_from(&mut inode, 0);
                    inode.set_links(0);
                    wr32(&mut inode.raw, 20, crate::rtc::now() as u32);
                    let _ = self.write_inode(ino, &inode);
                }
                let _ = self.free_inode(ino, is_dir);
                Err(e)
            }
        }
    }

    fn remove(&mut self, dir: Ino, name: &str, is_dir: bool) -> FsResult<()> {
        self.remove_impl(dir, name, is_dir)
    }

    fn symlink(&mut self, dir: Ino, name: &str, target: &str) -> FsResult<Ino> {
        if self.read_only {
            return Err(EROFS);
        }
        if target.len() >= self.block_size {
            return Err(ENAMETOOLONG);
        }
        let mut parent = self.inode(dir)?;
        if self.entries(&mut parent)?.iter().any(|e| e.0 == name) {
            return Err(EEXIST);
        }
        let ino = self.alloc_inode(false)?;
        match self.init_new_inode(dir, name, FileType::Symlink, ino, 0o777, Some(target)) {
            Ok(()) => Ok(ino),
            Err(e) => {
                if let Ok(mut inode) = self.inode(ino) {
                    let _ = self.free_blocks_from(&mut inode, 0);
                    inode.set_links(0);
                    let _ = self.write_inode(ino, &inode);
                }
                let _ = self.free_inode(ino, false);
                Err(e)
            }
        }
    }

    fn readlink(&mut self, ino: Ino) -> FsResult<String> {
        let mut inode = self.inode(ino)?;
        if !inode.is_symlink() {
            return Err(EINVAL);
        }
        let size = inode.size() as usize;
        if inode.is_fast_symlink() {
            return Ok(String::from_utf8_lossy(&inode.raw[40..40 + size.min(FAST_SYMLINK_MAX)]).into_owned());
        }
        let phys = self.bmap(&mut inode, 0, false)?;
        let data = self.read_block(phys as u64)?;
        Ok(String::from_utf8_lossy(&data[..size.min(self.block_size)]).into_owned())
    }

    fn link(&mut self, dir: Ino, name: &str, target: Ino) -> FsResult<()> {
        if self.read_only {
            return Err(EROFS);
        }
        let mut inode = self.inode(target)?;
        if inode.is_dir() {
            return Err(EPERM);
        }
        if inode.links() == u16::MAX {
            return Err(31); // EMLINK
        }
        let mut parent = self.inode(dir)?;
        if self.entries(&mut parent)?.iter().any(|e| e.0 == name) {
            return Err(EEXIST);
        }
        let ft = self.file_type_byte(Self::kind_of(&inode));
        self.add_entry(dir, name, target, ft)?;
        let mut inode = self.inode(target)?;
        inode.set_links(inode.links() + 1);
        wr32(&mut inode.raw, 12, crate::rtc::now() as u32); // ctime
        self.write_inode(target, &inode)
    }

    fn rename(&mut self, old_dir: Ino, old_name: &str, new_dir: Ino, new_name: &str) -> FsResult<()> {
        if self.read_only {
            return Err(EROFS);
        }
        if new_name.len() > 255 {
            return Err(ENAMETOOLONG);
        }
        let src = self.lookup(old_dir, old_name)?;
        let src_inode = self.inode(src)?;
        let is_dir = src_inode.is_dir();
        if let Ok(dst) = self.lookup(new_dir, new_name) {
            if dst == src {
                return Ok(());
            }
            let dst_is_dir = self.inode(dst)?.is_dir();
            self.remove_impl(new_dir, new_name, dst_is_dir)?; // ENOTEMPTY if a non-empty dir
        }
        let ft = self.file_type_byte(Self::kind_of(&src_inode));
        self.add_entry(new_dir, new_name, src, ft)?;
        self.remove_entry(old_dir, old_name)?;
        if is_dir && old_dir != new_dir {
            self.set_dotdot(src, new_dir)?;
            let mut old = self.inode(old_dir)?;
            old.set_links(old.links().saturating_sub(1));
            self.write_inode(old_dir, &old)?;
            let mut new = self.inode(new_dir)?;
            new.set_links(new.links() + 1);
            self.write_inode(new_dir, &new)?;
        }
        let mut inode = self.inode(src)?;
        wr32(&mut inode.raw, 12, crate::rtc::now() as u32); // ctime
        self.write_inode(src, &inode)
    }

    fn chmod(&mut self, ino: Ino, perm: u32) -> FsResult<()> {
        if self.read_only {
            return Err(EROFS);
        }
        let mut inode = self.inode(ino)?;
        let mode = (inode.mode() & S_IFMT) | (perm & 0o7777) as u16;
        wr16(&mut inode.raw, 0, mode);
        wr32(&mut inode.raw, 12, crate::rtc::now() as u32);
        self.write_inode(ino, &inode)
    }

    fn chown(&mut self, ino: Ino, uid: u32, gid: u32) -> FsResult<()> {
        if self.read_only {
            return Err(EROFS);
        }
        let mut inode = self.inode(ino)?;
        inode.set_owner(uid, gid);
        wr32(&mut inode.raw, 12, crate::rtc::now() as u32); // ctime
        self.write_inode(ino, &inode)
    }

    fn set_mtime(&mut self, ino: Ino, mtime: u64) -> FsResult<()> {
        if self.read_only {
            return Err(EROFS);
        }
        let mut inode = self.inode(ino)?;
        wr32(&mut inode.raw, 8, mtime as u32); // atime
        wr32(&mut inode.raw, 16, mtime as u32); // mtime
        self.write_inode(ino, &inode)
    }

    fn statfs(&mut self) -> FsStats {
        FsStats {
            block_size: self.block_size as u64,
            blocks: self.blocks_count as u64,
            free_blocks: rd32(&self.sb, 12) as u64,
            files: rd32(&self.sb, 0) as u64,
            free_files: rd32(&self.sb, 16) as u64,
        }
    }

    fn open_inode(&mut self, ino: Ino) {
        *self.open.entry(ino).or_insert(0) += 1;
    }

    fn close_inode(&mut self, ino: Ino) {
        let count = self.open.entry(ino).or_insert(1);
        *count -= 1;
        if *count == 0 {
            self.open.remove(&ino);
            if self.orphans.contains(&ino) {
                let _ = self.release_if_unused(ino);
            }
        }
    }

    fn sync(&mut self) -> FsResult<()> {
        self.dev.flush().map_err(|_| EIO)
    }
}

impl Ext2 {
    /// Fill in a freshly allocated inode and link it into `dir`.
    fn init_new_inode(&mut self, dir: Ino, name: &str, kind: FileType, ino: Ino, perm: u32, target: Option<&str>) -> FsResult<()> {
        let is_dir = kind == FileType::Dir;
        let mut inode = Inode { raw: vec![0u8; self.inode_size] };
        if self.inode_size > 128 {
            // Use the same "extra size" as the inodes mkfs made.
            let root = self.inode(ROOT_INO)?;
            inode.raw[128..130].copy_from_slice(&root.raw[128..130]);
        }
        let now = crate::rtc::now() as u32;
        wr32(&mut inode.raw, 8, now); // atime
        inode.touch();
        let perm = perm as u16;
        match kind {
            FileType::Dir => {
                wr16(&mut inode.raw, 0, S_IFDIR | perm);
                inode.set_links(2);
            }
            FileType::Symlink => {
                wr16(&mut inode.raw, 0, S_IFLNK | 0o777);
                inode.set_links(1);
                let t = target.unwrap_or("");
                inode.set_size(t.len() as u64);
                if t.len() < FAST_SYMLINK_MAX {
                    inode.raw[40..40 + t.len()].copy_from_slice(t.as_bytes());
                }
            }
            FileType::Socket => {
                wr16(&mut inode.raw, 0, S_IFSOCK | perm);
                inode.set_links(1);
            }
            _ => {
                wr16(&mut inode.raw, 0, S_IFREG | perm);
                inode.set_links(1);
            }
        }
        // Write the inode before anything can fail, so the cleanup in
        // create() sees (and frees) its blocks.
        self.write_inode(ino, &inode)?;
        if let Some(t) = target.filter(|t| t.len() >= FAST_SYMLINK_MAX) {
            // A long symlink target goes in a data block.
            let block = self.alloc_block()?;
            inode.set_block(0, block);
            inode.add_sectors((self.block_size / SECTOR_SIZE) as i64);
            self.write_inode(ino, &inode)?;
            let mut data = vec![0u8; self.block_size];
            data[..t.len()].copy_from_slice(t.as_bytes());
            self.write_block(block as u64, &data)?;
        }
        if is_dir {
            // The directory's first block: "." and "..".
            let block = self.alloc_block()?;
            inode.set_block(0, block);
            inode.add_sectors((self.block_size / SECTOR_SIZE) as i64);
            inode.set_size(self.block_size as u64);
            self.write_inode(ino, &inode)?;
            let mut data = vec![0u8; self.block_size];
            let ft = self.file_type_byte(FileType::Dir);
            wr32(&mut data, 0, ino as u32);
            wr16(&mut data, 4, 12);
            data[6] = 1;
            data[7] = ft;
            data[8] = b'.';
            wr32(&mut data, 12, dir as u32);
            wr16(&mut data, 16, (self.block_size - 12) as u16);
            data[18] = 2;
            data[19] = ft;
            data[20..22].copy_from_slice(b"..");
            self.write_block(block as u64, &data)?;
        }
        self.add_entry(dir, name, ino, self.file_type_byte(kind))?;
        if is_dir {
            let mut parent = self.inode(dir)?;
            parent.set_links(parent.links() + 1); // the new ".."
            self.write_inode(dir, &parent)?;
        }
        Ok(())
    }

    fn remove_impl(&mut self, dir: Ino, name: &str, is_dir: bool) -> FsResult<()> {
        if self.read_only {
            return Err(EROFS);
        }
        let ino = self.lookup(dir, name)?;
        let mut inode = self.inode(ino)?;
        if is_dir {
            if self.entries(&mut inode)?.iter().any(|e| e.0 != "." && e.0 != "..") {
                return Err(ENOTEMPTY);
            }
        }
        self.remove_entry(dir, name)?;
        if is_dir {
            let mut parent = self.inode(dir)?;
            parent.set_links(parent.links().saturating_sub(1)); // its ".." is gone
            self.write_inode(dir, &parent)?;
            inode.set_links(0);
        } else {
            inode.set_links(inode.links().saturating_sub(1));
        }
        inode.touch();
        self.write_inode(ino, &inode)?;
        if inode.links() == 0 {
            self.release_if_unused(ino)?;
        }
        Ok(())
    }
}

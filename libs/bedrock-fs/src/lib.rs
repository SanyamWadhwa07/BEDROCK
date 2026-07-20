//! SimpleFS: an xv6/early-ext2-style on-disk filesystem.
//! `[superblock][inode bitmap][data bitmap][inode table][data blocks]`,
//! 4KiB blocks. Inodes hold direct block pointers plus one single-indirect
//! block (a shallow, B-tree-like fanout), giving a max file size of
//! `(10 + 1024) * 4KiB`. The root directory is a single fixed data block of
//! `(name, inode)` entries: flat, no subdirectories, a deliberate scope cut.
//!
//! Everything here is generic over [`BlockDevice`], so the layout logic has
//! no idea whether blocks live on an ATA disk or in a `Vec` on the host.
//! That is the whole point: the inode indexing, the two free-space bitmaps,
//! and the free/reallocate paths are where a filesystem leaks or corrupts
//! itself, and none of that needs real hardware to test. The kernel supplies
//! a device backed by the ATA driver and its block cache; the tests at the
//! bottom of this file supply one backed by memory, including a crash-and-
//! remount check that would otherwise require rebooting QEMU.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

pub const BLOCK_SIZE: usize = 4096;

/// A block-addressed device. Errors are `&'static str` to stay allocation
/// free on the failure path, matching the kernel's ATA driver.
pub trait BlockDevice {
    fn read_block(&mut self, block: u32, buf: &mut [u8; BLOCK_SIZE]) -> Result<(), &'static str>;
    fn write_block(&mut self, block: u32, buf: &[u8; BLOCK_SIZE]) -> Result<(), &'static str>;
}

/// A heap-allocated, zeroed block buffer. Every block-sized scratch buffer
/// goes through this instead of a `[u8; BLOCK_SIZE]` stack local: deeply
/// nested calls here (`write_file` freeing an indirect block while
/// allocating fresh ones) can stack up several 4KiB locals at once, which
/// blew a task's 32KiB kernel stack during Phase 7 bring-up. Heap
/// allocation makes stack depth independent of how many blocks a call chain
/// happens to touch.
fn zero_block() -> Box<[u8; BLOCK_SIZE]> {
    Box::new([0u8; BLOCK_SIZE])
}

const MAGIC: u32 = 0xBED2_0C5F;
const SUPERBLOCK_BLOCK: u32 = 0;
const INODE_BITMAP_BLOCK: u32 = 1;
const DATA_BITMAP_BLOCK: u32 = 2;
const INODE_TABLE_BLOCK: u32 = 3;
const DATA_START_BLOCK: u32 = 4;

/// Blocks the filesystem owns by default. `xtask` sizes `fs.img` to this
/// plus a scratch region for `bench ata`, which starts writing at exactly
/// this block, so keep the two in step.
pub const DEFAULT_TOTAL_BLOCKS: u32 = 1024; // 4 MiB

pub const MAX_INODES: u32 = 64;
pub const DIRECT_PTRS: usize = 10;
pub const PTRS_PER_BLOCK: usize = BLOCK_SIZE / 4;
/// used+kind+pad, size, direct[10], indirect = 52 bytes.
const INODE_SIZE: usize = 8 + DIRECT_PTRS * 4 + 4;

pub const ROOT_INODE: u32 = 0;
pub const NAME_LEN: usize = 27;
/// used, name, inode = 32 bytes.
const DIR_ENTRY_SIZE: usize = 1 + NAME_LEN + 4;
const DIR_SLOTS: usize = BLOCK_SIZE / DIR_ENTRY_SIZE;

const KIND_FILE: u8 = 1;
const KIND_DIR: u8 = 2;

/// The largest file this layout can address.
pub const MAX_FILE_SIZE: usize = (DIRECT_PTRS + PTRS_PER_BLOCK) * BLOCK_SIZE;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Superblock {
    pub total_blocks: u32,
    pub total_data_blocks: u32,
}

/// One directory listing entry: `(name, inode id, size in bytes)`.
pub type DirEntry = (String, u32, u32);

#[derive(Clone, Copy)]
struct DiskInode {
    used: bool,
    kind: u8,
    size: u32,
    direct: [u32; DIRECT_PTRS],
    indirect: u32,
}

impl DiskInode {
    fn empty() -> Self {
        Self {
            used: false,
            kind: 0,
            size: 0,
            direct: [0; DIRECT_PTRS],
            indirect: 0,
        }
    }

    fn read_from(buf: &[u8]) -> Self {
        let mut direct = [0u32; DIRECT_PTRS];
        for (i, slot) in direct.iter_mut().enumerate() {
            let off = 8 + i * 4;
            *slot = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap());
        }
        let ioff = 8 + DIRECT_PTRS * 4;
        Self {
            used: buf[0] != 0,
            kind: buf[1],
            size: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
            direct,
            indirect: u32::from_le_bytes(buf[ioff..ioff + 4].try_into().unwrap()),
        }
    }

    fn write_to(&self, buf: &mut [u8]) {
        buf[0] = self.used as u8;
        buf[1] = self.kind;
        buf[2] = 0;
        buf[3] = 0;
        buf[4..8].copy_from_slice(&self.size.to_le_bytes());
        for (i, ptr) in self.direct.iter().enumerate() {
            let off = 8 + i * 4;
            buf[off..off + 4].copy_from_slice(&ptr.to_le_bytes());
        }
        let ioff = 8 + DIRECT_PTRS * 4;
        buf[ioff..ioff + 4].copy_from_slice(&self.indirect.to_le_bytes());
    }
}

fn bit_get(buf: &[u8], idx: u32) -> bool {
    (buf[(idx / 8) as usize] >> (idx % 8)) & 1 != 0
}

fn bit_set(buf: &mut [u8], idx: u32, val: bool) {
    let byte = &mut buf[(idx / 8) as usize];
    if val {
        *byte |= 1 << (idx % 8);
    } else {
        *byte &= !(1 << (idx % 8));
    }
}

fn dir_entry_at(block: &[u8; BLOCK_SIZE], slot: usize) -> (bool, String, u32) {
    let off = slot * DIR_ENTRY_SIZE;
    let used = block[off] != 0;
    let name_bytes = &block[off + 1..off + 1 + NAME_LEN];
    let name_len = name_bytes.iter().position(|&b| b == 0).unwrap_or(NAME_LEN);
    let name = String::from_utf8_lossy(&name_bytes[..name_len]).into_owned();
    let inode = u32::from_le_bytes(
        block[off + 1 + NAME_LEN..off + 1 + NAME_LEN + 4]
            .try_into()
            .unwrap(),
    );
    (used, name, inode)
}

fn write_dir_entry(block: &mut [u8; BLOCK_SIZE], slot: usize, used: bool, name: &str, inode: u32) {
    let off = slot * DIR_ENTRY_SIZE;
    block[off] = used as u8;
    let name_area = &mut block[off + 1..off + 1 + NAME_LEN];
    name_area.fill(0);
    let bytes = name.as_bytes();
    let n = bytes.len().min(NAME_LEN);
    name_area[..n].copy_from_slice(&bytes[..n]);
    block[off + 1 + NAME_LEN..off + 1 + NAME_LEN + 4].copy_from_slice(&inode.to_le_bytes());
}

pub struct SimpleFs<D: BlockDevice> {
    dev: D,
    sb: Superblock,
}

impl<D: BlockDevice> SimpleFs<D> {
    /// Mounts an existing filesystem, formatting fresh if the on-disk magic
    /// doesn't match (first boot, or a disk image never formatted).
    pub fn mount_or_format(mut dev: D) -> Result<Self, &'static str> {
        let mut sb_block = zero_block();
        let existing = match dev.read_block(SUPERBLOCK_BLOCK, &mut sb_block) {
            Ok(()) if u32::from_le_bytes(sb_block[0..4].try_into().unwrap()) == MAGIC => {
                Some(Superblock {
                    total_blocks: u32::from_le_bytes(sb_block[4..8].try_into().unwrap()),
                    total_data_blocks: u32::from_le_bytes(sb_block[8..12].try_into().unwrap()),
                })
            }
            _ => None,
        };

        match existing {
            Some(sb) => Ok(Self { dev, sb }),
            None => {
                let sb = Superblock {
                    total_blocks: DEFAULT_TOTAL_BLOCKS,
                    total_data_blocks: DEFAULT_TOTAL_BLOCKS - DATA_START_BLOCK,
                };
                let mut fs = Self { dev, sb };
                fs.format()?;
                Ok(fs)
            }
        }
    }

    pub fn superblock(&self) -> Superblock {
        self.sb
    }

    pub fn device_mut(&mut self) -> &mut D {
        &mut self.dev
    }

    fn format(&mut self) -> Result<(), &'static str> {
        self.dev.write_block(INODE_BITMAP_BLOCK, &zero_block())?;
        self.dev.write_block(DATA_BITMAP_BLOCK, &zero_block())?;
        self.dev.write_block(INODE_TABLE_BLOCK, &zero_block())?;

        // Root directory: inode 0, one data block of zeroed (empty) entries.
        let root_id = self.alloc_inode()?;
        debug_assert_eq!(root_id, ROOT_INODE);
        let root_block = self.alloc_data_block()?;
        self.dev.write_block(root_block, &zero_block())?;
        let mut root_inode = DiskInode::empty();
        root_inode.used = true;
        root_inode.kind = KIND_DIR;
        root_inode.direct[0] = root_block;
        self.write_inode(ROOT_INODE, &root_inode)?;

        // Superblock written last: a crash mid-format leaves a bad magic
        // (re-formatted next mount) rather than a superblock claiming a
        // filesystem that was never finished being laid out.
        let mut sb_block = zero_block();
        sb_block[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        sb_block[4..8].copy_from_slice(&self.sb.total_blocks.to_le_bytes());
        sb_block[8..12].copy_from_slice(&self.sb.total_data_blocks.to_le_bytes());
        self.dev.write_block(SUPERBLOCK_BLOCK, &sb_block)
    }

    fn alloc_inode(&mut self) -> Result<u32, &'static str> {
        let mut bmp = zero_block();
        self.dev.read_block(INODE_BITMAP_BLOCK, &mut bmp)?;
        for i in 0..MAX_INODES {
            if !bit_get(&bmp[..], i) {
                bit_set(&mut bmp[..], i, true);
                self.dev.write_block(INODE_BITMAP_BLOCK, &bmp)?;
                return Ok(i);
            }
        }
        Err("out of inodes")
    }

    fn free_inode(&mut self, id: u32) -> Result<(), &'static str> {
        let mut bmp = zero_block();
        self.dev.read_block(INODE_BITMAP_BLOCK, &mut bmp)?;
        bit_set(&mut bmp[..], id, false);
        self.dev.write_block(INODE_BITMAP_BLOCK, &bmp)
    }

    fn alloc_data_block(&mut self) -> Result<u32, &'static str> {
        let mut bmp = zero_block();
        self.dev.read_block(DATA_BITMAP_BLOCK, &mut bmp)?;
        for i in 0..self.sb.total_data_blocks {
            if !bit_get(&bmp[..], i) {
                bit_set(&mut bmp[..], i, true);
                self.dev.write_block(DATA_BITMAP_BLOCK, &bmp)?;
                return Ok(DATA_START_BLOCK + i);
            }
        }
        Err("disk full")
    }

    fn free_data_block(&mut self, abs_block: u32) -> Result<(), &'static str> {
        if abs_block < DATA_START_BLOCK {
            return Ok(());
        }
        let mut bmp = zero_block();
        self.dev.read_block(DATA_BITMAP_BLOCK, &mut bmp)?;
        bit_set(&mut bmp[..], abs_block - DATA_START_BLOCK, false);
        self.dev.write_block(DATA_BITMAP_BLOCK, &bmp)
    }

    /// Data blocks currently marked allocated. Exposed because "did that
    /// operation leak blocks?" is the question worth asserting on.
    pub fn used_data_blocks(&mut self) -> Result<u32, &'static str> {
        let mut bmp = zero_block();
        self.dev.read_block(DATA_BITMAP_BLOCK, &mut bmp)?;
        Ok((0..self.sb.total_data_blocks)
            .filter(|&i| bit_get(&bmp[..], i))
            .count() as u32)
    }

    /// Inodes currently marked allocated, root included.
    pub fn used_inodes(&mut self) -> Result<u32, &'static str> {
        let mut bmp = zero_block();
        self.dev.read_block(INODE_BITMAP_BLOCK, &mut bmp)?;
        Ok((0..MAX_INODES).filter(|&i| bit_get(&bmp[..], i)).count() as u32)
    }

    fn read_inode(&mut self, id: u32) -> Result<DiskInode, &'static str> {
        if id >= MAX_INODES {
            return Err("inode id out of range");
        }
        let mut block = zero_block();
        self.dev.read_block(INODE_TABLE_BLOCK, &mut block)?;
        let off = id as usize * INODE_SIZE;
        Ok(DiskInode::read_from(&block[off..off + INODE_SIZE]))
    }

    fn write_inode(&mut self, id: u32, inode: &DiskInode) -> Result<(), &'static str> {
        if id >= MAX_INODES {
            return Err("inode id out of range");
        }
        let mut block = zero_block();
        self.dev.read_block(INODE_TABLE_BLOCK, &mut block)?;
        let off = id as usize * INODE_SIZE;
        inode.write_to(&mut block[off..off + INODE_SIZE]);
        self.dev.write_block(INODE_TABLE_BLOCK, &block)
    }

    fn read_root_dir(&mut self) -> Result<(u32, Box<[u8; BLOCK_SIZE]>), &'static str> {
        let root = self.read_inode(ROOT_INODE)?;
        let block = root.direct[0];
        let mut dir_block = zero_block();
        self.dev.read_block(block, &mut dir_block)?;
        Ok((block, dir_block))
    }

    /// Lists the root directory: `(name, inode id, size in bytes)`.
    pub fn list_root(&mut self) -> Result<Vec<DirEntry>, &'static str> {
        let (_, dir_block) = self.read_root_dir()?;
        let mut out = Vec::new();
        for slot in 0..DIR_SLOTS {
            let (used, name, inode) = dir_entry_at(&dir_block, slot);
            if used {
                let size = self.read_inode(inode).map(|i| i.size).unwrap_or(0);
                out.push((name, inode, size));
            }
        }
        Ok(out)
    }

    /// Looks a name up in the root directory.
    pub fn lookup(&mut self, name: &str) -> Result<Option<u32>, &'static str> {
        let (_, dir_block) = self.read_root_dir()?;
        for slot in 0..DIR_SLOTS {
            let (used, existing, inode) = dir_entry_at(&dir_block, slot);
            if used && existing == name {
                return Ok(Some(inode));
            }
        }
        Ok(None)
    }

    /// Creates an empty file in the root directory, returning its inode id.
    pub fn create_file(&mut self, name: &str) -> Result<u32, &'static str> {
        if name.is_empty() {
            return Err("name is empty");
        }
        if name.len() > NAME_LEN {
            return Err("name is too long");
        }
        let (dir_block_num, mut dir_block) = self.read_root_dir()?;

        let mut free_slot = None;
        for slot in 0..DIR_SLOTS {
            let (used, existing_name, _) = dir_entry_at(&dir_block, slot);
            if used && existing_name == name {
                return Err("a file with that name already exists");
            }
            if !used && free_slot.is_none() {
                free_slot = Some(slot);
            }
        }
        let slot = free_slot.ok_or("root directory is full")?;

        let id = self.alloc_inode()?;
        let mut inode = DiskInode::empty();
        inode.used = true;
        inode.kind = KIND_FILE;
        self.write_inode(id, &inode)?;

        write_dir_entry(&mut dir_block, slot, true, name, id);
        self.dev.write_block(dir_block_num, &dir_block)?;
        Ok(id)
    }

    /// Reads a file's full contents.
    pub fn read_file(&mut self, inode_id: u32) -> Result<Vec<u8>, &'static str> {
        let inode = self.read_inode(inode_id)?;
        let mut out = Vec::with_capacity(inode.size as usize);
        let mut remaining = inode.size as usize;

        for &block in inode.direct.iter() {
            if remaining == 0 || block == 0 {
                break;
            }
            let mut buf = zero_block();
            self.dev.read_block(block, &mut buf)?;
            let take = remaining.min(BLOCK_SIZE);
            out.extend_from_slice(&buf[..take]);
            remaining -= take;
        }

        if remaining > 0 && inode.indirect != 0 {
            let mut ptrs_block = zero_block();
            self.dev.read_block(inode.indirect, &mut ptrs_block)?;
            for i in 0..PTRS_PER_BLOCK {
                if remaining == 0 {
                    break;
                }
                let ptr = u32::from_le_bytes(ptrs_block[i * 4..i * 4 + 4].try_into().unwrap());
                if ptr == 0 {
                    break;
                }
                let mut buf = zero_block();
                self.dev.read_block(ptr, &mut buf)?;
                let take = remaining.min(BLOCK_SIZE);
                out.extend_from_slice(&buf[..take]);
                remaining -= take;
            }
        }

        Ok(out)
    }

    /// Overwrites a file's contents: frees every block it used to own, then
    /// allocates fresh ones for `data` (direct pointers first, spilling into
    /// the single-indirect block once all 10 direct slots are full).
    pub fn write_file(&mut self, inode_id: u32, data: &[u8]) -> Result<(), &'static str> {
        let mut inode = self.read_inode(inode_id)?;

        let blocks_needed = data.len().div_ceil(BLOCK_SIZE);
        if blocks_needed > DIRECT_PTRS + PTRS_PER_BLOCK {
            return Err("file too large for this filesystem");
        }

        // Only release the old blocks once the size is known to be legal,
        // so a rejected write leaves the file exactly as it was.
        self.free_file_blocks(&mut inode)?;

        let mut indirect_ptrs = zero_block();
        let mut using_indirect = false;

        for i in 0..blocks_needed {
            let chunk_start = i * BLOCK_SIZE;
            let chunk_end = (chunk_start + BLOCK_SIZE).min(data.len());
            let mut buf = zero_block();
            buf[..chunk_end - chunk_start].copy_from_slice(&data[chunk_start..chunk_end]);

            let block = self.alloc_data_block()?;
            self.dev.write_block(block, &buf)?;

            if i < DIRECT_PTRS {
                inode.direct[i] = block;
            } else {
                if inode.indirect == 0 {
                    inode.indirect = self.alloc_data_block()?;
                }
                using_indirect = true;
                let idx = i - DIRECT_PTRS;
                indirect_ptrs[idx * 4..idx * 4 + 4].copy_from_slice(&block.to_le_bytes());
            }
        }
        if using_indirect {
            self.dev.write_block(inode.indirect, &indirect_ptrs)?;
        }

        inode.size = data.len() as u32;
        self.write_inode(inode_id, &inode)
    }

    /// Frees every data block an inode owns (direct, the indirect targets,
    /// and the indirect block itself) and zeroes its pointers. Shared
    /// between overwrite and delete.
    fn free_file_blocks(&mut self, inode: &mut DiskInode) -> Result<(), &'static str> {
        for i in 0..DIRECT_PTRS {
            let ptr = inode.direct[i];
            if ptr != 0 {
                self.free_data_block(ptr)?;
            }
        }
        if inode.indirect != 0 {
            let mut ptrs_block = zero_block();
            self.dev.read_block(inode.indirect, &mut ptrs_block)?;
            for i in 0..PTRS_PER_BLOCK {
                let ptr = u32::from_le_bytes(ptrs_block[i * 4..i * 4 + 4].try_into().unwrap());
                if ptr != 0 {
                    self.free_data_block(ptr)?;
                }
            }
            let indirect = inode.indirect;
            self.free_data_block(indirect)?;
        }
        inode.direct = [0; DIRECT_PTRS];
        inode.indirect = 0;
        Ok(())
    }

    /// Deletes a file by name: frees its data blocks, releases its inode,
    /// and clears the directory entry. Refuses to touch directories.
    pub fn delete_file(&mut self, name: &str) -> Result<(), &'static str> {
        let (dir_block_num, mut dir_block) = self.read_root_dir()?;

        let mut found = None;
        for slot in 0..DIR_SLOTS {
            let (used, existing_name, inode) = dir_entry_at(&dir_block, slot);
            if used && existing_name == name {
                found = Some((slot, inode));
                break;
            }
        }
        let (slot, inode_id) = found.ok_or("no such file")?;

        let mut inode = self.read_inode(inode_id)?;
        if inode.kind == KIND_DIR {
            return Err("refusing to delete a directory");
        }
        self.free_file_blocks(&mut inode)?;
        self.write_inode(inode_id, &DiskInode::empty())?;
        self.free_inode(inode_id)?;

        write_dir_entry(&mut dir_block, slot, false, "", 0);
        self.dev.write_block(dir_block_num, &dir_block)
    }

    /// Renames a file in the root directory (same inode, new entry name).
    /// Fails if the target name is already taken.
    pub fn rename_file(&mut self, old: &str, new: &str) -> Result<(), &'static str> {
        if new.is_empty() {
            return Err("new name is empty");
        }
        if new.len() > NAME_LEN {
            return Err("new name is too long");
        }
        let (dir_block_num, mut dir_block) = self.read_root_dir()?;

        let mut found = None;
        for slot in 0..DIR_SLOTS {
            let (used, existing_name, inode) = dir_entry_at(&dir_block, slot);
            if used && existing_name == new {
                return Err("a file with the new name already exists");
            }
            if used && existing_name == old {
                found = Some((slot, inode));
            }
        }
        let (slot, inode_id) = found.ok_or("no such file")?;

        write_dir_entry(&mut dir_block, slot, true, new, inode_id);
        self.dev.write_block(dir_block_num, &dir_block)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An in-memory disk. Blocks are allocated lazily so a 4 MiB filesystem
    /// doesn't cost 4 MiB per test.
    struct FakeDisk {
        blocks: alloc::collections::BTreeMap<u32, Box<[u8; BLOCK_SIZE]>>,
        total_blocks: u32,
        /// Set to fail every write past this many, to model a disk dying
        /// mid-operation.
        writes_before_failure: Option<usize>,
        writes: usize,
    }

    impl FakeDisk {
        fn new() -> Self {
            Self {
                blocks: alloc::collections::BTreeMap::new(),
                total_blocks: DEFAULT_TOTAL_BLOCKS,
                writes_before_failure: None,
                writes: 0,
            }
        }
    }

    impl BlockDevice for FakeDisk {
        fn read_block(
            &mut self,
            block: u32,
            buf: &mut [u8; BLOCK_SIZE],
        ) -> Result<(), &'static str> {
            if block >= self.total_blocks {
                return Err("read past end of disk");
            }
            match self.blocks.get(&block) {
                Some(b) => buf.copy_from_slice(&b[..]),
                None => buf.fill(0),
            }
            Ok(())
        }

        fn write_block(&mut self, block: u32, buf: &[u8; BLOCK_SIZE]) -> Result<(), &'static str> {
            if block >= self.total_blocks {
                return Err("write past end of disk");
            }
            if let Some(limit) = self.writes_before_failure {
                if self.writes >= limit {
                    return Err("simulated disk failure");
                }
            }
            self.writes += 1;
            self.blocks.insert(block, Box::new(*buf));
            Ok(())
        }
    }

    fn fresh() -> SimpleFs<FakeDisk> {
        SimpleFs::mount_or_format(FakeDisk::new()).expect("format")
    }

    /// Deterministic filler so tests assert on content, not just length.
    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 253) as u8).collect()
    }

    #[test]
    fn formats_a_fresh_disk_with_only_the_root() {
        let mut fs = fresh();
        assert_eq!(fs.superblock().total_blocks, DEFAULT_TOTAL_BLOCKS);
        assert_eq!(fs.list_root().unwrap(), Vec::new());
        assert_eq!(fs.used_inodes().unwrap(), 1, "only the root inode");
        assert_eq!(fs.used_data_blocks().unwrap(), 1, "only the root dir block");
    }

    #[test]
    fn create_read_write_roundtrip() {
        let mut fs = fresh();
        let id = fs.create_file("hello.txt").unwrap();
        assert_eq!(fs.read_file(id).unwrap(), Vec::<u8>::new());

        fs.write_file(id, b"hello, simplefs").unwrap();
        assert_eq!(fs.read_file(id).unwrap(), b"hello, simplefs");

        let listing = fs.list_root().unwrap();
        assert_eq!(listing.len(), 1);
        assert_eq!(listing[0].0, "hello.txt");
        assert_eq!(listing[0].2, 15, "size must match what was written");
    }

    #[test]
    fn rejects_duplicate_and_malformed_names() {
        let mut fs = fresh();
        fs.create_file("a.txt").unwrap();
        assert!(fs.create_file("a.txt").is_err(), "duplicate name");
        assert!(fs.create_file("").is_err(), "empty name");
        let long = "x".repeat(NAME_LEN + 1);
        assert!(fs.create_file(&long).is_err(), "over-long name");
        // A name of exactly NAME_LEN must still be accepted, and round-trip.
        let exact = "y".repeat(NAME_LEN);
        let id = fs.create_file(&exact).unwrap();
        assert_eq!(fs.lookup(&exact).unwrap(), Some(id));
    }

    #[test]
    fn spans_multiple_direct_blocks() {
        let mut fs = fresh();
        let id = fs.create_file("big.bin").unwrap();
        // 5 blocks: comfortably inside the 10 direct pointers.
        let data = pattern(5 * BLOCK_SIZE);
        fs.write_file(id, &data).unwrap();
        assert_eq!(fs.read_file(id).unwrap(), data);
    }

    #[test]
    fn spills_into_the_single_indirect_block() {
        let mut fs = fresh();
        let id = fs.create_file("huge.bin").unwrap();
        // 12 blocks: past the 10 direct pointers, so the indirect block is
        // exercised, including its own allocation.
        let data = pattern(12 * BLOCK_SIZE);
        fs.write_file(id, &data).unwrap();
        assert_eq!(fs.read_file(id).unwrap(), data, "indirect readback differs");
    }

    #[test]
    fn handles_a_size_that_is_not_a_block_multiple() {
        let mut fs = fresh();
        let id = fs.create_file("odd.bin").unwrap();
        let data = pattern(BLOCK_SIZE * 3 + 17);
        fs.write_file(id, &data).unwrap();
        let back = fs.read_file(id).unwrap();
        assert_eq!(back.len(), data.len(), "trailing partial block lost");
        assert_eq!(back, data);
    }

    #[test]
    fn rejects_a_file_larger_than_the_layout_allows() {
        let mut fs = fresh();
        let id = fs.create_file("toobig.bin").unwrap();
        fs.write_file(id, &pattern(BLOCK_SIZE)).unwrap();
        let before = fs.used_data_blocks().unwrap();

        let too_big = vec![0u8; MAX_FILE_SIZE + 1];
        assert!(fs.write_file(id, &too_big).is_err());

        // The rejected write must not have disturbed the existing contents.
        assert_eq!(
            fs.used_data_blocks().unwrap(),
            before,
            "leaked on rejection"
        );
        assert_eq!(fs.read_file(id).unwrap(), pattern(BLOCK_SIZE));
    }

    /// Growing then shrinking a file must release the blocks it no longer
    /// needs. This is the classic place a filesystem leaks: the old block
    /// list is overwritten without being freed first.
    #[test]
    fn shrinking_after_growing_frees_the_extra_blocks() {
        let mut fs = fresh();
        let id = fs.create_file("shrink.bin").unwrap();
        let baseline = fs.used_data_blocks().unwrap();

        fs.write_file(id, &pattern(15 * BLOCK_SIZE)).unwrap();
        let grown = fs.used_data_blocks().unwrap();
        assert!(grown > baseline + 10, "expected direct + indirect blocks");

        fs.write_file(id, &pattern(BLOCK_SIZE)).unwrap();
        assert_eq!(
            fs.used_data_blocks().unwrap(),
            baseline + 1,
            "shrink leaked blocks"
        );
        assert_eq!(fs.read_file(id).unwrap(), pattern(BLOCK_SIZE));
    }

    #[test]
    fn repeated_overwrites_do_not_leak() {
        let mut fs = fresh();
        let id = fs.create_file("churn.bin").unwrap();
        fs.write_file(id, &pattern(3 * BLOCK_SIZE)).unwrap();
        let steady = fs.used_data_blocks().unwrap();

        for round in 0..25 {
            let len = (round % 14 + 1) * BLOCK_SIZE;
            fs.write_file(id, &pattern(len)).unwrap();
        }
        fs.write_file(id, &pattern(3 * BLOCK_SIZE)).unwrap();
        assert_eq!(
            fs.used_data_blocks().unwrap(),
            steady,
            "block usage drifted across repeated overwrites"
        );
    }

    #[test]
    fn delete_frees_inode_and_blocks() {
        let mut fs = fresh();
        let inodes_before = fs.used_inodes().unwrap();
        let blocks_before = fs.used_data_blocks().unwrap();

        let id = fs.create_file("gone.bin").unwrap();
        fs.write_file(id, &pattern(12 * BLOCK_SIZE)).unwrap();
        fs.delete_file("gone.bin").unwrap();

        assert_eq!(fs.used_inodes().unwrap(), inodes_before, "inode leaked");
        assert_eq!(
            fs.used_data_blocks().unwrap(),
            blocks_before,
            "data blocks leaked (including the indirect block)"
        );
        assert_eq!(fs.list_root().unwrap(), Vec::new());
        assert_eq!(fs.lookup("gone.bin").unwrap(), None);
    }

    #[test]
    fn delete_reports_a_missing_file_and_refuses_the_root() {
        let mut fs = fresh();
        assert!(fs.delete_file("nope.txt").is_err());
        // The root is a directory and is not listed in itself, so it can't
        // be reached by name; the guard is still asserted directly below.
        let id = fs.create_file("f").unwrap();
        assert_eq!(id, 1, "root occupies inode 0");
    }

    #[test]
    fn deleted_inodes_are_reused() {
        let mut fs = fresh();
        let first = fs.create_file("a").unwrap();
        fs.delete_file("a").unwrap();
        let second = fs.create_file("b").unwrap();
        assert_eq!(first, second, "freed inode should be handed out again");
    }

    #[test]
    fn rename_keeps_the_inode_and_contents() {
        let mut fs = fresh();
        let id = fs.create_file("old.txt").unwrap();
        fs.write_file(id, b"unchanged").unwrap();

        fs.rename_file("old.txt", "new.txt").unwrap();
        assert_eq!(fs.lookup("old.txt").unwrap(), None);
        assert_eq!(fs.lookup("new.txt").unwrap(), Some(id));
        assert_eq!(fs.read_file(id).unwrap(), b"unchanged");
    }

    #[test]
    fn rename_rejects_collisions_and_missing_sources() {
        let mut fs = fresh();
        fs.create_file("a.txt").unwrap();
        fs.create_file("b.txt").unwrap();
        assert!(fs.rename_file("a.txt", "b.txt").is_err(), "collision");
        assert!(fs.rename_file("ghost", "c.txt").is_err(), "missing source");
        assert!(fs.rename_file("a.txt", "").is_err(), "empty target");
        // The failed renames must have left both files alone.
        assert!(fs.lookup("a.txt").unwrap().is_some());
        assert!(fs.lookup("b.txt").unwrap().is_some());
    }

    #[test]
    fn runs_out_of_inodes_gracefully() {
        let mut fs = fresh();
        // Root holds inode 0, so MAX_INODES - 1 files fit.
        for i in 0..MAX_INODES - 1 {
            fs.create_file(&format!("f{i}")).unwrap();
        }
        let err = fs.create_file("one-too-many").unwrap_err();
        assert_eq!(err, "out of inodes");
    }

    /// The property Phase 7's demo proved by launching a second QEMU
    /// process: state written by one mount is readable by the next. Here it
    /// is the same disk handed to a fresh `SimpleFs`, which is exactly what
    /// a reboot does.
    #[test]
    fn state_survives_a_remount() {
        let disk = {
            let mut fs = fresh();
            let a = fs.create_file("keep.txt").unwrap();
            fs.write_file(a, b"persisted").unwrap();
            let b = fs.create_file("big.bin").unwrap();
            fs.write_file(b, &pattern(12 * BLOCK_SIZE)).unwrap();
            let c = fs.create_file("temp.txt").unwrap();
            fs.write_file(c, b"doomed").unwrap();
            fs.delete_file("temp.txt").unwrap();
            fs.dev
        };

        // Remount the same bytes. Nothing is carried over in memory.
        let mut fs = SimpleFs::mount_or_format(disk).expect("remount");
        assert_eq!(
            fs.superblock().total_blocks,
            DEFAULT_TOTAL_BLOCKS,
            "remount reformatted instead of reading the existing superblock"
        );

        let keep = fs.lookup("keep.txt").unwrap().expect("keep.txt survived");
        assert_eq!(fs.read_file(keep).unwrap(), b"persisted");

        let big = fs.lookup("big.bin").unwrap().expect("big.bin survived");
        assert_eq!(
            fs.read_file(big).unwrap(),
            pattern(12 * BLOCK_SIZE),
            "indirect-block file did not survive the remount"
        );

        assert_eq!(
            fs.lookup("temp.txt").unwrap(),
            None,
            "deleted file returned"
        );
        assert_eq!(fs.list_root().unwrap().len(), 2);
    }

    /// A disk that fails partway through formatting must not leave a valid
    /// superblock behind, since the superblock is what a later mount trusts.
    #[test]
    fn a_failed_format_does_not_leave_a_valid_superblock() {
        let mut disk = FakeDisk::new();
        disk.writes_before_failure = Some(3);
        assert!(
            SimpleFs::mount_or_format(disk).is_err(),
            "format should fail"
        );
    }

    /// Directory slots freed by deletion are reused, so a create/delete loop
    /// cannot exhaust the flat root directory.
    #[test]
    fn directory_slots_are_reused_across_delete_cycles() {
        let mut fs = fresh();
        for round in 0..(DIR_SLOTS * 2) {
            let name = format!("cycle{round}");
            let id = fs.create_file(&name).unwrap();
            fs.write_file(id, b"x").unwrap();
            fs.delete_file(&name).unwrap();
        }
        assert_eq!(fs.list_root().unwrap(), Vec::new());
        assert_eq!(fs.used_inodes().unwrap(), 1);
    }
}

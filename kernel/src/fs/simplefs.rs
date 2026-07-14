//! SimpleFS: an xv6/early-ext2-style on-disk filesystem over the ATA PIO
//! driver -- `[superblock][inode bitmap][data bitmap][inode table][data
//! blocks]`, 4KiB blocks (8 ATA sectors each). Inodes hold direct block
//! pointers plus one single-indirect block (the "shallow B-tree-like
//! fanout" DSA named in the plan), giving a max file size of
//! `(10 + 1024) * 4KiB` -- comfortably more than this demo ever writes.
//! The root directory is a single fixed data block of `(name, inode)`
//! entries -- flat, no subdirectories, a deliberate scope cut so Phase 7's
//! apps have somewhere real to persist to without also building directory
//! tree traversal.

use super::ata;
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

/// A heap-allocated, zeroed block buffer. Every block-sized scratch buffer
/// in this module goes through this instead of a `[u8; BLOCK_SIZE]` stack
/// local -- deeply nested calls here (e.g. `write_file` freeing an
/// indirect block while allocating fresh ones) can stack up several
/// 4KiB-sized locals at once, which blew a task's 32KiB kernel stack in
/// testing (see PLAN.md's Phase 7 log). Heap allocation makes stack depth
/// independent of how many blocks a call chain happens to touch.
fn zero_block() -> Box<[u8; BLOCK_SIZE]> {
    Box::new([0u8; BLOCK_SIZE])
}

pub const BLOCK_SIZE: usize = 4096;
const SECTORS_PER_BLOCK: u32 = (BLOCK_SIZE / ata::SECTOR_SIZE) as u32;

const MAGIC: u32 = 0xBED2_0C5F;
const SUPERBLOCK_BLOCK: u32 = 0;
const INODE_BITMAP_BLOCK: u32 = 1;
const DATA_BITMAP_BLOCK: u32 = 2;
const INODE_TABLE_BLOCK: u32 = 3;
const DATA_START_BLOCK: u32 = 4;
/// Must match the fixed size `xtask` formats `fs.img` to (see
/// `xtask/src/main.rs`'s `FS_IMAGE_BLOCKS`) -- there's no `IDENTIFY DEVICE`
/// probe here, so the format-time disk size is a shared compile-time
/// constant instead of something discovered at runtime.
const DEFAULT_TOTAL_BLOCKS: u32 = 1024; // 4 MiB

const MAX_INODES: u32 = 64;
const DIRECT_PTRS: usize = 10;
const PTRS_PER_BLOCK: usize = BLOCK_SIZE / 4;
const INODE_SIZE: usize = 8 + DIRECT_PTRS * 4 + 4; // used+kind+pad, size, direct[10], indirect = 52

const ROOT_INODE: u32 = 0;
const NAME_LEN: usize = 27;
const DIR_ENTRY_SIZE: usize = 1 + NAME_LEN + 4; // used, name, inode = 32

const KIND_FILE: u8 = 1;
const KIND_DIR: u8 = 2;

#[derive(Clone, Copy)]
struct Superblock {
    total_blocks: u32,
    total_data_blocks: u32,
}

static MOUNTED: Mutex<Option<Superblock>> = Mutex::new(None);
static FS_LOCK: Mutex<()> = Mutex::new(());

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
        Self { used: false, kind: 0, size: 0, direct: [0; DIRECT_PTRS], indirect: 0 }
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

fn read_block(block: u32, buf: &mut [u8; BLOCK_SIZE]) -> Result<(), &'static str> {
    let mut sector = [0u8; ata::SECTOR_SIZE];
    for i in 0..SECTORS_PER_BLOCK {
        ata::read_sector(block * SECTORS_PER_BLOCK + i, &mut sector)?;
        let off = i as usize * ata::SECTOR_SIZE;
        buf[off..off + ata::SECTOR_SIZE].copy_from_slice(&sector);
    }
    Ok(())
}

fn write_block(block: u32, buf: &[u8; BLOCK_SIZE]) -> Result<(), &'static str> {
    let mut sector = [0u8; ata::SECTOR_SIZE];
    for i in 0..SECTORS_PER_BLOCK {
        let off = i as usize * ata::SECTOR_SIZE;
        sector.copy_from_slice(&buf[off..off + ata::SECTOR_SIZE]);
        ata::write_sector(block * SECTORS_PER_BLOCK + i, &sector)?;
    }
    Ok(())
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

fn alloc_inode() -> Result<u32, &'static str> {
    let mut bmp = zero_block();
    read_block(INODE_BITMAP_BLOCK, &mut bmp)?;
    for i in 0..MAX_INODES {
        if !bit_get(&bmp[..], i) {
            bit_set(&mut bmp[..], i, true);
            write_block(INODE_BITMAP_BLOCK, &bmp)?;
            return Ok(i);
        }
    }
    Err("out of inodes")
}

fn alloc_data_block(total_data_blocks: u32) -> Result<u32, &'static str> {
    let mut bmp = zero_block();
    read_block(DATA_BITMAP_BLOCK, &mut bmp)?;
    for i in 0..total_data_blocks {
        if !bit_get(&bmp[..], i) {
            bit_set(&mut bmp[..], i, true);
            write_block(DATA_BITMAP_BLOCK, &bmp)?;
            return Ok(DATA_START_BLOCK + i);
        }
    }
    Err("disk full")
}

fn free_data_block(abs_block: u32) -> Result<(), &'static str> {
    if abs_block < DATA_START_BLOCK {
        return Ok(());
    }
    let mut bmp = zero_block();
    read_block(DATA_BITMAP_BLOCK, &mut bmp)?;
    bit_set(&mut bmp[..], abs_block - DATA_START_BLOCK, false);
    write_block(DATA_BITMAP_BLOCK, &bmp)
}

fn read_inode(id: u32) -> Result<DiskInode, &'static str> {
    let mut block = zero_block();
    read_block(INODE_TABLE_BLOCK, &mut block)?;
    let off = id as usize * INODE_SIZE;
    Ok(DiskInode::read_from(&block[off..off + INODE_SIZE]))
}

fn write_inode(id: u32, inode: &DiskInode) -> Result<(), &'static str> {
    let mut block = zero_block();
    read_block(INODE_TABLE_BLOCK, &mut block)?;
    let off = id as usize * INODE_SIZE;
    inode.write_to(&mut block[off..off + INODE_SIZE]);
    write_block(INODE_TABLE_BLOCK, &block)
}

fn format() -> Result<Superblock, &'static str> {
    let sb = Superblock {
        total_blocks: DEFAULT_TOTAL_BLOCKS,
        total_data_blocks: DEFAULT_TOTAL_BLOCKS - DATA_START_BLOCK,
    };

    write_block(INODE_BITMAP_BLOCK, &zero_block())?;
    write_block(DATA_BITMAP_BLOCK, &zero_block())?;
    write_block(INODE_TABLE_BLOCK, &zero_block())?;

    // Root directory: inode 0, one data block of zeroed (empty) entries.
    let root_id = alloc_inode()?;
    debug_assert_eq!(root_id, ROOT_INODE);
    let root_block = alloc_data_block(sb.total_data_blocks)?;
    write_block(root_block, &zero_block())?;
    let mut root_inode = DiskInode::empty();
    root_inode.used = true;
    root_inode.kind = KIND_DIR;
    root_inode.direct[0] = root_block;
    write_inode(ROOT_INODE, &root_inode)?;

    // Superblock written last: a crash mid-format leaves a bad magic
    // (re-formatted next boot) instead of a superblock that claims a
    // filesystem which was never actually finished being laid out.
    let mut sb_block = zero_block();
    sb_block[0..4].copy_from_slice(&MAGIC.to_le_bytes());
    sb_block[4..8].copy_from_slice(&sb.total_blocks.to_le_bytes());
    sb_block[8..12].copy_from_slice(&sb.total_data_blocks.to_le_bytes());
    write_block(SUPERBLOCK_BLOCK, &sb_block)?;

    Ok(sb)
}

/// Mounts the filesystem, formatting it fresh if the on-disk magic doesn't
/// match (first boot, or a disk image that was never formatted).
pub fn init() {
    ata::disable_interrupts();
    let _guard = FS_LOCK.lock();
    let mut sb_block = zero_block();
    let sb = match read_block(SUPERBLOCK_BLOCK, &mut sb_block) {
        Ok(()) if u32::from_le_bytes(sb_block[0..4].try_into().unwrap()) == MAGIC => Superblock {
            total_blocks: u32::from_le_bytes(sb_block[4..8].try_into().unwrap()),
            total_data_blocks: u32::from_le_bytes(sb_block[8..12].try_into().unwrap()),
        },
        Ok(()) => format().expect("SimpleFS format failed"),
        Err(_) => format().expect("SimpleFS format failed"),
    };
    crate::serial_println!(
        "SimpleFS mounted: {} blocks total, {} data blocks.",
        sb.total_blocks,
        sb.total_data_blocks
    );
    *MOUNTED.lock() = Some(sb);
}

fn superblock() -> Superblock {
    MOUNTED
        .lock()
        .as_ref()
        .copied()
        .expect("SimpleFS not mounted -- call fs::simplefs::init() first")
}

fn dir_entry_at(block: &[u8; BLOCK_SIZE], slot: usize) -> (bool, String, u32) {
    let off = slot * DIR_ENTRY_SIZE;
    let used = block[off] != 0;
    let name_bytes = &block[off + 1..off + 1 + NAME_LEN];
    let name_len = name_bytes.iter().position(|&b| b == 0).unwrap_or(NAME_LEN);
    let name = String::from_utf8_lossy(&name_bytes[..name_len]).into_owned();
    let inode = u32::from_le_bytes(block[off + 1 + NAME_LEN..off + 1 + NAME_LEN + 4].try_into().unwrap());
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

/// Lists the root directory: `(name, inode id, size in bytes)`.
pub fn list_root() -> Vec<(String, u32, u32)> {
    let _guard = FS_LOCK.lock();
    let root = read_inode(ROOT_INODE).expect("root inode read failed");
    let mut dir_block = zero_block();
    read_block(root.direct[0], &mut dir_block).expect("root dir block read failed");

    let mut out = Vec::new();
    for slot in 0..(BLOCK_SIZE / DIR_ENTRY_SIZE) {
        let (used, name, inode) = dir_entry_at(&dir_block, slot);
        if used {
            let size = read_inode(inode).map(|i| i.size).unwrap_or(0);
            out.push((name, inode, size));
        }
    }
    out
}

/// Creates an empty file in the root directory, returning its inode id.
pub fn create_file(name: &str) -> Result<u32, &'static str> {
    let _guard = FS_LOCK.lock();
    let root = read_inode(ROOT_INODE)?;
    let mut dir_block = zero_block();
    read_block(root.direct[0], &mut dir_block)?;

    let mut free_slot = None;
    for slot in 0..(BLOCK_SIZE / DIR_ENTRY_SIZE) {
        let (used, existing_name, _) = dir_entry_at(&dir_block, slot);
        if used && existing_name == name {
            return Err("a file with that name already exists");
        }
        if !used && free_slot.is_none() {
            free_slot = Some(slot);
        }
    }
    let slot = free_slot.ok_or("root directory is full")?;

    let id = alloc_inode()?;
    let mut inode = DiskInode::empty();
    inode.used = true;
    inode.kind = KIND_FILE;
    write_inode(id, &inode)?;

    write_dir_entry(&mut dir_block, slot, true, name, id);
    write_block(root.direct[0], &dir_block)?;
    Ok(id)
}

/// Reads a file's full contents.
pub fn read_file(inode_id: u32) -> Result<Vec<u8>, &'static str> {
    let _guard = FS_LOCK.lock();
    let inode = read_inode(inode_id)?;
    let mut out = Vec::with_capacity(inode.size as usize);
    let mut remaining = inode.size as usize;

    for &block in inode.direct.iter() {
        if remaining == 0 || block == 0 {
            break;
        }
        let mut buf = zero_block();
        read_block(block, &mut buf)?;
        let take = remaining.min(BLOCK_SIZE);
        out.extend_from_slice(&buf[..take]);
        remaining -= take;
    }

    if remaining > 0 && inode.indirect != 0 {
        let mut ptrs_block = zero_block();
        read_block(inode.indirect, &mut ptrs_block)?;
        for i in 0..PTRS_PER_BLOCK {
            if remaining == 0 {
                break;
            }
            let ptr = u32::from_le_bytes(ptrs_block[i * 4..i * 4 + 4].try_into().unwrap());
            if ptr == 0 {
                break;
            }
            let mut buf = zero_block();
            read_block(ptr, &mut buf)?;
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
pub fn write_file(inode_id: u32, data: &[u8]) -> Result<(), &'static str> {
    let _guard = FS_LOCK.lock();
    let sb = superblock();
    let mut inode = read_inode(inode_id)?;

    for ptr in inode.direct.iter() {
        if *ptr != 0 {
            free_data_block(*ptr)?;
        }
    }
    if inode.indirect != 0 {
        let mut ptrs_block = zero_block();
        read_block(inode.indirect, &mut ptrs_block)?;
        for i in 0..PTRS_PER_BLOCK {
            let ptr = u32::from_le_bytes(ptrs_block[i * 4..i * 4 + 4].try_into().unwrap());
            if ptr != 0 {
                free_data_block(ptr)?;
            }
        }
        free_data_block(inode.indirect)?;
    }
    inode.direct = [0; DIRECT_PTRS];
    inode.indirect = 0;

    let blocks_needed = data.len().div_ceil(BLOCK_SIZE);
    if blocks_needed > DIRECT_PTRS + PTRS_PER_BLOCK {
        return Err("file too large for this filesystem");
    }

    let mut indirect_ptrs = zero_block();
    let mut using_indirect = false;

    for i in 0..blocks_needed {
        let chunk_start = i * BLOCK_SIZE;
        let chunk_end = (chunk_start + BLOCK_SIZE).min(data.len());
        let mut buf = zero_block();
        buf[..chunk_end - chunk_start].copy_from_slice(&data[chunk_start..chunk_end]);

        let block = alloc_data_block(sb.total_data_blocks)?;
        write_block(block, &buf)?;

        if i < DIRECT_PTRS {
            inode.direct[i] = block;
        } else {
            if inode.indirect == 0 {
                inode.indirect = alloc_data_block(sb.total_data_blocks)?;
            }
            using_indirect = true;
            let idx = i - DIRECT_PTRS;
            indirect_ptrs[idx * 4..idx * 4 + 4].copy_from_slice(&block.to_le_bytes());
        }
    }
    if using_indirect {
        write_block(inode.indirect, &indirect_ptrs)?;
    }

    inode.size = data.len() as u32;
    write_inode(inode_id, &inode)?;
    Ok(())
}

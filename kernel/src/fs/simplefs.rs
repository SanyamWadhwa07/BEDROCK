//! SimpleFS, the kernel's binding to the on-disk filesystem.
//!
//! The layout itself (superblock, the two free-space bitmaps, inode
//! indexing with direct + single-indirect pointers, the flat root
//! directory) lives in the `bedrock-fs` crate, where it is generic over a
//! `BlockDevice` and covered by ordinary `cargo test` against an in-memory
//! fake disk. That is deliberate: the free-and-reallocate paths are where a
//! filesystem corrupts itself, and testing them should not require booting
//! QEMU. The host tests include a remount check, which is the same property
//! Phase 7 demonstrated by launching a second QEMU process.
//!
//! What lives here is only what needs the target: the block device backed
//! by the ATA PIO driver and its LRU cache, the global mount, and the lock
//! that serializes access from every task.

use super::ata;
use super::cache;
use alloc::string::String;
use alloc::vec::Vec;
use bedrock_fs::{BlockDevice, SimpleFs};
use spin::Mutex;

pub use bedrock_fs::{BLOCK_SIZE, DEFAULT_TOTAL_BLOCKS};

/// Re-export of the block-cache controls for the shell and bench reporting.
pub use super::cache::{set_enabled as set_cache_enabled, stats as cache_stats};

/// The kernel's block device: ATA PIO underneath, LRU block cache in front.
/// Write-through, so on-disk durability is exactly what it would be with no
/// cache at all.
pub struct CachedAtaDevice;

impl BlockDevice for CachedAtaDevice {
    fn read_block(&mut self, block: u32, buf: &mut [u8; BLOCK_SIZE]) -> Result<(), &'static str> {
        cache::read_block(block, buf)
    }

    fn write_block(&mut self, block: u32, buf: &[u8; BLOCK_SIZE]) -> Result<(), &'static str> {
        cache::write_block(block, buf)
    }
}

pub type Fs = SimpleFs<CachedAtaDevice>;

/// The mounted filesystem. This mutex is the single serialization point for
/// all disk access: the ATA driver has no lock of its own, so anything
/// touching the disk (including raw port access from `bench ata`) has to
/// hold this.
static FS: Mutex<Option<Fs>> = Mutex::new(None);

/// Exposes the filesystem lock for callers that need raw ATA access
/// (`bench ata`), so their port I/O can't interleave with a filesystem
/// transfer from another task.
pub fn lock_disk() -> spin::MutexGuard<'static, Option<Fs>> {
    FS.lock()
}

/// Mounts the filesystem, formatting it fresh if the on-disk magic doesn't
/// match (first boot, or a disk image that was never formatted).
pub fn init() {
    // Must happen before any transfer: a PIO-only driver still has to
    // actively silence the controller's own IRQ14. See DESIGN.md.
    ata::disable_interrupts();

    let fs = SimpleFs::mount_or_format(CachedAtaDevice).expect("SimpleFS mount/format failed");
    let sb = fs.superblock();
    crate::serial_println!(
        "SimpleFS mounted: {} blocks total, {} data blocks.",
        sb.total_blocks,
        sb.total_data_blocks
    );
    *FS.lock() = Some(fs);
}

/// Runs `f` against the mounted filesystem, panicking if nothing is
/// mounted. Every public operation below funnels through here so the lock
/// is held for exactly one operation and released before returning.
fn with_fs<T>(f: impl FnOnce(&mut Fs) -> T) -> T {
    let mut guard = FS.lock();
    let fs = guard
        .as_mut()
        .expect("SimpleFS not mounted -- call fs::simplefs::init() first");
    f(fs)
}

/// Lists the root directory: `(name, inode id, size in bytes)`.
pub fn list_root() -> Vec<(String, u32, u32)> {
    with_fs(|fs| fs.list_root().unwrap_or_default())
}

/// Creates an empty file in the root directory, returning its inode id.
pub fn create_file(name: &str) -> Result<u32, &'static str> {
    with_fs(|fs| fs.create_file(name))
}

/// Reads a file's full contents.
pub fn read_file(inode_id: u32) -> Result<Vec<u8>, &'static str> {
    with_fs(|fs| fs.read_file(inode_id))
}

/// Overwrites a file's contents, releasing whatever blocks it used to own.
pub fn write_file(inode_id: u32, data: &[u8]) -> Result<(), &'static str> {
    with_fs(|fs| fs.write_file(inode_id, data))
}

/// Deletes a file by name, freeing its data blocks and its inode.
pub fn delete_file(name: &str) -> Result<(), &'static str> {
    with_fs(|fs| fs.delete_file(name))
}

/// Renames a file in the root directory, keeping its inode and contents.
pub fn rename_file(old: &str, new: &str) -> Result<(), &'static str> {
    with_fs(|fs| fs.rename_file(old, new))
}

//! LRU block cache between SimpleFS and the ATA PIO driver -- the single
//! biggest FS performance lever in this kernel, since every uncached block
//! access costs 8 polled PIO sector round trips.
//!
//! **Write-through**: writes always go to the disk before returning, so
//! crash-durability is exactly what it was before the cache existed (no
//! journal means no delayed-write reordering questions to answer). Reads
//! hit the cache; misses fill it, evicting least-recently-used.
//!
//! Runtime-toggleable (`fscache on|off` on the serial shell) so `bench fs`
//! can measure the same workload both ways in one boot.

use super::ata;
use super::simplefs::BLOCK_SIZE;
use alloc::boxed::Box;
use bedrock_lru::LruCache;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use spin::Mutex;

/// 64 x 4KiB = 256 KiB of the 8 MiB kernel heap -- enough to hold all four
/// metadata blocks plus a typical working set of file data.
const CACHE_BLOCKS: usize = 64;
const SECTORS_PER_BLOCK: u32 = (BLOCK_SIZE / ata::SECTOR_SIZE) as u32;

static CACHE: Mutex<Option<LruCache<Box<[u8; BLOCK_SIZE]>>>> = Mutex::new(None);
static ENABLED: AtomicBool = AtomicBool::new(true);
static HITS: AtomicU64 = AtomicU64::new(0);
static MISSES: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy)]
pub struct CacheStats {
    pub enabled: bool,
    pub hits: u64,
    pub misses: u64,
    pub resident: usize,
    pub capacity: usize,
}

pub fn stats() -> CacheStats {
    CacheStats {
        enabled: ENABLED.load(Ordering::Relaxed),
        hits: HITS.load(Ordering::Relaxed),
        misses: MISSES.load(Ordering::Relaxed),
        resident: CACHE.lock().as_ref().map(|c| c.len()).unwrap_or(0),
        capacity: CACHE_BLOCKS,
    }
}

/// Enables/disables the cache at runtime. Disabling drops every cached
/// block (write-through means nothing is dirty, so dropping is safe).
pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
    if !enabled {
        if let Some(c) = CACHE.lock().as_mut() {
            c.clear();
        }
    }
}

fn ata_read_block(block: u32, buf: &mut [u8; BLOCK_SIZE]) -> Result<(), &'static str> {
    let mut sector = [0u8; ata::SECTOR_SIZE];
    for i in 0..SECTORS_PER_BLOCK {
        ata::read_sector(block * SECTORS_PER_BLOCK + i, &mut sector)?;
        let off = i as usize * ata::SECTOR_SIZE;
        buf[off..off + ata::SECTOR_SIZE].copy_from_slice(&sector);
    }
    Ok(())
}

fn ata_write_block(block: u32, buf: &[u8; BLOCK_SIZE]) -> Result<(), &'static str> {
    let mut sector = [0u8; ata::SECTOR_SIZE];
    for i in 0..SECTORS_PER_BLOCK {
        let off = i as usize * ata::SECTOR_SIZE;
        sector.copy_from_slice(&buf[off..off + ata::SECTOR_SIZE]);
        ata::write_sector(block * SECTORS_PER_BLOCK + i, &sector)?;
    }
    Ok(())
}

pub fn read_block(block: u32, buf: &mut [u8; BLOCK_SIZE]) -> Result<(), &'static str> {
    if !ENABLED.load(Ordering::Relaxed) {
        return ata_read_block(block, buf);
    }
    let mut guard = CACHE.lock();
    let cache = guard.get_or_insert_with(|| LruCache::new(CACHE_BLOCKS));
    if let Some(cached) = cache.get(block) {
        HITS.fetch_add(1, Ordering::Relaxed);
        buf.copy_from_slice(&cached[..]);
        return Ok(());
    }
    MISSES.fetch_add(1, Ordering::Relaxed);
    ata_read_block(block, buf)?;
    cache.put(block, Box::new(*buf));
    Ok(())
}

pub fn write_block(block: u32, buf: &[u8; BLOCK_SIZE]) -> Result<(), &'static str> {
    // Disk first (write-through): if the write fails, the cache never
    // learns contents the disk doesn't have.
    ata_write_block(block, buf)?;
    if ENABLED.load(Ordering::Relaxed) {
        let mut guard = CACHE.lock();
        let cache = guard.get_or_insert_with(|| LruCache::new(CACHE_BLOCKS));
        cache.put(block, Box::new(*buf));
    }
    Ok(())
}

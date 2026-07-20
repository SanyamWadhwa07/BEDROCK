//! In-kernel benchmark suite, permanently wired to the serial shell
//! (`bench <ata|alloc|fs|ctxswitch|all>`) so every number in the README is
//! reproducible by anyone who boots the kernel -- unlike the original
//! Phase-7 numbers, which came from temporary harnesses that were deleted
//! after capture.
//!
//! Everything is timed twice where feasible: RDTSC (calibrated against the
//! PIT at boot, see `task::timer::calibrate_tsc`) for resolution, and raw
//! PIT ticks as a coarse cross-check that the calibration isn't lying.

use crate::fs::{ata, simplefs};
use crate::serial_println;
use crate::task::context_switch::switch_to;
use crate::task::tcb::{alloc_task_stack, prepare_initial_stack};
use crate::task::timer;
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

/// First block of the raw-ATA benchmark scratch region: immediately past
/// the blocks SimpleFS owns. `xtask` sizes `fs.img` to include this extra
/// region (see `BENCH_SCRATCH_BLOCKS` there), so raw writes here can never
/// touch filesystem data.
const ATA_SCRATCH_START_BLOCK: u32 = simplefs::DEFAULT_TOTAL_BLOCKS;
const ATA_SCRATCH_BLOCKS: u32 = 64; // 256 KiB, matching the original Phase-7 methodology
const SECTORS_PER_BLOCK: u32 = (simplefs::BLOCK_SIZE / ata::SECTOR_SIZE) as u32;

pub fn run(arg: &str) {
    if timer::tsc_per_ms() == 0 {
        serial_println!("bench: TSC not calibrated yet -- try again in a second");
        return;
    }
    match arg {
        "ata" => bench_ata(),
        "alloc" => bench_alloc(),
        "fs" => bench_fs(),
        "ctxswitch" => bench_ctxswitch(),
        "all" => {
            bench_ctxswitch();
            bench_alloc();
            bench_ata();
            bench_fs();
        }
        _ => serial_println!("usage: bench <ata|alloc|fs|ctxswitch|all>"),
    }
}

fn report_throughput(what: &str, bytes: u64, cycles: u64, ticks: u64) {
    let us = timer::cycles_to_us(cycles).max(1);
    let kib_per_s = bytes * 1_000_000 / 1024 / us;
    let tick_ms = timer::ticks_to_ms(ticks);
    serial_println!(
        "  {}: {} KiB in {}.{:03}ms -> ~{} KiB/s  (PIT cross-check: ~{}ms)",
        what,
        bytes / 1024,
        us / 1000,
        us % 1000,
        kib_per_s,
        tick_ms
    );
}

/// Sequential raw-disk throughput over the ATA PIO driver, bypassing the
/// filesystem (and, once it exists, the block cache): 64 x 4KiB blocks
/// written then read against a dedicated scratch region past the FS.
fn bench_ata() {
    serial_println!("bench ata: 256 KiB sequential raw write+read (PIO, no cache)...");
    // The ATA driver has no lock of its own -- all normal access is
    // serialized by the FS lock, so raw benching must hold it too or a GUI
    // `ls` could interleave port accesses mid-transfer.
    let _disk = simplefs::lock_disk();

    let mut sector = [0u8; ata::SECTOR_SIZE];
    for (i, b) in sector.iter_mut().enumerate() {
        *b = (i % 251) as u8;
    }

    let total_sectors = ATA_SCRATCH_BLOCKS * SECTORS_PER_BLOCK;
    let bytes = total_sectors as u64 * ata::SECTOR_SIZE as u64;
    let base = ATA_SCRATCH_START_BLOCK * SECTORS_PER_BLOCK;

    let t0 = timer::ticks();
    let c0 = timer::rdtsc();
    for s in 0..total_sectors {
        if ata::write_sector(base + s, &sector).is_err() {
            serial_println!(
                "  write error at sector {} -- is fs.img large enough? (rerun cargo xtask run)",
                s
            );
            return;
        }
    }
    let c1 = timer::rdtsc();
    let t1 = timer::ticks();
    report_throughput("write", bytes, c1 - c0, t1 - t0);

    let mut readback = [0u8; ata::SECTOR_SIZE];
    let t0 = timer::ticks();
    let c0 = timer::rdtsc();
    for s in 0..total_sectors {
        if ata::read_sector(base + s, &mut readback).is_err() {
            serial_println!("  read error at sector {}", s);
            return;
        }
    }
    let c1 = timer::rdtsc();
    let t1 = timer::ticks();
    report_throughput("read ", bytes, c1 - c0, t1 - t0);

    if readback != sector {
        serial_println!("  VERIFY FAILED: last sector read back differently than written");
    }
}

/// Heap + physical-frame allocator throughput: the same workload shape as
/// the `alloctest` leak check, but timed.
fn bench_alloc() {
    const ROUNDS: u64 = 200;
    serial_println!(
        "bench alloc: {} rounds of Box/Vec churn + 16 frame alloc/free...",
        ROUNDS
    );

    let c0 = timer::rdtsc();
    for _ in 0..ROUNDS {
        let mut boxes: Vec<Box<[u8; 4096]>> = Vec::new();
        for i in 0..20u32 {
            boxes.push(Box::new([i as u8; 4096]));
        }
        boxes.remove(5);
        boxes.remove(10);
        boxes.truncate(3);
        drop(boxes);

        let mut frames = Vec::new();
        for _ in 0..16 {
            if let Some(f) = crate::memory::frame_allocator::alloc_frame() {
                frames.push(f);
            }
        }
        for f in frames {
            unsafe { crate::memory::frame_allocator::dealloc_frame(f) };
        }
    }
    let c1 = timer::rdtsc();

    let us = timer::cycles_to_us(c1 - c0).max(1);
    // 20 heap allocs + 20 frees + 16 frame allocs + 16 frees per round.
    let ops = ROUNDS * (20 + 20 + 16 + 16);
    serial_println!(
        "  {} alloc/free ops in {}.{:03}ms -> ~{} ops/s",
        ops,
        us / 1000,
        us % 1000,
        ops * 1_000_000 / us
    );
}

/// End-to-end SimpleFS latency: create, write (64 KiB -- deep enough to
/// exercise the single-indirect block), read-back+verify, list, delete.
/// This is the number the LRU block cache exists to improve.
fn bench_fs() {
    const NAME: &str = "bench.tmp";
    const SIZE: usize = 64 * 1024;
    serial_println!(
        "bench fs: create/write/read/list/delete of a {} KiB file...",
        SIZE / 1024
    );

    // Leftover scratch file from an interrupted earlier run.
    let _ = simplefs::delete_file(NAME);

    let mut data = Vec::with_capacity(SIZE);
    for i in 0..SIZE {
        data.push((i % 253) as u8);
    }

    let c0 = timer::rdtsc();
    let inode = match simplefs::create_file(NAME) {
        Ok(i) => i,
        Err(e) => {
            serial_println!("  create failed: {}", e);
            return;
        }
    };
    let c_create = timer::rdtsc();
    if let Err(e) = simplefs::write_file(inode, &data) {
        serial_println!("  write failed: {}", e);
        return;
    }
    let c_write = timer::rdtsc();
    let back = match simplefs::read_file(inode) {
        Ok(b) => b,
        Err(e) => {
            serial_println!("  read failed: {}", e);
            return;
        }
    };
    let c_read = timer::rdtsc();
    let listing = simplefs::list_root();
    let c_list = timer::rdtsc();
    let deleted = simplefs::delete_file(NAME);
    let c_delete = timer::rdtsc();

    let us = |a: u64, b: u64| timer::cycles_to_us(b - a);
    serial_println!("  create: {} us", us(c0, c_create));
    serial_println!(
        "  write:  {} us ({} KiB -> ~{} KiB/s)",
        us(c_create, c_write),
        SIZE / 1024,
        SIZE as u64 * 1_000_000 / 1024 / us(c_create, c_write).max(1)
    );
    serial_println!(
        "  read:   {} us ({} KiB -> ~{} KiB/s)",
        us(c_write, c_read),
        SIZE / 1024,
        SIZE as u64 * 1_000_000 / 1024 / us(c_write, c_read).max(1)
    );
    serial_println!(
        "  list:   {} us ({} entries)",
        us(c_read, c_list),
        listing.len()
    );
    serial_println!("  delete: {} us", us(c_list, c_delete));

    if back != data {
        serial_println!("  VERIFY FAILED: read-back differs from what was written");
    }
    if let Err(e) = deleted {
        serial_println!("  delete failed: {}", e);
    }
    let stats = simplefs::cache_stats();
    serial_println!(
        "  block cache: {} ({} hits / {} misses this boot)",
        if stats.enabled { "ON" } else { "OFF" },
        stats.hits,
        stats.misses
    );
}

// --- context-switch latency ------------------------------------------------

/// The benchmark fiber's saved stack pointer, and the main context's.
/// `AtomicU64::as_ptr` gives `switch_to` the `*mut u64` it wants without
/// needing `static mut`.
static MAIN_RSP: AtomicU64 = AtomicU64::new(0);
static FIBER_RSP: AtomicU64 = AtomicU64::new(0);
/// The fiber's stack is allocated once and reused across bench runs, so
/// repeated `bench ctxswitch` invocations don't leak task-stack slots.
static FIBER_STACK_TOP: AtomicU64 = AtomicU64::new(0);

extern "C" fn bench_fiber() -> ! {
    // The spawn trampoline `sti`s unconditionally (new tasks need it);
    // this bench wants interrupts *off* so the timer can't preempt a
    // measured switch. IF is a global CPU flag, so disabling it here
    // holds for the rest of the measured loop, on both sides.
    x86_64::instructions::interrupts::disable();
    loop {
        unsafe {
            switch_to(FIBER_RSP.as_ptr(), MAIN_RSP.load(Ordering::Relaxed));
        }
    }
}

/// Measures the raw cost of the naked-fn context switch itself: a
/// ping-pong between the calling context and a dedicated fiber, bypassing
/// the scheduler and ready queue entirely (in-band measurement through the
/// scheduler would be dominated by other tasks' ~50ms time slices).
fn bench_ctxswitch() {
    const ITERS: u64 = 100_000;
    serial_println!(
        "bench ctxswitch: {} ping-pong round trips (interrupts off)...",
        ITERS
    );

    let were_enabled = x86_64::instructions::interrupts::are_enabled();
    x86_64::instructions::interrupts::disable();

    let mut top = FIBER_STACK_TOP.load(Ordering::Relaxed);
    if top == 0 {
        top = alloc_task_stack().as_u64();
        FIBER_STACK_TOP.store(top, Ordering::Relaxed);
    }
    FIBER_RSP.store(
        prepare_initial_stack(x86_64::VirtAddr::new(top), bench_fiber),
        Ordering::Relaxed,
    );

    // Warm-up round trip: runs the spawn trampoline (whose `sti` the fiber
    // immediately undoes) and leaves the fiber parked mid-`switch_to`, so
    // every measured iteration is a symmetric save/restore pair.
    unsafe {
        switch_to(MAIN_RSP.as_ptr(), FIBER_RSP.load(Ordering::Relaxed));
    }

    let c0 = timer::rdtsc();
    for _ in 0..ITERS {
        unsafe {
            switch_to(MAIN_RSP.as_ptr(), FIBER_RSP.load(Ordering::Relaxed));
        }
    }
    let c1 = timer::rdtsc();

    if were_enabled {
        x86_64::instructions::interrupts::enable();
    }

    let switches = ITERS * 2; // each round trip is two one-way switches
    let cycles_per_switch = (c1 - c0) / switches;
    let ns_total = timer::cycles_to_ns(c1 - c0);
    serial_println!(
        "  {} one-way switches: ~{} cycles (~{} ns) per switch",
        switches,
        cycles_per_switch,
        ns_total / switches
    );
}

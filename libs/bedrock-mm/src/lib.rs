//! The physical frame allocator's core: a **bitset** over frame indices,
//! 1 bit per frame, packed into `u8` words and indexed with the classic
//! `idx / 8` + `1 << (idx % 8)` bit manipulation.
//!
//! Deliberately knows nothing about `PhysFrame`, `PhysAddr`, or the
//! bootloader's memory map -- it allocates *indices*. The kernel wraps it in
//! the address arithmetic and the `FrameAllocator` trait impl. Keeping the
//! bit twiddling free of address types is what lets the tricky part (scan
//! wraparound, the cursor hint, double-free handling) be tested on the host.
//!
//! Two properties the naive version didn't have, both load-bearing at real
//! sizes (a 128 MiB machine is ~32k frames):
//!
//! - `free_count()` is O(1), maintained incrementally, instead of counting
//!   every bit on each call (the shell's `meminfo` and `alloctest` call it
//!   constantly).
//! - `alloc()` skips fully-allocated bytes eight frames at a time rather
//!   than testing every bit, so a nearly-full bitmap scans ~8x faster.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;

/// All bits set = every frame in this byte is allocated.
const FULL_BYTE: u8 = 0xFF;

pub struct FrameBitmap {
    bits: Vec<u8>,
    frame_count: usize,
    free_count: usize,
    /// Rotating scan cursor, so allocation doesn't restart at 0 every call.
    next_hint: usize,
}

impl FrameBitmap {
    /// Creates a bitmap covering `frame_count` frames with everything marked
    /// **allocated**. The caller then calls [`set_free`] for each frame the
    /// memory map says is genuinely usable -- defaulting to "used" means a
    /// frame missing from the map can never be handed out by accident.
    pub fn new_all_allocated(frame_count: usize) -> Self {
        Self {
            bits: vec![FULL_BYTE; frame_count.div_ceil(8)],
            frame_count,
            free_count: 0,
            next_hint: 0,
        }
    }

    pub fn frame_count(&self) -> usize {
        self.frame_count
    }

    pub fn free_count(&self) -> usize {
        self.free_count
    }

    pub fn used_count(&self) -> usize {
        self.frame_count - self.free_count
    }

    pub fn is_free(&self, idx: usize) -> bool {
        idx < self.frame_count && self.bits[idx / 8] & (1 << (idx % 8)) == 0
    }

    /// Marks a frame free. Idempotent: freeing an already-free frame leaves
    /// the count correct rather than inflating it (the kernel's double-free
    /// guard, since `free_count` is maintained incrementally).
    pub fn set_free(&mut self, idx: usize) {
        if idx >= self.frame_count || self.is_free(idx) {
            return;
        }
        self.bits[idx / 8] &= !(1 << (idx % 8));
        self.free_count += 1;
    }

    /// Marks a frame allocated. Idempotent, for the same reason.
    pub fn set_allocated(&mut self, idx: usize) {
        if idx >= self.frame_count || !self.is_free(idx) {
            return;
        }
        self.bits[idx / 8] |= 1 << (idx % 8);
        self.free_count -= 1;
    }

    /// Allocates the next free frame index, scanning from the rotating
    /// cursor and wrapping. Returns `None` only when genuinely full.
    pub fn alloc(&mut self) -> Option<usize> {
        if self.free_count == 0 {
            return None;
        }
        let mut idx = self.next_hint % self.frame_count;
        let mut scanned = 0;
        while scanned < self.frame_count {
            // Fast path: an entirely-allocated byte skips eight frames at
            // once. Only worth taking when aligned, so the bit we'd be
            // starting mid-byte on isn't skipped over.
            if idx.is_multiple_of(8) && self.bits[idx / 8] == FULL_BYTE {
                idx = (idx + 8) % self.frame_count;
                scanned += 8;
                continue;
            }
            if self.is_free(idx) {
                self.set_allocated(idx);
                self.next_hint = (idx + 1) % self.frame_count;
                return Some(idx);
            }
            idx = (idx + 1) % self.frame_count;
            scanned += 1;
        }
        None
    }

    /// Frees a previously-allocated frame index.
    pub fn free(&mut self, idx: usize) {
        self.set_free(idx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bitmap of `n` frames, all free -- the shape most tests want.
    fn all_free(n: usize) -> FrameBitmap {
        let mut b = FrameBitmap::new_all_allocated(n);
        for i in 0..n {
            b.set_free(i);
        }
        b
    }

    #[test]
    fn starts_fully_allocated() {
        let b = FrameBitmap::new_all_allocated(100);
        assert_eq!(b.free_count(), 0);
        assert_eq!(b.used_count(), 100);
        assert!(!b.is_free(0));
        assert!(!b.is_free(99));
    }

    #[test]
    fn alloc_on_a_full_bitmap_returns_none() {
        let mut b = FrameBitmap::new_all_allocated(64);
        assert_eq!(b.alloc(), None);
    }

    #[test]
    fn allocates_every_frame_exactly_once() {
        const N: usize = 200;
        let mut b = all_free(N);
        let mut seen = [false; N];
        for _ in 0..N {
            let idx = b.alloc().expect("should still have frames");
            assert!(!seen[idx], "frame {idx} handed out twice");
            seen[idx] = true;
        }
        assert_eq!(b.alloc(), None, "must be exhausted");
        assert_eq!(b.free_count(), 0);
        assert!(seen.iter().all(|&s| s), "some frame was never allocated");
    }

    #[test]
    fn freed_frames_are_reused() {
        let mut b = all_free(32);
        let a = b.alloc().unwrap();
        let c = b.alloc().unwrap();
        assert_ne!(a, c);
        b.free(a);
        assert_eq!(b.free_count(), 31);
        // Exhaust everything; the freed frame must reappear.
        let mut got = vec![];
        while let Some(i) = b.alloc() {
            got.push(i);
        }
        assert!(got.contains(&a), "freed frame never came back");
    }

    #[test]
    fn double_free_does_not_inflate_the_free_count() {
        let mut b = all_free(16);
        let idx = b.alloc().unwrap();
        assert_eq!(b.free_count(), 15);
        b.free(idx);
        b.free(idx);
        b.free(idx);
        assert_eq!(b.free_count(), 16, "double free corrupted the count");
        // And the frame is still only handed out once afterwards.
        let mut count = 0;
        while b.alloc().is_some() {
            count += 1;
        }
        assert_eq!(count, 16);
    }

    #[test]
    fn out_of_range_indices_are_ignored() {
        let mut b = all_free(8);
        assert!(!b.is_free(8), "past the end is never free");
        b.set_free(9999);
        b.set_allocated(9999);
        assert_eq!(b.free_count(), 8, "out-of-range ops must not corrupt state");
    }

    #[test]
    fn scan_wraps_around_past_the_cursor() {
        let mut b = all_free(24);
        // Drain everything, then free only a low frame. The cursor is now
        // parked past it, so allocation must wrap to find it.
        while b.alloc().is_some() {}
        b.free(1);
        assert_eq!(b.alloc(), Some(1), "cursor failed to wrap");
    }

    #[test]
    fn handles_a_non_byte_multiple_frame_count() {
        // 13 frames = one and a bit bytes; the trailing bits of the last
        // byte are padding and must never be allocated.
        let mut b = all_free(13);
        let mut got = vec![];
        while let Some(i) = b.alloc() {
            got.push(i);
        }
        assert_eq!(got.len(), 13, "padding bits leaked into allocation");
        assert!(got.iter().all(|&i| i < 13));
    }

    #[test]
    fn full_byte_skip_finds_a_frame_behind_full_bytes() {
        // Frames 0..64 allocated, 70 free: exercises the eight-at-a-time
        // fast path and then the per-bit path within the final byte.
        let mut b = FrameBitmap::new_all_allocated(128);
        b.set_free(70);
        assert_eq!(b.alloc(), Some(70));
        assert_eq!(b.alloc(), None);
    }

    /// The invariant the kernel actually depends on: a frame handed out by
    /// `alloc` is never handed out again until it is freed. Checked against
    /// a reference model under a randomized alloc/free stream -- this is the
    /// property whose violation would mean two tasks sharing a stack.
    #[test]
    fn never_double_allocates_under_random_churn() {
        const N: usize = 512;
        let mut b = all_free(N);
        let mut live = vec![false; N];
        let mut outstanding: Vec<usize> = Vec::new();

        let mut rng: u64 = 0xA5A5_1234_DEAD_0001;
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };

        for op in 0..200_000u64 {
            // Bias towards allocation so the bitmap spends time near-full,
            // which is where the skip-ahead and wraparound paths matter.
            if next() % 3 != 0 || outstanding.is_empty() {
                if let Some(idx) = b.alloc() {
                    assert!(idx < N);
                    assert!(!live[idx], "op {op}: frame {idx} double-allocated");
                    live[idx] = true;
                    outstanding.push(idx);
                } else {
                    assert_eq!(outstanding.len(), N, "op {op}: alloc failed early");
                }
            } else {
                let pos = (next() as usize) % outstanding.len();
                let idx = outstanding.swap_remove(pos);
                live[idx] = false;
                b.free(idx);
            }
            assert_eq!(
                b.free_count(),
                N - outstanding.len(),
                "op {op}: free_count drifted from reality"
            );
        }
    }
}

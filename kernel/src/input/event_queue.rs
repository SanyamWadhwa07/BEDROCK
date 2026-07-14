//! Lock-free SPSC (single-producer / single-consumer) ring buffer -- the
//! classic circular-buffer DSA, done with atomics instead of a lock because
//! the producer is an interrupt handler: an ISR must never spin on a lock
//! its own interrupted task might hold.
//!
//! Monotonic wrapping counters (`head` only ever written by the producer,
//! `tail` only by the consumer) with a power-of-two capacity so `% N` is a
//! mask. Release/acquire pairing: the producer stores the byte, then
//! publishes with a Release store to `head`; the consumer Acquire-loads
//! `head` before reading the byte, so the byte write can never be observed
//! after the index that announces it.

use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

pub struct ByteRing<const N: usize> {
    buf: [AtomicU8; N],
    head: AtomicUsize, // producer-owned write counter
    tail: AtomicUsize, // consumer-owned read counter
}

impl<const N: usize> ByteRing<N> {
    pub const fn new() -> Self {
        const {
            assert!(N.is_power_of_two(), "capacity must be a power of two");
        }
        Self {
            buf: [const { AtomicU8::new(0) }; N],
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }

    /// Producer side (ISR). Returns false (dropping the byte) when full --
    /// an ISR can't block, and for input a dropped byte under extreme
    /// backlog is the correct failure mode.
    pub fn push(&self, byte: u8) -> bool {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        if head.wrapping_sub(tail) >= N {
            return false;
        }
        self.buf[head % N].store(byte, Ordering::Relaxed);
        self.head.store(head.wrapping_add(1), Ordering::Release);
        true
    }

    /// Consumer side (task context).
    pub fn pop(&self) -> Option<u8> {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        if tail == head {
            return None;
        }
        let byte = self.buf[tail % N].load(Ordering::Relaxed);
        self.tail.store(tail.wrapping_add(1), Ordering::Release);
        Some(byte)
    }
}

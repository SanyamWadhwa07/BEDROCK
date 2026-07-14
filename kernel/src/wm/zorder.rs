//! Z-order bookkeeping: a back-to-front `Vec` of indices into the window
//! table. Raising a window is remove+push (O(n), but n is a handful of
//! windows); hit-testing walks the list top-down (reverse) so the
//! frontmost window wins overlapping clicks -- the standard "last drawn
//! wins" compositor rule, paired with painter's-algorithm draw order
//! (`back_to_front`) on the way out.

use super::window::Window;
use alloc::vec::Vec;

pub struct ZOrder {
    /// Back-to-front: index 0 is bottommost, the last entry is topmost.
    order: Vec<usize>,
}

impl ZOrder {
    pub const fn new() -> Self {
        Self { order: Vec::new() }
    }

    pub fn push_top(&mut self, idx: usize) {
        self.order.push(idx);
    }

    /// Drops `idx` and shifts every index greater than it down by one, to
    /// track a `Vec::remove(idx)` on the parallel window table (stable
    /// order, unlike `swap_remove`, is what makes this reindex correct).
    pub fn remove(&mut self, idx: usize) {
        self.order.retain(|&i| i != idx);
        for i in self.order.iter_mut() {
            if *i > idx {
                *i -= 1;
            }
        }
    }

    pub fn raise(&mut self, idx: usize) {
        if let Some(pos) = self.order.iter().position(|&i| i == idx) {
            self.order.remove(pos);
            self.order.push(idx);
        }
    }

    pub fn top(&self) -> Option<usize> {
        self.order.last().copied()
    }

    /// Back-to-front iteration order for compositing (painter's algorithm).
    pub fn back_to_front(&self) -> impl Iterator<Item = usize> + '_ {
        self.order.iter().copied()
    }

    /// Top-down point hit test: the frontmost window containing the point
    /// wins, even if others behind it also contain it.
    pub fn hit_test(&self, windows: &[Window], x: usize, y: usize) -> Option<usize> {
        for &idx in self.order.iter().rev() {
            if windows[idx].rect.contains(x, y) {
                return Some(idx);
            }
        }
        None
    }
}

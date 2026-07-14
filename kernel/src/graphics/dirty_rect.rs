//! Dirty-rectangle tracking: the classic interval/rectangle-union problem.
//! Rects accumulate as drawing happens; overlapping/touching rects are
//! greedily merged via bounding-box union on insert, and if the list still
//! fragments past a cap, everything collapses into one overall bounding box
//! (the "give up and repaint one big region" fallback real compositors use).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

impl Rect {
    pub const fn new(x: usize, y: usize, w: usize, h: usize) -> Self {
        Self { x, y, w, h }
    }

    pub fn right(&self) -> usize {
        self.x + self.w
    }

    pub fn bottom(&self) -> usize {
        self.y + self.h
    }

    /// Overlapping OR directly adjacent (sharing an edge) — merging adjacent
    /// rects too keeps a moving object's erase+draw pair as one rect.
    pub fn touches(&self, other: &Rect) -> bool {
        self.x <= other.right()
            && other.x <= self.right()
            && self.y <= other.bottom()
            && other.y <= self.bottom()
    }

    pub fn union(&self, other: &Rect) -> Rect {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = self.right().max(other.right());
        let bottom = self.bottom().max(other.bottom());
        Rect::new(x, y, right - x, bottom - y)
    }

    pub fn clamp_to(&self, width: usize, height: usize) -> Rect {
        let x = self.x.min(width);
        let y = self.y.min(height);
        let w = self.w.min(width - x);
        let h = self.h.min(height - y);
        Rect::new(x, y, w, h)
    }
}

pub const MAX_DIRTY_RECTS: usize = 16;

pub struct DirtyTracker {
    rects: heapless_vec::BoundedVec,
}

/// A tiny fixed-capacity vec so dirty tracking itself never allocates
/// (it can be called with the heap lock's owner preempted).
mod heapless_vec {
    use super::{Rect, MAX_DIRTY_RECTS};

    pub struct BoundedVec {
        items: [Rect; MAX_DIRTY_RECTS],
        len: usize,
    }

    impl BoundedVec {
        pub const fn new() -> Self {
            Self {
                items: [Rect::new(0, 0, 0, 0); MAX_DIRTY_RECTS],
                len: 0,
            }
        }

        pub fn len(&self) -> usize {
            self.len
        }

        pub fn as_slice(&self) -> &[Rect] {
            &self.items[..self.len]
        }

        pub fn clear(&mut self) {
            self.len = 0;
        }

        /// Returns false if full.
        pub fn push(&mut self, r: Rect) -> bool {
            if self.len == MAX_DIRTY_RECTS {
                return false;
            }
            self.items[self.len] = r;
            self.len += 1;
            true
        }

        pub fn get_mut(&mut self, i: usize) -> &mut Rect {
            &mut self.items[i]
        }
    }
}

impl DirtyTracker {
    pub const fn new() -> Self {
        Self {
            rects: heapless_vec::BoundedVec::new(),
        }
    }

    pub fn mark(&mut self, rect: Rect) {
        if rect.w == 0 || rect.h == 0 {
            return;
        }
        // Greedy merge: union into the first rect we touch.
        for i in 0..self.rects.len() {
            let existing = *self.rects.get_mut(i);
            if existing.touches(&rect) {
                *self.rects.get_mut(i) = existing.union(&rect);
                return;
            }
        }
        if !self.rects.push(rect) {
            // Fragmentation cap hit: collapse everything into one bounding box.
            let mut bound = rect;
            for r in self.rects.as_slice() {
                bound = bound.union(r);
            }
            self.rects.clear();
            let _ = self.rects.push(bound);
        }
    }

    pub fn take(&mut self) -> ([Rect; MAX_DIRTY_RECTS], usize) {
        let mut out = [Rect::new(0, 0, 0, 0); MAX_DIRTY_RECTS];
        let n = self.rects.len();
        out[..n].copy_from_slice(self.rects.as_slice());
        self.rects.clear();
        (out, n)
    }
}

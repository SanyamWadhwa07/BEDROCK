//! Dirty-rectangle tracking: the classic interval/rectangle-union problem.
//! Rects accumulate as drawing happens; overlapping/touching rects are
//! greedily merged via bounding-box union on insert, and if the list still
//! fragments past a cap, everything collapses into one overall bounding box
//! (the "give up and repaint one big region" fallback real compositors use).
//!
//! Deliberately free of both `alloc` and any framebuffer/MMIO types: the
//! tracker is called from contexts where the heap lock's owner may be
//! preempted, and keeping it pure is also what makes it host-testable under
//! ordinary `cargo test` instead of only inside QEMU.

#![cfg_attr(not(test), no_std)]

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

    pub fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }

    pub fn area(&self) -> usize {
        self.w * self.h
    }

    /// Overlapping OR directly adjacent (sharing an edge) -- merging adjacent
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

    pub fn contains(&self, x: usize, y: usize) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    /// Clips to a `width` x `height` screen. A rect entirely off-screen
    /// clamps to zero width/height rather than underflowing.
    pub fn clamp_to(&self, width: usize, height: usize) -> Rect {
        let x = self.x.min(width);
        let y = self.y.min(height);
        let w = self.w.min(width - x);
        let h = self.h.min(height - y);
        Rect::new(x, y, w, h)
    }
}

pub const MAX_DIRTY_RECTS: usize = 16;

/// A tiny fixed-capacity vec so dirty tracking itself never allocates.
struct BoundedVec {
    items: [Rect; MAX_DIRTY_RECTS],
    len: usize,
}

impl BoundedVec {
    const fn new() -> Self {
        Self {
            items: [Rect::new(0, 0, 0, 0); MAX_DIRTY_RECTS],
            len: 0,
        }
    }

    fn len(&self) -> usize {
        self.len
    }

    fn as_slice(&self) -> &[Rect] {
        &self.items[..self.len]
    }

    fn clear(&mut self) {
        self.len = 0;
    }

    /// Returns false if full.
    fn push(&mut self, r: Rect) -> bool {
        if self.len == MAX_DIRTY_RECTS {
            return false;
        }
        self.items[self.len] = r;
        self.len += 1;
        true
    }

    fn get_mut(&mut self, i: usize) -> &mut Rect {
        &mut self.items[i]
    }
}

pub struct DirtyTracker {
    rects: BoundedVec,
}

impl Default for DirtyTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl DirtyTracker {
    pub const fn new() -> Self {
        Self {
            rects: BoundedVec::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.rects.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rects.len() == 0
    }

    pub fn as_slice(&self) -> &[Rect] {
        self.rects.as_slice()
    }

    pub fn mark(&mut self, rect: Rect) {
        if rect.is_empty() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn union_covers_both_inputs() {
        let a = Rect::new(10, 10, 5, 5);
        let b = Rect::new(100, 50, 20, 20);
        let u = a.union(&b);
        assert_eq!(u, Rect::new(10, 10, 110, 60));
        // A union must contain every corner of both inputs.
        for r in [a, b] {
            assert!(u.contains(r.x, r.y));
            assert!(u.contains(r.right() - 1, r.bottom() - 1));
        }
    }

    #[test]
    fn union_is_commutative_and_idempotent() {
        let a = Rect::new(3, 7, 11, 13);
        let b = Rect::new(20, 1, 4, 40);
        assert_eq!(a.union(&b), b.union(&a));
        assert_eq!(a.union(&a), a);
    }

    #[test]
    fn touches_includes_shared_edges_but_not_gaps() {
        let a = Rect::new(0, 0, 10, 10);
        assert!(a.touches(&Rect::new(5, 5, 10, 10)), "overlapping");
        assert!(a.touches(&Rect::new(10, 0, 10, 10)), "edge-adjacent");
        assert!(!a.touches(&Rect::new(11, 0, 10, 10)), "one-pixel gap");
    }

    #[test]
    fn contains_is_half_open_on_the_far_edge() {
        let r = Rect::new(2, 3, 4, 5);
        assert!(r.contains(2, 3));
        assert!(r.contains(5, 7));
        assert!(!r.contains(6, 7), "right edge is exclusive");
        assert!(!r.contains(5, 8), "bottom edge is exclusive");
        assert!(!r.contains(1, 3));
    }

    #[test]
    fn clamp_never_underflows_for_offscreen_rects() {
        // Entirely past the right/bottom edge: must clamp to empty, not wrap.
        let r = Rect::new(5000, 5000, 100, 100).clamp_to(1280, 720);
        assert!(r.is_empty(), "got {r:?}");
        // Straddling the edge: keeps only the on-screen part.
        let r = Rect::new(1200, 700, 200, 200).clamp_to(1280, 720);
        assert_eq!(r, Rect::new(1200, 700, 80, 20));
        assert!(r.right() <= 1280 && r.bottom() <= 720);
    }

    #[test]
    fn empty_marks_are_ignored() {
        let mut t = DirtyTracker::new();
        t.mark(Rect::new(0, 0, 0, 10));
        t.mark(Rect::new(0, 0, 10, 0));
        assert!(t.is_empty());
    }

    #[test]
    fn overlapping_marks_merge_into_one_rect() {
        let mut t = DirtyTracker::new();
        t.mark(Rect::new(0, 0, 10, 10));
        t.mark(Rect::new(5, 5, 10, 10));
        let (rects, n) = t.take();
        assert_eq!(n, 1);
        assert_eq!(rects[0], Rect::new(0, 0, 15, 15));
    }

    #[test]
    fn disjoint_marks_stay_separate() {
        let mut t = DirtyTracker::new();
        t.mark(Rect::new(0, 0, 10, 10));
        t.mark(Rect::new(500, 500, 10, 10));
        let (_, n) = t.take();
        assert_eq!(n, 2, "far-apart rects must not be merged");
    }

    #[test]
    fn take_resets_the_tracker() {
        let mut t = DirtyTracker::new();
        t.mark(Rect::new(0, 0, 10, 10));
        let (_, n) = t.take();
        assert_eq!(n, 1);
        let (_, n) = t.take();
        assert_eq!(n, 0, "take must drain");
    }

    #[test]
    fn overflow_collapses_to_a_single_bounding_box() {
        let mut t = DirtyTracker::new();
        // MAX_DIRTY_RECTS + 1 mutually non-touching rects: spaced 100 apart
        // so no greedy merge can absorb any of them.
        for i in 0..=MAX_DIRTY_RECTS {
            t.mark(Rect::new(i * 100, i * 100, 10, 10));
        }
        let (rects, n) = t.take();
        assert_eq!(n, 1, "past the cap everything must collapse");
        // The collapsed box must still cover every rect that was marked.
        let bound = rects[0];
        for i in 0..=MAX_DIRTY_RECTS {
            assert!(
                bound.contains(i * 100, i * 100),
                "collapsed box lost rect {i}"
            );
        }
    }

    /// The invariant the compositor's correctness actually rests on: whatever
    /// the tracker hands back must cover every pixel that was ever marked
    /// dirty. Under-coverage means visible stale pixels on screen.
    #[test]
    fn output_always_covers_every_marked_pixel() {
        let mut rng: u64 = 0xDEAD_BEEF_CAFE_F00D;
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };

        for trial in 0..2_000 {
            let mut t = DirtyTracker::new();
            let mut marked = [Rect::new(0, 0, 0, 0); 40];
            let count = (next() % 40) as usize + 1;
            for slot in marked.iter_mut().take(count) {
                let r = Rect::new(
                    (next() % 1280) as usize,
                    (next() % 720) as usize,
                    (next() % 200) as usize + 1,
                    (next() % 200) as usize + 1,
                );
                *slot = r;
                t.mark(r);
            }

            let (out, n) = t.take();
            assert!(n <= MAX_DIRTY_RECTS, "cap exceeded on trial {trial}");
            for m in marked.iter().take(count) {
                // Every corner of every marked rect must land inside some
                // output rect (corners suffice: outputs are axis-aligned
                // boxes, so covering both corners covers the whole rect).
                for (px, py) in [(m.x, m.y), (m.right() - 1, m.bottom() - 1)] {
                    assert!(
                        out[..n].iter().any(|o| o.contains(px, py)),
                        "trial {trial}: pixel ({px},{py}) of {m:?} left uncovered by {:?}",
                        &out[..n]
                    );
                }
            }
        }
    }
}

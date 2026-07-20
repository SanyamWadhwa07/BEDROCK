//! A fixed-capacity LRU cache built from the two classic pieces the
//! interview answer names: a **hash table** (open addressing, linear
//! probing, backward-shift deletion) mapping keys to slots, and an
//! **arena-backed doubly-linked list** ordering those slots from
//! most-recently to least-recently used. `get`, `put`, and eviction are
//! all O(1); capacity is fixed at construction so the kernel's memory use
//! is bounded and predictable.
//!
//! `no_std` + `alloc` so the same code runs inside the Bedrock kernel and
//! under ordinary `cargo test` on the host.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::vec::Vec;

const NIL: usize = usize::MAX;

struct Entry<V> {
    key: u32,
    val: V,
    prev: usize,
    next: usize,
}

/// Open-addressing hash table from `u32` keys to arena indices. Fixed
/// power-of-two bucket count (4x capacity keeps the load factor <= 0.25),
/// linear probing, and backward-shift deletion so no tombstones accumulate.
struct ProbeMap {
    buckets: Vec<Option<(u32, usize)>>,
    mask: usize,
}

impl ProbeMap {
    fn new(capacity: usize) -> Self {
        let buckets = (capacity * 4).next_power_of_two().max(8);
        Self {
            buckets: (0..buckets).map(|_| None).collect(),
            mask: buckets - 1,
        }
    }

    /// Fibonacci hashing: multiply by 2^32/phi and use the high bits.
    fn bucket_of(&self, key: u32) -> usize {
        let h = key.wrapping_mul(0x9E37_79B9);
        (h >> 16) as usize & self.mask
    }

    fn get(&self, key: u32) -> Option<usize> {
        let mut b = self.bucket_of(key);
        loop {
            match self.buckets[b] {
                Some((k, idx)) if k == key => return Some(idx),
                Some(_) => b = (b + 1) & self.mask,
                None => return None,
            }
        }
    }

    fn insert(&mut self, key: u32, idx: usize) {
        let mut b = self.bucket_of(key);
        loop {
            match self.buckets[b] {
                Some((k, _)) if k == key => {
                    self.buckets[b] = Some((key, idx));
                    return;
                }
                Some(_) => b = (b + 1) & self.mask,
                None => {
                    self.buckets[b] = Some((key, idx));
                    return;
                }
            }
        }
    }

    /// Backward-shift deletion: after emptying the slot, walk the probe
    /// chain and move back every entry whose home bucket the hole now
    /// blocks -- keeps lookups tombstone-free forever.
    fn remove(&mut self, key: u32) {
        let mut b = self.bucket_of(key);
        loop {
            match self.buckets[b] {
                Some((k, _)) if k == key => break,
                Some(_) => b = (b + 1) & self.mask,
                None => return, // not present
            }
        }
        self.buckets[b] = None;
        let mut hole = b;
        let mut probe = (b + 1) & self.mask;
        while let Some((k, idx)) = self.buckets[probe] {
            let home = self.bucket_of(k);
            // Shift back iff the hole sits within [home, probe) cyclically.
            let dist_home = probe.wrapping_sub(home) & self.mask;
            let dist_hole = probe.wrapping_sub(hole) & self.mask;
            if dist_hole <= dist_home {
                self.buckets[hole] = Some((k, idx));
                self.buckets[probe] = None;
                hole = probe;
            }
            probe = (probe + 1) & self.mask;
        }
    }

    fn clear(&mut self) {
        for b in self.buckets.iter_mut() {
            *b = None;
        }
    }
}

pub struct LruCache<V> {
    cap: usize,
    entries: Vec<Entry<V>>,
    head: usize, // most-recently-used
    tail: usize, // least-recently-used
    map: ProbeMap,
}

impl<V> LruCache<V> {
    /// # Panics
    /// If `capacity` is zero.
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "LruCache capacity must be non-zero");
        Self {
            cap: capacity,
            entries: Vec::with_capacity(capacity),
            head: NIL,
            tail: NIL,
            map: ProbeMap::new(capacity),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.cap
    }

    fn unlink(&mut self, idx: usize) {
        let (prev, next) = (self.entries[idx].prev, self.entries[idx].next);
        if prev != NIL {
            self.entries[prev].next = next;
        } else {
            self.head = next;
        }
        if next != NIL {
            self.entries[next].prev = prev;
        } else {
            self.tail = prev;
        }
    }

    fn push_front(&mut self, idx: usize) {
        self.entries[idx].prev = NIL;
        self.entries[idx].next = self.head;
        if self.head != NIL {
            self.entries[self.head].prev = idx;
        }
        self.head = idx;
        if self.tail == NIL {
            self.tail = idx;
        }
    }

    /// Looks the key up and, on a hit, marks it most-recently-used.
    pub fn get(&mut self, key: u32) -> Option<&V> {
        let idx = self.map.get(key)?;
        if idx != self.head {
            self.unlink(idx);
            self.push_front(idx);
        }
        Some(&self.entries[idx].val)
    }

    /// Inserts or updates. On insert at capacity, evicts the
    /// least-recently-used entry and returns `Some((key, value))` for it.
    pub fn put(&mut self, key: u32, val: V) -> Option<(u32, V)> {
        if let Some(idx) = self.map.get(key) {
            self.entries[idx].val = val;
            if idx != self.head {
                self.unlink(idx);
                self.push_front(idx);
            }
            return None;
        }

        if self.entries.len() < self.cap {
            let idx = self.entries.len();
            self.entries.push(Entry {
                key,
                val,
                prev: NIL,
                next: NIL,
            });
            self.map.insert(key, idx);
            self.push_front(idx);
            return None;
        }

        // Full: reuse the LRU tail's arena slot in place.
        let idx = self.tail;
        self.unlink(idx);
        let old_key = self.entries[idx].key;
        self.map.remove(old_key);
        let old_val = core::mem::replace(&mut self.entries[idx].val, val);
        self.entries[idx].key = key;
        self.map.insert(key, idx);
        self.push_front(idx);
        Some((old_key, old_val))
    }

    pub fn contains(&self, key: u32) -> bool {
        self.map.get(key).is_some()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.map.clear();
        self.head = NIL;
        self.tail = NIL;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_get() {
        let mut c = LruCache::new(2);
        assert!(c.put(1, "a").is_none());
        assert!(c.put(2, "b").is_none());
        assert_eq!(c.get(1), Some(&"a"));
        assert_eq!(c.get(2), Some(&"b"));
        assert_eq!(c.get(3), None);
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn evicts_least_recently_used() {
        let mut c = LruCache::new(2);
        c.put(1, "a");
        c.put(2, "b");
        c.get(1); // 2 is now LRU
        assert_eq!(c.put(3, "c"), Some((2, "b")));
        assert_eq!(c.get(2), None);
        assert_eq!(c.get(1), Some(&"a"));
        assert_eq!(c.get(3), Some(&"c"));
    }

    #[test]
    fn update_refreshes_recency_without_evicting() {
        let mut c = LruCache::new(2);
        c.put(1, "a");
        c.put(2, "b");
        assert!(c.put(1, "a2").is_none()); // update, not insert
        assert_eq!(c.put(3, "c"), Some((2, "b"))); // 2 was LRU after 1's refresh
        assert_eq!(c.get(1), Some(&"a2"));
    }

    #[test]
    fn clear_empties_everything() {
        let mut c = LruCache::new(4);
        for k in 0..4 {
            c.put(k, k);
        }
        c.clear();
        assert!(c.is_empty());
        for k in 0..4 {
            assert_eq!(c.get(k), None);
        }
        // Reusable after clear.
        c.put(9, 9);
        assert_eq!(c.get(9), Some(&9));
    }

    #[test]
    fn single_entry_capacity() {
        let mut c = LruCache::new(1);
        assert!(c.put(1, 10).is_none());
        assert_eq!(c.put(2, 20), Some((1, 10)));
        assert_eq!(c.get(1), None);
        assert_eq!(c.get(2), Some(&20));
    }

    /// Stress the cache against a naive reference model with a
    /// deterministic pseudo-random op stream -- specifically to exercise
    /// the probe map's backward-shift deletion under heavy key churn.
    #[test]
    fn matches_reference_model_under_churn() {
        struct Model {
            cap: usize,
            items: Vec<(u32, u64)>, // front = MRU
        }
        impl Model {
            fn get(&mut self, key: u32) -> Option<u64> {
                let pos = self.items.iter().position(|&(k, _)| k == key)?;
                let item = self.items.remove(pos);
                self.items.insert(0, item);
                Some(item.1)
            }
            fn put(&mut self, key: u32, val: u64) {
                if let Some(pos) = self.items.iter().position(|&(k, _)| k == key) {
                    self.items.remove(pos);
                } else if self.items.len() == self.cap {
                    self.items.pop();
                }
                self.items.insert(0, (key, val));
            }
        }

        let mut cache = LruCache::new(16);
        let mut model = Model {
            cap: 16,
            items: Vec::new(),
        };
        let mut rng: u64 = 0x1234_5678_9abc_def0;
        let mut next = move || {
            // xorshift64
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };

        for i in 0..100_000u64 {
            let r = next();
            let key = (r % 48) as u32; // 3x capacity -> constant eviction
            if r % 3 == 0 {
                assert_eq!(
                    cache.get(key).copied(),
                    model.get(key),
                    "get({key}) diverged at op {i}"
                );
            } else {
                cache.put(key, i);
                model.put(key, i);
                assert_eq!(cache.len(), model.items.len());
            }
        }
    }
}

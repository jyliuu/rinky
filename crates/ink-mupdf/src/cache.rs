//! A least-recently-used cache with a byte cap (rendered pages, page text).

use std::collections::HashMap;
use std::hash::Hash;

/// LRU map whose entries carry a size in bytes; the total never exceeds the cap.
#[derive(Debug)]
pub struct ByteCache<K, V> {
    cap: usize,
    used: usize,
    tick: u64,
    map: HashMap<K, (V, usize, u64)>,
    evictions: u64,
}

impl<K: Hash + Eq + Clone, V> ByteCache<K, V> {
    /// An empty cache holding at most `cap` bytes.
    pub fn new(cap: usize) -> Self {
        ByteCache { cap, used: 0, tick: 0, map: HashMap::new(), evictions: 0 }
    }

    /// The cap in bytes.
    pub fn cap(&self) -> usize {
        self.cap
    }

    /// Bytes held now.
    pub fn used(&self) -> usize {
        self.used
    }

    /// True when empty.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Entries evicted so far (for tests and logs).
    pub fn evictions(&self) -> u64 {
        self.evictions
    }

    /// True when `k` is cached (does not count as a use).
    pub fn contains(&self, k: &K) -> bool {
        self.map.contains_key(k)
    }

    /// The entry for `k`, marked as most recently used.
    pub fn get(&mut self, k: &K) -> Option<&V> {
        self.tick += 1;
        let tick = self.tick;
        self.map.get_mut(k).map(|e| {
            e.2 = tick;
            &e.0
        })
    }

    /// Stores `v` (of `bytes` bytes), evicting the least recently used entries until it
    /// fits. A value larger than the whole cap is not stored. Returns true when stored.
    pub fn insert(&mut self, k: K, v: V, bytes: usize) -> bool {
        if let Some(old) = self.map.remove(&k) {
            self.used -= old.1;
        }
        if bytes > self.cap {
            return false;
        }
        self.evict_to(self.cap - bytes);
        self.tick += 1;
        self.used += bytes;
        self.map.insert(k, (v, bytes, self.tick));
        true
    }

    /// Evicts least recently used entries until at most `limit` bytes are held.
    fn evict_to(&mut self, limit: usize) {
        while self.used > limit {
            let Some(oldest) = self.map.iter().min_by_key(|(_, e)| e.2).map(|(k, _)| k.clone()) else { break };
            self.used -= self.map.remove(&oldest).map_or(0, |e| e.1);
            self.evictions += 1;
        }
    }

    /// Changes the cap, evicting as needed.
    pub fn set_cap(&mut self, cap: usize) {
        self.cap = cap;
        self.evict_to(cap);
    }

    /// Drops everything.
    pub fn clear(&mut self) {
        self.map.clear();
        self.used = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evicts_least_recent_and_respects_cap() {
        let mut c = ByteCache::new(100);
        assert!(c.insert(1, "a", 40));
        assert!(c.insert(2, "b", 40));
        assert_eq!(c.get(&1), Some(&"a")); // 1 is now newer than 2
        assert!(c.insert(3, "c", 40)); // evicts 2
        assert!(c.contains(&1) && !c.contains(&2) && c.contains(&3));
        assert_eq!(c.used(), 80);
        assert_eq!(c.evictions(), 1);
        assert!(!c.insert(4, "too big", 101));
        assert!(c.used() <= c.cap());
        c.insert(1, "a2", 10); // replacing frees the old size
        assert_eq!(c.used(), 50);
        c.set_cap(20);
        assert!(c.used() <= 20 && c.contains(&1));
        c.clear();
        assert!(c.is_empty() && c.used() == 0);
    }
}

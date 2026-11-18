//! Access-order cache for numeric archive offsets and cluster indices.
use std::collections::HashMap;
use std::hash::Hash;

#[derive(Debug)]
struct Node<K, V> {
    value: V,
    newer: Option<K>,
    older: Option<K>,
}

#[derive(Debug)]
pub(super) struct Cache<K, V> {
    entries: HashMap<K, Node<K, V>>,
    newest: Option<K>,
    oldest: Option<K>,
}

impl<K: Copy + Eq + Hash, V> Cache<K, V> {
    pub(super) fn new() -> Self {
        Self {
            entries: HashMap::new(),
            newest: None,
            oldest: None,
        }
    }

    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    fn unlink(&mut self, newer: Option<K>, older: Option<K>) {
        match newer {
            Some(k) => self.entries.get_mut(&k).unwrap().older = older,
            None => self.newest = older,
        }
        match older {
            Some(k) => self.entries.get_mut(&k).unwrap().newer = newer,
            None => self.oldest = newer,
        }
    }

    fn link_newest(&mut self, key: K) {
        if let Some(prev) = self.newest {
            self.entries.get_mut(&prev).unwrap().newer = Some(key);
        } else {
            self.oldest = Some(key);
        }
        self.newest = Some(key);
    }

    pub(super) fn get(&mut self, key: &K) -> Option<&V> {
        if self.newest != Some(*key) {
            let node = self.entries.get_mut(key)?;
            let (newer, older) = (node.newer.take(), node.older);
            node.older = self.newest;
            self.unlink(newer, older);
            self.link_newest(*key);
        }
        self.entries.get(key).map(|n| &n.value)
    }

    pub(super) fn put(&mut self, key: K, value: V) -> Option<V> {
        let old = self.entries.remove(&key).map(|node| {
            self.unlink(node.newer, node.older);
            node.value
        });
        let older = self.newest;
        self.link_newest(key);
        self.entries.insert(
            key,
            Node {
                value,
                newer: None,
                older,
            },
        );
        old
    }

    pub(super) fn pop_lru(&mut self) -> Option<(K, V)> {
        let key = self.oldest?;
        let node = self.entries.remove(&key).unwrap();
        self.unlink(node.newer, node.older);
        Some((key, node.value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[test]
    fn promotion_replacement_and_eviction_match_access_order() {
        let mut cache = Cache::new();
        let mut ordered = VecDeque::new();
        let mut state = 7u32;
        for value in 0..12000 {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            let key = (state >> 16) % 19;
            let index = ordered.iter().position(|&(k, _)| k == key);
            match state % 5 {
                0 => assert_eq!(cache.pop_lru(), ordered.pop_back()),
                1 | 2 => {
                    let hit = index.and_then(|i| ordered.remove(i));
                    assert_eq!(cache.get(&key).copied(), hit.map(|(_, v)| v));
                    if let Some(row) = hit {
                        ordered.push_front(row);
                    }
                }
                _ => {
                    let old = index.and_then(|i| ordered.remove(i));
                    ordered.push_front((key, value));
                    assert_eq!(cache.put(key, value), old.map(|(_, v)| v));
                }
            }
            assert_eq!(cache.len(), ordered.len());
        }
    }
}

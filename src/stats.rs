//! Aggregation of per-key observations into namespace -> type statistics.

use serde::Serialize;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

/// Per-type accumulator.
#[derive(Debug, Default, Clone, Serialize)]
pub struct TypeStat {
    pub count: u64,
    /// Total memory in bytes (only populated when `--memory` is enabled).
    pub total_bytes: u64,
    /// Number of keys we actually measured memory for (may be < count if some
    /// keys vanished between SCAN and MEMORY USAGE).
    pub measured: u64,
    /// Breakdown by Redis data type (only when `--types` enabled).
    pub data_types: HashMap<String, u64>,
    /// Keys with no expiry set (only when `--ttl` enabled).
    pub persistent: u64,
    /// Keys with an expiry set.
    pub expiring: u64,
    /// Memory held by persistent (no-TTL) keys — i.e. space that won't be freed
    /// by expiry. Only populated when both memory and TTL are collected.
    pub persistent_bytes: u64,
}

impl TypeStat {
    pub fn avg_bytes(&self) -> u64 {
        self.total_bytes.checked_div(self.measured).unwrap_or(0)
    }
}

/// One observation gathered for a single key.
#[derive(Debug, Default)]
pub struct KeyObservation {
    pub bytes: Option<u64>,
    pub data_type: Option<String>,
    pub ttl: Option<i64>, // -1 = no expiry, -2 = missing, >=0 seconds
}

/// A single key retained in the biggest-keys leaderboard.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct BigKey {
    pub key: String,
    pub bytes: u64,
    pub data_type: Option<String>,
    pub ttl: Option<i64>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Stats {
    /// namespace -> (type -> stat)
    pub namespaces: HashMap<String, HashMap<String, TypeStat>>,
    pub total_keys: u64,

    /// Bounded leaderboard of the largest keys by memory. Kept as a min-heap
    /// (smallest-on-top) capped at `biggest_cap` so memory stays constant no
    /// matter how huge the DB is. Empty unless memory measurement is enabled.
    #[serde(skip)]
    biggest_heap: BinaryHeap<Reverse<HeapEntry>>,
    #[serde(skip)]
    biggest_cap: usize,
}

/// Heap entry ordered purely by byte size (then key for deterministic ties).
#[derive(Debug, Clone, PartialEq, Eq)]
struct HeapEntry {
    bytes: u64,
    key: String,
    data_type: Option<String>,
    ttl: Option<i64>,
}

impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.bytes
            .cmp(&other.bytes)
            .then_with(|| self.key.cmp(&other.key))
    }
}

impl Stats {
    /// Create a Stats that retains the `cap` largest keys (0 disables tracking).
    pub fn with_biggest_cap(cap: usize) -> Self {
        Stats {
            biggest_cap: cap,
            ..Default::default()
        }
    }

    pub fn record(&mut self, key: &str, namespace: &str, key_type: &str, obs: &KeyObservation) {
        self.total_keys += 1;
        {
            let ns = self.namespaces.entry(namespace.to_string()).or_default();
            let stat = ns.entry(key_type.to_string()).or_default();
            stat.count += 1;

            if let Some(bytes) = obs.bytes {
                stat.total_bytes += bytes;
                stat.measured += 1;
            }
            if let Some(dt) = &obs.data_type {
                *stat.data_types.entry(dt.clone()).or_insert(0) += 1;
            }
            if let Some(ttl) = obs.ttl {
                if ttl == -1 {
                    stat.persistent += 1;
                    // Track the memory of no-TTL keys: space expiry won't free.
                    if let Some(bytes) = obs.bytes {
                        stat.persistent_bytes += bytes;
                    }
                } else if ttl >= 0 {
                    stat.expiring += 1;
                }
                // ttl == -2 means the key disappeared; don't count it.
            }
        }

        // Done with the per-type borrow; now offer to the global leaderboard.
        if let Some(bytes) = obs.bytes {
            self.record_big_key(key, bytes, &obs.data_type, obs.ttl);
        }
    }

    /// Offer a measured key to the bounded biggest-keys leaderboard.
    fn record_big_key(
        &mut self,
        key: &str,
        bytes: u64,
        data_type: &Option<String>,
        ttl: Option<i64>,
    ) {
        if self.biggest_cap == 0 {
            return;
        }
        // Skip work once full if this key can't beat the current smallest.
        if self.biggest_heap.len() >= self.biggest_cap {
            if let Some(Reverse(smallest)) = self.biggest_heap.peek() {
                if bytes <= smallest.bytes {
                    return;
                }
            }
            self.biggest_heap.pop();
        }
        self.biggest_heap.push(Reverse(HeapEntry {
            bytes,
            key: key.to_string(),
            data_type: data_type.clone(),
            ttl,
        }));
    }

    /// The retained biggest keys, sorted largest-first.
    pub fn biggest(&self) -> Vec<BigKey> {
        let mut v: Vec<BigKey> = self
            .biggest_heap
            .iter()
            .map(|Reverse(e)| BigKey {
                key: e.key.clone(),
                bytes: e.bytes,
                data_type: e.data_type.clone(),
                ttl: e.ttl,
            })
            .collect();
        v.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.key.cmp(&b.key)));
        v
    }

    /// Total key count for a namespace.
    pub fn namespace_count(&self, ns: &str) -> u64 {
        self.namespaces
            .get(ns)
            .map(|m| m.values().map(|s| s.count).sum())
            .unwrap_or(0)
    }

    /// Total memory for a namespace.
    pub fn namespace_bytes(&self, ns: &str) -> u64 {
        self.namespaces
            .get(ns)
            .map(|m| m.values().map(|s| s.total_bytes).sum())
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(bytes: u64, ttl: i64) -> KeyObservation {
        KeyObservation {
            bytes: Some(bytes),
            data_type: Some("string".into()),
            ttl: Some(ttl),
        }
    }

    #[test]
    fn records_and_aggregates() {
        let mut s = Stats::default();
        s.record("ns1:a", "ns1", "product", &obs(100, -1));
        s.record("ns1:b", "ns1", "product", &obs(300, 60));
        assert_eq!(s.total_keys, 2);
        let stat = &s.namespaces["ns1"]["product"];
        assert_eq!(stat.count, 2);
        assert_eq!(stat.total_bytes, 400);
        assert_eq!(stat.avg_bytes(), 200);
        assert_eq!(stat.persistent, 1);
        assert_eq!(stat.expiring, 1);
        // Only the no-TTL key (100 bytes) counts toward unevictable memory.
        assert_eq!(stat.persistent_bytes, 100);
        assert_eq!(stat.data_types["string"], 2);
        assert_eq!(s.namespace_count("ns1"), 2);
    }

    #[test]
    fn biggest_keys_are_bounded_and_sorted() {
        let mut s = Stats::with_biggest_cap(3);
        for (i, bytes) in [50u64, 500, 10, 999, 250, 700].iter().enumerate() {
            s.record(&format!("k{i}"), "ns", "t", &obs(*bytes, -1));
        }
        let big = s.biggest();
        // Only the cap is retained, largest first.
        assert_eq!(big.len(), 3);
        assert_eq!(big[0].bytes, 999);
        assert_eq!(big[1].bytes, 700);
        assert_eq!(big[2].bytes, 500);
    }

    #[test]
    fn biggest_disabled_when_cap_zero() {
        let mut s = Stats::default(); // cap 0
        s.record("k", "ns", "t", &obs(123, -1));
        assert!(s.biggest().is_empty());
    }
}

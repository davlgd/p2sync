use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// A short peer identifier (first 8 bytes of the PeerId hash).
pub type PeerId = [u8; 8];

/// Vector clock for tracking causal ordering of file modifications.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct VectorClock {
    #[serde(with = "peer_id_map")]
    pub counters: BTreeMap<PeerId, u64>,
}

mod peer_id_map {
    use super::*;
    use serde::de::{self, MapAccess, Visitor};
    use serde::ser::SerializeMap;
    use std::fmt;

    pub fn serialize<S>(map: &BTreeMap<PeerId, u64>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut m = serializer.serialize_map(Some(map.len()))?;
        for (k, v) in map {
            let hex = crate::util::peer_id_hex(k);
            m.serialize_entry(&hex, v)?;
        }
        m.end()
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<BTreeMap<PeerId, u64>, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct PeerIdMapVisitor;

        impl<'de> Visitor<'de> for PeerIdMapVisitor {
            type Value = BTreeMap<PeerId, u64>;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a map with hex-encoded peer ID keys")
            }

            fn visit_map<M>(self, mut access: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut map = BTreeMap::new();
                while let Some((key, value)) = access.next_entry::<String, u64>()? {
                    if key.len() % 2 != 0 || !key.is_ascii() {
                        return Err(de::Error::custom("invalid hex peer ID key"));
                    }
                    let bytes: Vec<u8> = (0..key.len())
                        .step_by(2)
                        .map(|i| u8::from_str_radix(&key[i..i + 2], 16))
                        .collect::<Result<_, _>>()
                        .map_err(de::Error::custom)?;
                    if bytes.len() != 8 {
                        return Err(de::Error::custom("peer ID must be 8 bytes"));
                    }
                    let mut peer_id = [0u8; 8];
                    peer_id.copy_from_slice(&bytes);
                    map.insert(peer_id, value);
                }
                Ok(map)
            }
        }

        deserializer.deserialize_map(PeerIdMapVisitor)
    }
}

/// Causal ordering between two vector clocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ordering {
    /// First happened before second.
    Before,
    /// First happened after second.
    After,
    /// Concurrent modifications (conflict).
    Concurrent,
    /// Identical clocks.
    Equal,
}

impl VectorClock {
    pub fn new() -> Self {
        Self::default()
    }

    /// Increment the counter for a peer, marking a local modification.
    pub fn tick(&mut self, peer: PeerId) {
        let counter = self.counters.entry(peer).or_insert(0);
        *counter += 1;
    }

    /// Merge another clock into this one (take component-wise max).
    pub fn merge(&mut self, other: &VectorClock) {
        for (&peer, &count) in &other.counters {
            let entry = self.counters.entry(peer).or_insert(0);
            *entry = (*entry).max(count);
        }
    }

    /// Compare two vector clocks for causal ordering.
    pub fn compare(&self, other: &VectorClock) -> Ordering {
        let all_keys: std::collections::BTreeSet<&PeerId> =
            self.counters.keys().chain(other.counters.keys()).collect();

        let mut has_less = false;
        let mut has_greater = false;

        for key in all_keys {
            let a = self.counters.get(key).copied().unwrap_or(0);
            let b = other.counters.get(key).copied().unwrap_or(0);

            if a < b {
                has_less = true;
            }
            if a > b {
                has_greater = true;
            }

            if has_less && has_greater {
                return Ordering::Concurrent;
            }
        }

        match (has_less, has_greater) {
            (false, false) => Ordering::Equal,
            (true, false) => Ordering::Before,
            (false, true) => Ordering::After,
            (true, true) => Ordering::Concurrent,
        }
    }
}

/// Tombstone marking a deleted file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tombstone {
    pub path: String,
    pub deleted_by: PeerId,
    pub clock: VectorClock,
    pub timestamp: u64,
}

impl Tombstone {
    pub fn new(path: String, deleted_by: PeerId, clock: VectorClock) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            path,
            deleted_by,
            clock,
            timestamp,
        }
    }

    /// Check if this tombstone has expired given a TTL in seconds.
    pub fn is_expired(&self, ttl_secs: u64) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        now.saturating_sub(self.timestamp) > ttl_secs
    }
}

/// Conflict resolution outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// No conflict: apply the incoming change.
    Accept,
    /// No conflict: keep local, reject incoming.
    Reject,
    /// Conflict: keep both, rename the loser.
    Conflict {
        /// The peer ID to use in the conflict filename suffix.
        loser_peer: PeerId,
    },
}

/// Decide how to resolve a file modification from a remote peer.
pub fn resolve(
    local_clock: &VectorClock,
    remote_clock: &VectorClock,
    local_timestamp: u64,
    remote_timestamp: u64,
    remote_peer: PeerId,
    local_peer: PeerId,
) -> Resolution {
    match local_clock.compare(remote_clock) {
        Ordering::Before => Resolution::Accept,
        Ordering::After => Resolution::Reject,
        Ordering::Equal => Resolution::Reject,
        Ordering::Concurrent => {
            // Last-writer-wins by timestamp, tie-break by peer ID.
            if remote_timestamp > local_timestamp
                || (remote_timestamp == local_timestamp && remote_peer > local_peer)
            {
                Resolution::Conflict {
                    loser_peer: local_peer,
                }
            } else {
                Resolution::Conflict {
                    loser_peer: remote_peer,
                }
            }
        }
    }
}

/// Format a conflict backup filename.
pub fn conflict_filename(original: &str, peer: &PeerId) -> String {
    let peer_hex = crate::util::peer_id_hex(peer);
    let short = &peer_hex[..8.min(peer_hex.len())];

    if let Some((base, ext)) = original.rsplit_once('.') {
        format!("{base}.conflict.{short}.{ext}")
    } else {
        format!("{original}.conflict.{short}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(id: u8) -> PeerId {
        let mut p = [0u8; 8];
        p[0] = id;
        p
    }

    #[test]
    fn new_clocks_are_equal() {
        let a = VectorClock::new();
        let b = VectorClock::new();
        assert_eq!(a.compare(&b), Ordering::Equal);
    }

    #[test]
    fn tick_makes_clock_advance() {
        let mut a = VectorClock::new();
        let b = VectorClock::new();
        a.tick(peer(1));
        assert_eq!(a.compare(&b), Ordering::After);
        assert_eq!(b.compare(&a), Ordering::Before);
    }

    #[test]
    fn concurrent_modifications() {
        let mut a = VectorClock::new();
        let mut b = VectorClock::new();
        a.tick(peer(1));
        b.tick(peer(2));
        assert_eq!(a.compare(&b), Ordering::Concurrent);
    }

    #[test]
    fn merge_takes_max() {
        let mut a = VectorClock::new();
        let mut b = VectorClock::new();
        a.tick(peer(1));
        a.tick(peer(1));
        b.tick(peer(1));
        b.tick(peer(2));

        a.merge(&b);
        assert_eq!(a.counters[&peer(1)], 2);
        assert_eq!(a.counters[&peer(2)], 1);
    }

    #[test]
    fn merge_then_compare() {
        let mut a = VectorClock::new();
        let mut b = VectorClock::new();
        a.tick(peer(1));
        b.tick(peer(2));

        let mut merged = a.clone();
        merged.merge(&b);
        merged.tick(peer(1));

        assert_eq!(merged.compare(&a), Ordering::After);
        assert_eq!(merged.compare(&b), Ordering::After);
    }

    #[test]
    fn resolve_accepts_when_local_is_behind() {
        let mut local = VectorClock::new();
        let mut remote = VectorClock::new();
        local.tick(peer(1));
        remote.tick(peer(1));
        remote.tick(peer(2));

        assert_eq!(
            resolve(&local, &remote, 100, 200, peer(2), peer(1)),
            Resolution::Accept
        );
    }

    #[test]
    fn resolve_rejects_when_local_is_ahead() {
        let mut local = VectorClock::new();
        let mut remote = VectorClock::new();
        local.tick(peer(1));
        local.tick(peer(1));
        remote.tick(peer(1));

        assert_eq!(
            resolve(&local, &remote, 100, 200, peer(2), peer(1)),
            Resolution::Reject
        );
    }

    #[test]
    fn resolve_conflict_on_concurrent() {
        let mut local = VectorClock::new();
        let mut remote = VectorClock::new();
        local.tick(peer(1));
        remote.tick(peer(2));

        let result = resolve(&local, &remote, 100, 200, peer(2), peer(1));
        assert!(matches!(result, Resolution::Conflict { .. }));
    }

    #[test]
    fn conflict_filename_with_extension() {
        let name = conflict_filename("config.toml", &peer(0xAB));
        assert_eq!(name, "config.conflict.ab000000.toml");
    }

    #[test]
    fn conflict_filename_without_extension() {
        let name = conflict_filename("Makefile", &peer(0xCD));
        assert_eq!(name, "Makefile.conflict.cd000000");
    }

    #[test]
    fn tombstone_not_expired() {
        let t = Tombstone::new("f.txt".into(), peer(1), VectorClock::new());
        assert!(!t.is_expired(3600));
    }

    #[test]
    fn tombstone_expired() {
        let mut t = Tombstone::new("f.txt".into(), peer(1), VectorClock::new());
        t.timestamp = 0; // epoch = long ago
        assert!(t.is_expired(3600));
    }
}

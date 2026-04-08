use std::time::{SystemTime, UNIX_EPOCH};

use crate::conflict::PeerId;

/// Current time as seconds since UNIX epoch.
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Convert a short PeerId to a hex string.
pub fn peer_id_hex(peer: &PeerId) -> String {
    peer.iter().map(|b| format!("{b:02x}")).collect()
}

/// Truncate a string to `max` characters, adding "..." prefix if truncated.
pub fn truncate_start(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let suffix: String = s
            .chars()
            .rev()
            .take(max - 3)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("...{suffix}")
    }
}

/// Truncate a string to `max` characters, adding "..." suffix if truncated.
pub fn truncate_end(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let prefix: String = s.chars().take(max - 3).collect();
        format!("{prefix}...")
    }
}

use std::path::PathBuf;

use p2sync_core::chunk::ChunkMeta;
use p2sync_core::conflict::{Tombstone, VectorClock};
use p2sync_core::merkle::Node;
use serde::{Deserialize, Serialize};

/// Request types for the sync protocol.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SyncRequest {
    /// Request the peer's Merkle tree root hash.
    GetRootHash,

    /// Request the full Merkle tree for reconciliation.
    GetTree,

    /// Request a subtree at a given path for partial reconciliation.
    GetSubtree { path: PathBuf },

    /// Request specific chunks of a file.
    GetChunks {
        path: PathBuf,
        chunk_indices: Vec<usize>,
    },

    /// Request file metadata (clock, timestamp).
    GetFileMeta { path: PathBuf },
}

/// Response types for the sync protocol.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SyncResponse {
    /// The peer's root hash.
    RootHash { hash: [u8; 32] },

    /// The full Merkle tree.
    Tree { tree: Node },

    /// A subtree at the requested path.
    Subtree { path: PathBuf, node: Option<Node> },

    /// Requested chunk data.
    Chunks {
        path: PathBuf,
        chunks: Vec<(usize, Vec<u8>)>,
    },

    /// File metadata.
    FileMeta {
        path: PathBuf,
        clock: Option<VectorClock>,
        last_modified: u64,
    },

    /// An error occurred processing the request.
    Error { message: String },
}

/// GossipSub notification messages for real-time sync.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SyncNotification {
    /// A file was modified by a peer.
    FileChanged {
        path: PathBuf,
        /// Aggregate file hash after modification.
        hash: [u8; 32],
        /// Changed chunks metadata (enables incremental transfer).
        chunks_changed: Vec<ChunkMeta>,
        /// Vector clock at time of modification.
        clock: VectorClock,
        timestamp: u64,
        /// Short peer ID of the modifier.
        peer: p2sync_core::conflict::PeerId,
    },

    /// A file was deleted.
    FileDeleted { tombstone: Tombstone },

    /// A new peer joined the sync group.
    PeerJoined { peer: p2sync_core::conflict::PeerId },
}

/// The GossipSub topic name for a sync group.
pub fn sync_topic(group_id: &str) -> String {
    format!("p2sync/{group_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_request_serializes_roundtrip() {
        let req = SyncRequest::GetChunks {
            path: PathBuf::from("src/main.rs"),
            chunk_indices: vec![0, 2, 5],
        };
        let bytes = serde_json::to_vec(&req).unwrap();
        let decoded: SyncRequest = serde_json::from_slice(&bytes).unwrap();
        if let SyncRequest::GetChunks {
            path,
            chunk_indices,
        } = decoded
        {
            assert_eq!(path, PathBuf::from("src/main.rs"));
            assert_eq!(chunk_indices, vec![0, 2, 5]);
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn sync_response_serializes_roundtrip() {
        let resp = SyncResponse::RootHash { hash: [0xAB; 32] };
        let bytes = serde_json::to_vec(&resp).unwrap();
        let decoded: SyncResponse = serde_json::from_slice(&bytes).unwrap();
        if let SyncResponse::RootHash { hash } = decoded {
            assert_eq!(hash, [0xAB; 32]);
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn notification_serializes_roundtrip() {
        let notif = SyncNotification::FileDeleted {
            tombstone: Tombstone::new("old.txt".into(), [1; 8], VectorClock::new()),
        };
        let bytes = serde_json::to_vec(&notif).unwrap();
        let decoded: SyncNotification = serde_json::from_slice(&bytes).unwrap();
        if let SyncNotification::FileDeleted { tombstone } = decoded {
            assert_eq!(tombstone.path, "old.txt");
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn sync_topic_format() {
        let topic = sync_topic("12D3KooW...");
        assert_eq!(topic, "p2sync/12D3KooW...");
    }
}

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::conflict::{PeerId, Tombstone, VectorClock};
use crate::merkle::{self, Node};

/// Per-file metadata tracked alongside the Merkle tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileMeta {
    pub clock: VectorClock,
    pub last_modified: u64,
    pub last_modified_by: PeerId,
}

/// The full sync index: Merkle tree + per-file clocks + tombstones.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncIndex {
    pub tree: Node,
    pub file_meta: HashMap<PathBuf, FileMeta>,
    pub tombstones: Vec<Tombstone>,
    pub local_peer: PeerId,
    #[serde(skip)]
    pub root_path: PathBuf,
    pub exclude_patterns: Vec<String>,
    pub chunk_size: usize,
    pub tombstone_ttl_secs: u64,
}

impl SyncIndex {
    /// Build a new index by scanning a directory with the given config.
    pub fn build(
        root: &Path,
        local_peer: PeerId,
        exclude_patterns: Vec<String>,
        storage_config: &crate::config::StorageConfig,
    ) -> std::io::Result<Self> {
        let exclude = |p: &Path| should_exclude(p, root, &exclude_patterns);
        let tree = merkle::from_path_configured(root, &exclude, storage_config.chunk_size)?;
        let mut file_meta = HashMap::new();
        collect_file_meta(&tree, &PathBuf::new(), local_peer, &mut file_meta);

        Ok(Self {
            tree,
            file_meta,
            tombstones: Vec::new(),
            local_peer,
            root_path: root.to_path_buf(),
            exclude_patterns,
            chunk_size: storage_config.chunk_size,
            tombstone_ttl_secs: storage_config.tombstone_ttl_secs,
        })
    }

    /// Update the index after a local file change.
    pub fn local_file_changed(&mut self, rel_path: &Path) -> std::io::Result<()> {
        let abs_path = self.root_path.join(rel_path);
        let data = std::fs::read(&abs_path)?;
        merkle::update_file_with_chunk_size(&mut self.tree, rel_path, &data, self.chunk_size);

        let meta = self
            .file_meta
            .entry(rel_path.to_path_buf())
            .or_insert_with(|| FileMeta {
                clock: VectorClock::new(),
                last_modified: 0,
                last_modified_by: self.local_peer,
            });
        meta.clock.tick(self.local_peer);
        meta.last_modified = now_secs();
        meta.last_modified_by = self.local_peer;

        Ok(())
    }

    /// Mark a file as deleted locally.
    pub fn local_file_deleted(&mut self, rel_path: &Path) {
        merkle::remove_path(&mut self.tree, rel_path);

        let clock = self
            .file_meta
            .remove(rel_path)
            .map(|m| m.clock)
            .unwrap_or_default();

        self.tombstones.push(Tombstone::new(
            rel_path.to_string_lossy().into_owned(),
            self.local_peer,
            clock,
        ));
    }

    /// Purge expired tombstones.
    pub fn gc_tombstones(&mut self) {
        self.tombstones
            .retain(|t| !t.is_expired(self.tombstone_ttl_secs));
    }

    /// Get file count in the tree.
    pub fn file_count(&self) -> usize {
        self.file_meta.len()
    }

    /// Get total size of all tracked files.
    pub fn total_size(&self) -> u64 {
        count_size(&self.tree)
    }

    /// Check if a path should be excluded.
    pub fn is_excluded(&self, path: &Path) -> bool {
        should_exclude(path, &self.root_path, &self.exclude_patterns)
    }
}

fn should_exclude(path: &Path, root: &Path, patterns: &[String]) -> bool {
    let rel = path.strip_prefix(root).unwrap_or(path);

    // Match pattern against any path component (exact match) or the filename
    patterns.iter().any(|pattern| {
        rel.components().any(|c| {
            if let std::path::Component::Normal(s) = c {
                s.to_string_lossy() == *pattern
            } else {
                false
            }
        })
    })
}

fn collect_file_meta(
    node: &Node,
    prefix: &Path,
    local_peer: PeerId,
    out: &mut HashMap<PathBuf, FileMeta>,
) {
    match node {
        Node::File { .. } => {
            out.insert(
                prefix.to_path_buf(),
                FileMeta {
                    clock: VectorClock::new(),
                    last_modified: now_secs(),
                    last_modified_by: local_peer,
                },
            );
        }
        Node::Directory { children, .. } => {
            for (name, child) in children {
                collect_file_meta(child, &prefix.join(name), local_peer, out);
            }
        }
    }
}

fn count_size(node: &Node) -> u64 {
    match node {
        Node::File { size, .. } => *size,
        Node::Directory { children, .. } => children.values().map(count_size).sum(),
    }
}

fn now_secs() -> u64 {
    crate::util::now_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::config::StorageConfig;

    fn test_peer() -> PeerId {
        [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08]
    }

    fn test_storage() -> StorageConfig {
        StorageConfig::default()
    }

    #[test]
    fn build_index_from_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"hello").unwrap();
        std::fs::write(dir.path().join("b.txt"), b"world").unwrap();

        let index = SyncIndex::build(dir.path(), test_peer(), vec![], &test_storage()).unwrap();

        assert_eq!(index.file_count(), 2);
        assert_eq!(index.total_size(), 10);
        assert!(index.file_meta.contains_key(Path::new("a.txt")));
        assert!(index.file_meta.contains_key(Path::new("b.txt")));
    }

    #[test]
    fn build_with_exclusions() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("keep.txt"), b"yes").unwrap();
        std::fs::create_dir(dir.path().join("node_modules")).unwrap();
        std::fs::write(dir.path().join("node_modules/pkg.json"), b"no").unwrap();

        let index = SyncIndex::build(
            dir.path(),
            test_peer(),
            vec!["node_modules".into()],
            &test_storage(),
        )
        .unwrap();

        assert_eq!(index.file_count(), 1);
        assert!(index.file_meta.contains_key(Path::new("keep.txt")));
    }

    #[test]
    fn local_file_changed_updates_tree_and_clock() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), b"v1").unwrap();

        let mut index = SyncIndex::build(dir.path(), test_peer(), vec![], &test_storage()).unwrap();
        let old_hash = *index.tree.hash();

        std::fs::write(dir.path().join("f.txt"), b"v2").unwrap();
        index.local_file_changed(Path::new("f.txt")).unwrap();

        assert_ne!(*index.tree.hash(), old_hash);
        assert_eq!(
            index.file_meta[Path::new("f.txt")].clock.counters[&test_peer()],
            1
        );
    }

    #[test]
    fn local_file_deleted_creates_tombstone() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), b"data").unwrap();

        let mut index = SyncIndex::build(dir.path(), test_peer(), vec![], &test_storage()).unwrap();
        index.local_file_deleted(Path::new("f.txt"));

        assert_eq!(index.file_count(), 0);
        assert_eq!(index.tombstones.len(), 1);
        assert_eq!(index.tombstones[0].path, "f.txt");
    }

    #[test]
    fn gc_removes_expired_tombstones() {
        let dir = tempfile::tempdir().unwrap();
        let mut index = SyncIndex::build(dir.path(), test_peer(), vec![], &test_storage()).unwrap();

        let mut old = Tombstone::new("old.txt".into(), test_peer(), VectorClock::new());
        old.timestamp = 0;
        let fresh = Tombstone::new("new.txt".into(), test_peer(), VectorClock::new());

        index.tombstones.push(old);
        index.tombstones.push(fresh);

        index.gc_tombstones();

        assert_eq!(index.tombstones.len(), 1);
        assert_eq!(index.tombstones[0].path, "new.txt");
    }

    #[test]
    fn exclude_pattern_matching() {
        let root = Path::new("/project");
        assert!(should_exclude(
            Path::new("/project/node_modules/foo"),
            root,
            &["node_modules".into()]
        ));
        assert!(should_exclude(
            Path::new("/project/.git"),
            root,
            &[".git".into()]
        ));
        assert!(!should_exclude(
            Path::new("/project/src/main.rs"),
            root,
            &["node_modules".into()]
        ));
    }
}

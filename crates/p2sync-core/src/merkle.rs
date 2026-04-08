use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::chunk::{self, ChunkMeta, DEFAULT_CHUNK_SIZE};

/// A node in the Merkle tree, representing either a file or a directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Node {
    File {
        hash: [u8; 32],
        size: u64,
        chunks: Vec<ChunkMeta>,
    },
    Directory {
        hash: [u8; 32],
        children: BTreeMap<String, Node>,
    },
}

impl Node {
    pub fn hash(&self) -> &[u8; 32] {
        match self {
            Node::File { hash, .. } | Node::Directory { hash, .. } => hash,
        }
    }

    pub fn is_file(&self) -> bool {
        matches!(self, Node::File { .. })
    }

    pub fn is_dir(&self) -> bool {
        matches!(self, Node::Directory { .. })
    }
}

/// Build a file node from raw data using the default chunk size.
pub fn file_node(data: &[u8]) -> Node {
    file_node_with_chunk_size(data, DEFAULT_CHUNK_SIZE)
}

/// Build a file node from raw data with a custom chunk size.
pub fn file_node_with_chunk_size(data: &[u8], chunk_size: usize) -> Node {
    let chunks = chunk::metadata(data, chunk_size);
    let hash = chunk::file_hash(&chunks);
    Node::File {
        hash,
        size: data.len() as u64,
        chunks,
    }
}

/// Compute the hash of a directory from its children.
fn dir_hash(children: &BTreeMap<String, Node>) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    for (name, node) in children {
        hasher.update(name.as_bytes());
        hasher.update(node.hash());
    }
    *hasher.finalize().as_bytes()
}

/// Build a directory node from children.
pub fn dir_node(children: BTreeMap<String, Node>) -> Node {
    let hash = dir_hash(&children);
    Node::Directory { hash, children }
}

/// Build a Merkle tree from a filesystem directory using default chunk size.
pub fn from_path(root: &Path) -> std::io::Result<Node> {
    from_path_configured(root, &|_| false, DEFAULT_CHUNK_SIZE)
}

/// Build a Merkle tree, skipping excluded paths, using default chunk size.
pub fn from_path_filtered(root: &Path, exclude: &dyn Fn(&Path) -> bool) -> std::io::Result<Node> {
    from_path_configured(root, exclude, DEFAULT_CHUNK_SIZE)
}

/// Build a Merkle tree with full configuration.
pub fn from_path_configured(
    root: &Path,
    exclude: &dyn Fn(&Path) -> bool,
    chunk_size: usize,
) -> std::io::Result<Node> {
    let meta = std::fs::symlink_metadata(root)?;
    if meta.is_symlink() {
        return Ok(dir_node(BTreeMap::new()));
    }

    if meta.is_file() {
        let data = std::fs::read(root)?;
        return Ok(file_node_with_chunk_size(&data, chunk_size));
    }

    let mut children = BTreeMap::new();

    let mut entries: Vec<_> = std::fs::read_dir(root)?.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();

        if path.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
            continue;
        }

        if exclude(&path) {
            continue;
        }

        let name = entry
            .file_name()
            .to_str()
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "non-UTF8 filename")
            })?
            .to_string();

        let node = from_path_configured(&path, exclude, chunk_size)?;
        children.insert(name, node);
    }

    Ok(dir_node(children))
}

/// Diff result between two Merkle trees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffEntry {
    Added(PathBuf, Node),
    Removed(PathBuf, Node),
    Modified(PathBuf, Node, Node),
}

/// Compare two Merkle trees and return the differences.
pub fn diff(old: &Node, new: &Node) -> Vec<DiffEntry> {
    let mut result = Vec::new();
    diff_recursive(old, new, &PathBuf::new(), &mut result);
    result
}

fn diff_recursive(old: &Node, new: &Node, prefix: &Path, result: &mut Vec<DiffEntry>) {
    if old.hash() == new.hash() {
        return;
    }

    match (old, new) {
        (
            Node::Directory {
                children: old_children,
                ..
            },
            Node::Directory {
                children: new_children,
                ..
            },
        ) => {
            for (name, new_node) in new_children {
                let child_path = prefix.join(name);
                match old_children.get(name) {
                    Some(old_node) => diff_recursive(old_node, new_node, &child_path, result),
                    None => result.push(DiffEntry::Added(child_path, new_node.clone())),
                }
            }
            for (name, old_node) in old_children {
                if !new_children.contains_key(name) {
                    result.push(DiffEntry::Removed(prefix.join(name), old_node.clone()));
                }
            }
        }
        _ => {
            result.push(DiffEntry::Modified(
                prefix.to_path_buf(),
                old.clone(),
                new.clone(),
            ));
        }
    }
}

/// Update a single file in an existing Merkle tree using default chunk size.
pub fn update_file(tree: &mut Node, rel_path: &Path, data: &[u8]) {
    let new_file = file_node(data);
    insert_node(tree, rel_path, new_file);
}

/// Update a single file with a custom chunk size.
pub fn update_file_with_chunk_size(
    tree: &mut Node,
    rel_path: &Path,
    data: &[u8],
    chunk_size: usize,
) {
    let new_file = file_node_with_chunk_size(data, chunk_size);
    insert_node(tree, rel_path, new_file);
}

/// Remove a file or directory from the tree.
pub fn remove_path(tree: &mut Node, rel_path: &Path) -> bool {
    let components: Vec<&str> = rel_path
        .components()
        .filter_map(|c| {
            if let std::path::Component::Normal(s) = c {
                s.to_str()
            } else {
                None
            }
        })
        .collect();

    remove_recursive(tree, &components)
}

fn remove_recursive(node: &mut Node, components: &[&str]) -> bool {
    let Node::Directory { children, hash } = node else {
        return false;
    };

    let Some((&first, rest)) = components.split_first() else {
        return false;
    };

    if rest.is_empty() {
        let removed = children.remove(first).is_some();
        if removed {
            *hash = dir_hash(children);
        }
        return removed;
    }

    let Some(child) = children.get_mut(first) else {
        return false;
    };

    let removed = remove_recursive(child, rest);
    if removed {
        *hash = dir_hash(children);
    }
    removed
}

fn insert_node(tree: &mut Node, rel_path: &Path, new_node: Node) {
    let components: Vec<&str> = rel_path
        .components()
        .filter_map(|c| {
            if let std::path::Component::Normal(s) = c {
                s.to_str()
            } else {
                None
            }
        })
        .collect();

    insert_recursive(tree, &components, new_node);
}

fn insert_recursive(node: &mut Node, components: &[&str], new_node: Node) {
    let Node::Directory { children, hash } = node else {
        return;
    };

    let Some((&first, rest)) = components.split_first() else {
        return;
    };

    if rest.is_empty() {
        children.insert(first.to_string(), new_node);
        *hash = dir_hash(children);
        return;
    }

    let child = children
        .entry(first.to_string())
        .or_insert_with(|| dir_node(BTreeMap::new()));

    insert_recursive(child, rest, new_node);

    *hash = dir_hash(children);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_node_from_data() {
        let node = file_node(b"hello");
        assert!(node.is_file());
        if let Node::File { size, chunks, .. } = &node {
            assert_eq!(*size, 5);
            assert_eq!(chunks.len(), 1);
        }
    }

    #[test]
    fn empty_dir_node() {
        let node = dir_node(BTreeMap::new());
        assert!(node.is_dir());
    }

    #[test]
    fn dir_hash_depends_on_children() {
        let a = dir_node(BTreeMap::from([("a.txt".into(), file_node(b"aaa"))]));
        let b = dir_node(BTreeMap::from([("a.txt".into(), file_node(b"bbb"))]));
        assert_ne!(a.hash(), b.hash());
    }

    #[test]
    fn dir_hash_depends_on_names() {
        let a = dir_node(BTreeMap::from([("file1.txt".into(), file_node(b"data"))]));
        let b = dir_node(BTreeMap::from([("file2.txt".into(), file_node(b"data"))]));
        assert_ne!(a.hash(), b.hash());
    }

    #[test]
    fn identical_trees_have_same_hash() {
        let make_tree = || {
            dir_node(BTreeMap::from([
                ("a.txt".into(), file_node(b"hello")),
                ("b.txt".into(), file_node(b"world")),
            ]))
        };
        assert_eq!(make_tree().hash(), make_tree().hash());
    }

    #[test]
    fn diff_identical_trees_is_empty() {
        let tree = dir_node(BTreeMap::from([("f.txt".into(), file_node(b"data"))]));
        assert!(diff(&tree, &tree).is_empty());
    }

    #[test]
    fn diff_detects_added_file() {
        let old = dir_node(BTreeMap::new());
        let new = dir_node(BTreeMap::from([("new.txt".into(), file_node(b"new"))]));
        let d = diff(&old, &new);
        assert_eq!(d.len(), 1);
        assert!(matches!(&d[0], DiffEntry::Added(p, _) if p == Path::new("new.txt")));
    }

    #[test]
    fn diff_detects_removed_file() {
        let old = dir_node(BTreeMap::from([("old.txt".into(), file_node(b"old"))]));
        let new = dir_node(BTreeMap::new());
        let d = diff(&old, &new);
        assert_eq!(d.len(), 1);
        assert!(matches!(&d[0], DiffEntry::Removed(p, _) if p == Path::new("old.txt")));
    }

    #[test]
    fn diff_detects_modified_file() {
        let old = dir_node(BTreeMap::from([("f.txt".into(), file_node(b"v1"))]));
        let new = dir_node(BTreeMap::from([("f.txt".into(), file_node(b"v2"))]));
        let d = diff(&old, &new);
        assert_eq!(d.len(), 1);
        assert!(matches!(&d[0], DiffEntry::Modified(p, _, _) if p == Path::new("f.txt")));
    }

    #[test]
    fn diff_recurses_into_subdirectories() {
        let old = dir_node(BTreeMap::from([(
            "sub".into(),
            dir_node(BTreeMap::from([("a.txt".into(), file_node(b"old"))])),
        )]));
        let new = dir_node(BTreeMap::from([(
            "sub".into(),
            dir_node(BTreeMap::from([("a.txt".into(), file_node(b"new"))])),
        )]));
        let d = diff(&old, &new);
        assert_eq!(d.len(), 1);
        assert!(matches!(&d[0], DiffEntry::Modified(p, _, _) if p == Path::new("sub/a.txt")));
    }

    #[test]
    fn update_file_in_tree() {
        let mut tree = dir_node(BTreeMap::from([("f.txt".into(), file_node(b"v1"))]));
        let old_hash = *tree.hash();

        update_file(&mut tree, Path::new("f.txt"), b"v2");

        assert_ne!(*tree.hash(), old_hash);
        if let Node::Directory { children, .. } = &tree {
            if let Node::File { size, .. } = &children["f.txt"] {
                assert_eq!(*size, 2);
            }
        }
    }

    #[test]
    fn update_creates_intermediate_dirs() {
        let mut tree = dir_node(BTreeMap::new());
        update_file(&mut tree, Path::new("a/b/c.txt"), b"deep");

        if let Node::Directory { children, .. } = &tree {
            assert!(children.contains_key("a"));
            if let Node::Directory {
                children: a_children,
                ..
            } = &children["a"]
            {
                assert!(a_children.contains_key("b"));
            }
        }
    }

    #[test]
    fn remove_file_from_tree() {
        let mut tree = dir_node(BTreeMap::from([
            ("a.txt".into(), file_node(b"data")),
            ("b.txt".into(), file_node(b"keep")),
        ]));

        assert!(remove_path(&mut tree, Path::new("a.txt")));

        if let Node::Directory { children, .. } = &tree {
            assert_eq!(children.len(), 1);
            assert!(children.contains_key("b.txt"));
        }
    }

    #[test]
    fn remove_nested_file() {
        let mut tree = dir_node(BTreeMap::from([(
            "sub".into(),
            dir_node(BTreeMap::from([
                ("f.txt".into(), file_node(b"data")),
                ("g.txt".into(), file_node(b"keep")),
            ])),
        )]));

        assert!(remove_path(&mut tree, Path::new("sub/f.txt")));

        if let Node::Directory { children, .. } = &tree {
            if let Node::Directory { children: sub, .. } = &children["sub"] {
                assert_eq!(sub.len(), 1);
                assert!(sub.contains_key("g.txt"));
            }
        }
    }

    #[test]
    fn remove_nonexistent_returns_false() {
        let mut tree = dir_node(BTreeMap::new());
        assert!(!remove_path(&mut tree, Path::new("nope.txt")));
    }

    #[test]
    fn from_path_reads_real_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"alpha").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/b.txt"), b"beta").unwrap();

        let tree = from_path(dir.path()).unwrap();

        if let Node::Directory { children, .. } = &tree {
            assert!(children.contains_key("a.txt"));
            assert!(children.contains_key("sub"));
            if let Node::Directory { children: sub, .. } = &children["sub"] {
                assert!(sub.contains_key("b.txt"));
            }
        } else {
            panic!("expected directory node");
        }
    }

    #[test]
    fn from_path_filtered_excludes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("keep.txt"), b"yes").unwrap();
        std::fs::write(dir.path().join("skip.tmp"), b"no").unwrap();

        let tree =
            from_path_filtered(dir.path(), &|p| p.extension().is_some_and(|e| e == "tmp")).unwrap();

        if let Node::Directory { children, .. } = &tree {
            assert!(children.contains_key("keep.txt"));
            assert!(!children.contains_key("skip.tmp"));
        }
    }
}

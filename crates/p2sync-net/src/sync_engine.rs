use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use libp2p::PeerId;
use libp2p::request_response;
use tokio::sync::{Mutex, mpsc};
use tracing::{debug, info, warn};

use p2sync_core::chunk;
use p2sync_core::config::P2SyncConfig;
use p2sync_core::conflict::{self, Resolution, VectorClock};
use p2sync_core::index::SyncIndex;
use p2sync_core::merkle::{self, DiffEntry, Node};
use p2sync_core::watcher::{self, FsEvent};

use crate::protocol::{SyncNotification, SyncRequest, SyncResponse};
use crate::swarm::{self, Event, NetworkHandle};

fn safe_path(root: &Path, rel: &Path) -> Option<PathBuf> {
    if rel.is_absolute() {
        return None;
    }
    for component in rel.components() {
        match component {
            std::path::Component::Normal(_) => {}
            _ => return None,
        }
    }
    let joined = root.join(rel);
    if joined.starts_with(root) {
        Some(joined)
    } else {
        None
    }
}

/// Events emitted by the sync engine for the UI layer.
#[derive(Debug, Clone)]
pub enum SyncEvent {
    IndexReady {
        file_count: usize,
        total_size: u64,
    },
    PeerConnected(PeerId),
    PeerDisconnected(PeerId),
    Listening(String),
    TransferStarted {
        path: PathBuf,
        direction: TransferDirection,
        total_chunks: usize,
    },
    ChunkTransferred {
        path: PathBuf,
        direction: TransferDirection,
        chunk_index: usize,
        total_chunks: usize,
    },
    TransferComplete {
        path: PathBuf,
        direction: TransferDirection,
    },
    Conflict {
        path: PathBuf,
        conflict_path: PathBuf,
    },
    RemoteDelete {
        path: PathBuf,
    },
    ReconciliationComplete {
        peer: PeerId,
        files_synced: usize,
    },
    Log(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferDirection {
    Upload,
    Download,
}

pub struct SyncConfig {
    pub root_path: PathBuf,
    pub group_id: String,
    pub exclude_patterns: Vec<String>,
    /// Don't push local changes to peers (disable watcher notifications).
    pub no_push: bool,
    /// Don't accept incoming changes from peers (reject downloads during reconciliation).
    pub no_pull: bool,
}

/// Time-based guard to suppress duplicate events for the same path within a TTL window.
/// Used both for sync-write suppression (prevent watcher feedback) and event debouncing.
#[derive(Clone)]
struct EventGuard {
    inner: Arc<std::sync::Mutex<HashMap<PathBuf, Instant>>>,
    ttl: std::time::Duration,
}

impl EventGuard {
    fn new(ttl: std::time::Duration) -> Self {
        Self {
            inner: Arc::default(),
            ttl,
        }
    }

    fn mark(&self, path: PathBuf) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(path, Instant::now());
    }

    fn is_suppressed(&self, path: &Path) -> bool {
        let mut map = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(written_at) = map.get(path) {
            if written_at.elapsed() < self.ttl {
                return true;
            }
            map.remove(path);
        }
        false
    }
}

/// Shared context that can be cloned into spawned tasks.
#[derive(Clone)]
struct Ctx {
    index: Arc<Mutex<SyncIndex>>,
    net: NetworkHandle,
    ui: mpsc::Sender<SyncEvent>,
    root: PathBuf,
    local_id: conflict::PeerId,
    no_push: bool,
    no_pull: bool,
    /// Suppress watcher events for files written by the sync engine.
    sync_write_guard: EventGuard,
    /// Deduplicate rapid filesystem events for the same path.
    change_debounce: EventGuard,
    config: Arc<P2SyncConfig>,
}

struct RemoteOrigin {
    clock: VectorClock,
    timestamp: u64,
    peer: conflict::PeerId,
}

/// Run the sync engine.
pub async fn run(
    keypair: libp2p::identity::Keypair,
    config: SyncConfig,
    app_config: P2SyncConfig,
    listen_addr: libp2p::Multiaddr,
    ui_tx: mpsc::Sender<SyncEvent>,
) -> anyhow::Result<()> {
    let local_peer_id = keypair.public().to_peer_id();
    let short_id = swarm::short_peer_id(&local_peer_id);
    let app_config = Arc::new(app_config);

    let _ = ui_tx
        .send(SyncEvent::Log(format!(
            "indexing {}",
            config.root_path.display()
        )))
        .await;

    let index = SyncIndex::build(
        &config.root_path,
        short_id,
        config.exclude_patterns.clone(),
        &app_config.storage,
    )?;
    let _ = ui_tx
        .send(SyncEvent::IndexReady {
            file_count: index.file_count(),
            total_size: index.total_size(),
        })
        .await;

    let index = Arc::new(Mutex::new(index));
    let (network_loop, net_handle, mut event_rx) = swarm::build(keypair, &app_config.network)?;
    let network_task = tokio::spawn(network_loop.run());

    net_handle.listen(listen_addr).await?;
    net_handle.subscribe(&config.group_id).await?;

    // Log discovery mode
    if app_config.network.is_wan() {
        let _ = ui_tx
            .send(SyncEvent::Log("discovery: LAN + WAN (DHT)".into()))
            .await;
    } else {
        let _ = ui_tx
            .send(SyncEvent::Log("discovery: LAN only".into()))
            .await;
    }

    // Bootstrap Kademlia if WAN mode is enabled
    if app_config.network.is_wan() {
        let _ = ui_tx
            .send(SyncEvent::Log(
                "connecting to DHT bootstrap nodes...".into(),
            ))
            .await;
        for addr_str in crate::behaviour::IPFS_BOOTSTRAP_NODES {
            if let Ok(addr) = addr_str.parse::<libp2p::Multiaddr>() {
                // Extract PeerId from the multiaddr
                if let Some(libp2p::multiaddr::Protocol::P2p(peer_id)) = addr.iter().last() {
                    net_handle.add_kad_address(peer_id, addr.clone()).await?;
                }
            }
        }
        net_handle.bootstrap().await?;

        // If the group_id is a PeerId (joining a group), search for it via DHT
        if let Ok(target_peer) = config.group_id.parse::<PeerId>() {
            let _ = ui_tx
                .send(SyncEvent::Log("searching for peer on DHT...".into()))
                .await;
            net_handle.search_peer(target_peer).await?;
        }
    }

    // Connect to relay if configured
    match &app_config.network.relay {
        p2sync_core::config::RelayMode::None => {}
        p2sync_core::config::RelayMode::Explicit(relay_addr) => {
            let _ = ui_tx
                .send(SyncEvent::Log(format!("connecting to relay {relay_addr}")))
                .await;
            if let Ok(addr) = relay_addr.parse::<libp2p::Multiaddr>() {
                // Listen via the relay so we're reachable through it
                let relay_listen = addr.clone().with(libp2p::multiaddr::Protocol::P2pCircuit);
                net_handle.listen(relay_listen).await?;
            } else {
                warn!("invalid relay multiaddr: {relay_addr}");
            }
        }
        p2sync_core::config::RelayMode::Auto => {
            warn!("relay = \"auto\" routes traffic through third-party nodes");
            let _ = ui_tx
                .send(SyncEvent::Log(
                    "WARNING: relay auto uses third-party nodes — not yet implemented, direct connections only"
                        .into(),
                ))
                .await;
        }
    }

    let (fs_rx, _guard) = watcher::watch(&config.root_path, config.exclude_patterns.clone())?;

    let (fs_tx_async, mut fs_rx_async) =
        mpsc::channel::<FsEvent>(app_config.network.channel_capacity);
    let sync_write_guard = EventGuard::new(app_config.watcher.write_guard_ttl());
    let change_debounce = EventGuard::new(app_config.watcher.debounce());
    let bridge_guard = sync_write_guard.clone();
    std::thread::spawn(move || {
        while let Ok(event) = fs_rx.recv() {
            // Pre-filter in the sync thread using the write guard
            let path = match &event {
                FsEvent::Created(p) | FsEvent::Modified(p) | FsEvent::Removed(p) => p,
            };
            if bridge_guard.is_suppressed(path) {
                continue;
            }
            if fs_tx_async.blocking_send(event).is_err() {
                break;
            }
        }
    });

    let ctx = Ctx {
        index,
        net: net_handle.clone(),
        ui: ui_tx,
        root: config.root_path,
        local_id: short_id,
        no_push: config.no_push,
        no_pull: config.no_pull,
        sync_write_guard,
        change_debounce,
        config: app_config,
    };

    loop {
        tokio::select! {
            Some(event) = event_rx.recv() => {
                handle_network_event(event, &ctx).await;
            }
            Some(fs_event) = fs_rx_async.recv() => {
                if !ctx.no_push {
                    handle_fs_event(fs_event, &ctx).await;
                }
            }
            else => break,
        }
    }

    let _ = net_handle.shutdown().await;
    network_task.await?;
    Ok(())
}

async fn handle_network_event(event: Event, ctx: &Ctx) {
    match event {
        Event::PeerDiscovered(peer_id) => {
            info!("peer discovered: {peer_id}");
            let _ = ctx.ui.send(SyncEvent::PeerConnected(peer_id)).await;
            let _ = ctx
                .ui
                .send(SyncEvent::Log(format!("peer discovered: {peer_id}")))
                .await;

            // Spawn reconciliation so it doesn't block the event loop
            let ctx = ctx.clone();
            tokio::spawn(async move {
                if let Err(e) = reconcile(peer_id, &ctx).await {
                    warn!("reconciliation with {peer_id} failed: {e}");
                    let _ = ctx
                        .ui
                        .send(SyncEvent::Log(format!("reconciliation failed: {e}")))
                        .await;
                }
            });
        }
        Event::PeerDisconnected(peer_id) => {
            let _ = ctx.ui.send(SyncEvent::PeerDisconnected(peer_id)).await;
            let _ = ctx
                .ui
                .send(SyncEvent::Log(format!("peer disconnected: {peer_id}")))
                .await;
        }
        Event::Listening(addr) => {
            let _ = ctx.ui.send(SyncEvent::Listening(addr.to_string())).await;
        }
        Event::InboundRequest {
            request, channel, ..
        } => {
            handle_inbound_request(request, channel, ctx).await;
        }
        Event::Notification {
            source,
            notification,
        } => {
            if !ctx.no_pull {
                // Spawn so notification handling doesn't block event loop
                let ctx = ctx.clone();
                tokio::spawn(async move {
                    handle_notification(source, notification, &ctx).await;
                });
            }
        }
    }
}

async fn handle_inbound_request(
    request: SyncRequest,
    channel: request_response::ResponseChannel<SyncResponse>,
    ctx: &Ctx,
) {
    let idx = ctx.index.lock().await;
    let response = match request {
        SyncRequest::GetRootHash => SyncResponse::RootHash {
            hash: *idx.tree.hash(),
        },
        SyncRequest::GetTree => SyncResponse::Tree {
            tree: idx.tree.clone(),
        },
        SyncRequest::GetSubtree { path } => {
            let node = find_node(&idx.tree, &path);
            SyncResponse::Subtree {
                path,
                node: node.cloned(),
            }
        }
        SyncRequest::GetChunks {
            path,
            chunk_indices,
        } => match safe_path(&ctx.root, &path) {
            Some(abs_path) => match std::fs::read(&abs_path) {
                Ok(data) => {
                    let (_, chunks) = chunk::split(&data, ctx.config.storage.chunk_size);
                    let requested: Vec<(usize, Vec<u8>)> = chunk_indices
                        .iter()
                        .filter(|&&i| i < ctx.config.storage.max_chunk_index)
                        .filter_map(|&i| chunks.get(i).map(|c| (i, c.data.clone())))
                        .collect();
                    SyncResponse::Chunks {
                        path,
                        chunks: requested,
                    }
                }
                Err(e) => SyncResponse::Error {
                    message: format!("read error: {e}"),
                },
            },
            None => SyncResponse::Error {
                message: "invalid path".into(),
            },
        },
        SyncRequest::GetFileMeta { path } => {
            let meta = idx.file_meta.get(&path);
            SyncResponse::FileMeta {
                path,
                clock: meta.map(|m| m.clock.clone()),
                last_modified: meta.map(|m| m.last_modified).unwrap_or(0),
            }
        }
    };
    drop(idx);
    let _ = ctx.net.respond(channel, response).await;
}

async fn handle_notification(source: PeerId, notification: SyncNotification, ctx: &Ctx) {
    match notification {
        SyncNotification::FileChanged {
            path,
            chunks_changed,
            clock: remote_clock,
            timestamp: remote_ts,
            peer: remote_peer,
            ..
        } => {
            // Ignore our own notifications (GossipSub may echo them back)
            if remote_peer == ctx.local_id {
                return;
            }
            if safe_path(&ctx.root, &path).is_none() {
                warn!("rejected path traversal attempt: {}", path.display());
                return;
            }
            let idx = ctx.index.lock().await;
            let local_meta = idx.file_meta.get(&path);
            let local_clock = local_meta.map(|m| m.clock.clone()).unwrap_or_default();
            let local_ts = local_meta.map(|m| m.last_modified).unwrap_or(0);

            let resolution = conflict::resolve(
                &local_clock,
                &remote_clock,
                local_ts,
                remote_ts,
                remote_peer,
                ctx.local_id,
            );

            let chunk_indices: Vec<usize> = chunks_changed.iter().map(|c| c.index).collect();

            match resolution {
                Resolution::Accept => {
                    drop(idx);
                    // Pre-mark write guard before downloading to prevent watcher race
                    ctx.sync_write_guard.mark(path.to_path_buf());
                    let origin = RemoteOrigin {
                        clock: remote_clock,
                        timestamp: remote_ts,
                        peer: remote_peer,
                    };
                    fetch_and_apply(source, &path, &chunk_indices, ctx, origin).await;
                }
                Resolution::Reject => {
                    debug!("rejecting update for {} from {source}", path.display());
                }
                Resolution::Conflict { loser_peer } => {
                    let conflict_name =
                        conflict::conflict_filename(&path.to_string_lossy(), &loser_peer);
                    let conflict_path = PathBuf::from(&conflict_name);

                    if loser_peer == ctx.local_id {
                        let abs = ctx.root.join(&path);
                        let backup = ctx.root.join(&conflict_name);
                        if abs.exists() {
                            let _ = std::fs::copy(&abs, &backup);
                        }
                        drop(idx);
                        let origin = RemoteOrigin {
                            clock: remote_clock,
                            timestamp: remote_ts,
                            peer: remote_peer,
                        };
                        fetch_and_apply(source, &path, &chunk_indices, ctx, origin).await;
                    } else {
                        drop(idx);
                        let origin = RemoteOrigin {
                            clock: remote_clock,
                            timestamp: remote_ts,
                            peer: remote_peer,
                        };
                        fetch_and_apply(source, &conflict_path, &chunk_indices, ctx, origin).await;
                    }

                    let _ = ctx
                        .ui
                        .send(SyncEvent::Conflict {
                            path,
                            conflict_path,
                        })
                        .await;
                }
            }
        }

        SyncNotification::FileDeleted { tombstone } if tombstone.deleted_by != ctx.local_id => {
            let path = PathBuf::from(&tombstone.path);
            let Some(abs_path) = safe_path(&ctx.root, &path) else {
                warn!("rejected path traversal in delete: {}", path.display());
                return;
            };
            if abs_path.exists() {
                let _ = std::fs::remove_file(&abs_path);
            }
            let mut idx = ctx.index.lock().await;
            merkle::remove_path(&mut idx.tree, &path);
            idx.file_meta.remove(&path);
            idx.tombstones.push(tombstone);
            let _ = ctx.ui.send(SyncEvent::RemoteDelete { path }).await;
        }

        SyncNotification::PeerJoined { .. } | SyncNotification::FileDeleted { .. } => {}
    }
}

async fn fetch_and_apply(
    peer: PeerId,
    path: &Path,
    chunk_indices: &[usize],
    ctx: &Ctx,
    origin: RemoteOrigin,
) {
    let Some(abs_path) = safe_path(&ctx.root, path) else {
        warn!("rejected path traversal in fetch: {}", path.display());
        return;
    };

    // Check exclude patterns on incoming files
    {
        let idx = ctx.index.lock().await;
        if idx.is_excluded(&abs_path) {
            debug!("skipping excluded path: {}", path.display());
            return;
        }
    }

    let total = chunk_indices.len();
    let _ = ctx
        .ui
        .send(SyncEvent::TransferStarted {
            path: path.to_path_buf(),
            direction: TransferDirection::Download,
            total_chunks: total,
        })
        .await;

    let response = ctx
        .net
        .request(
            peer,
            SyncRequest::GetChunks {
                path: path.to_path_buf(),
                chunk_indices: chunk_indices.to_vec(),
            },
        )
        .await;

    match response {
        Ok(SyncResponse::Chunks { chunks, .. }) => {
            let mut file_data = std::fs::read(&abs_path).unwrap_or_default();

            for (i, (chunk_idx, data)) in chunks.iter().enumerate() {
                if *chunk_idx >= ctx.config.storage.max_chunk_index {
                    warn!("chunk index too large: {chunk_idx}");
                    continue;
                }
                let Some(offset) = chunk_idx.checked_mul(ctx.config.storage.chunk_size) else {
                    warn!("chunk offset overflow for index {chunk_idx}");
                    continue;
                };
                let Some(required_len) = offset.checked_add(data.len()) else {
                    warn!("file size overflow for {}", path.display());
                    continue;
                };
                if required_len as u64 > ctx.config.storage.max_file_size {
                    warn!(
                        "file {} exceeds max size ({} > {} bytes)",
                        path.display(),
                        required_len,
                        ctx.config.storage.max_file_size
                    );
                    return;
                }
                if file_data.len() < required_len {
                    file_data.resize(required_len, 0);
                }
                file_data[offset..offset + data.len()].copy_from_slice(data);

                let _ = ctx
                    .ui
                    .send(SyncEvent::ChunkTransferred {
                        path: path.to_path_buf(),
                        direction: TransferDirection::Download,
                        chunk_index: i,
                        total_chunks: total,
                    })
                    .await;
            }

            if let Some(parent) = abs_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }

            // Mark path so the watcher ignores the write we're about to do
            ctx.sync_write_guard.mark(path.to_path_buf());

            if let Err(e) = std::fs::write(&abs_path, &file_data) {
                warn!("failed to write {}: {e}", abs_path.display());
                return;
            }

            let mut idx = ctx.index.lock().await;
            merkle::update_file_with_chunk_size(
                &mut idx.tree,
                path,
                &file_data,
                ctx.config.storage.chunk_size,
            );

            let meta = idx.file_meta.entry(path.to_path_buf()).or_insert_with(|| {
                p2sync_core::index::FileMeta {
                    clock: VectorClock::new(),
                    last_modified: 0,
                    last_modified_by: origin.peer,
                }
            });
            meta.clock.merge(&origin.clock);
            meta.last_modified = origin.timestamp;
            meta.last_modified_by = origin.peer;

            let _ = ctx
                .ui
                .send(SyncEvent::TransferComplete {
                    path: path.to_path_buf(),
                    direction: TransferDirection::Download,
                })
                .await;
            let _ = ctx
                .ui
                .send(SyncEvent::Log(format!("synced {}", path.display())))
                .await;
        }
        Ok(SyncResponse::Error { message }) => {
            warn!("peer error fetching {}: {message}", path.display())
        }
        Ok(_) => warn!("unexpected response type for chunk request"),
        Err(e) => warn!("request failed for {}: {e}", path.display()),
    }
}

async fn handle_fs_event(event: FsEvent, ctx: &Ctx) {
    match event {
        FsEvent::Created(rel_path) | FsEvent::Modified(rel_path) => {
            // Skip if this was recently written by the sync engine itself
            if ctx.sync_write_guard.is_suppressed(&rel_path) {
                return;
            }

            // Debounce: skip if we already processed this file recently
            if ctx.change_debounce.is_suppressed(&rel_path) {
                return;
            }
            ctx.change_debounce.mark(rel_path.clone());

            // Skip conflict backup files
            if rel_path.to_string_lossy().contains(".conflict.") {
                return;
            }

            let abs_path = ctx.root.join(&rel_path);
            if !abs_path.is_file() {
                return;
            }

            let mut idx = ctx.index.lock().await;
            if idx.is_excluded(&abs_path) {
                return;
            }

            if let Err(e) = idx.local_file_changed(&rel_path) {
                warn!("index update failed for {}: {e}", rel_path.display());
                return;
            }

            let Some(meta) = idx.file_meta.get(&rel_path) else {
                warn!("file meta missing after update for {}", rel_path.display());
                return;
            };
            let data = match std::fs::read(&abs_path) {
                Ok(d) => d,
                Err(_) => return,
            };
            let chunks_meta = chunk::metadata(&data, ctx.config.storage.chunk_size);
            let hash = chunk::file_hash(&chunks_meta);

            let notification = SyncNotification::FileChanged {
                path: rel_path.clone(),
                hash,
                chunks_changed: chunks_meta,
                clock: meta.clock.clone(),
                timestamp: meta.last_modified,
                peer: ctx.local_id,
            };
            drop(idx);

            let _ = ctx.net.notify(notification).await;
            let _ = ctx
                .ui
                .send(SyncEvent::Log(format!("modified {}", rel_path.display())))
                .await;
        }

        FsEvent::Removed(rel_path) => {
            let mut idx = ctx.index.lock().await;
            idx.local_file_deleted(&rel_path);

            if let Some(tombstone) = idx.tombstones.last().cloned() {
                let notification = SyncNotification::FileDeleted { tombstone };
                drop(idx);
                let _ = ctx.net.notify(notification).await;
                let _ = ctx
                    .ui
                    .send(SyncEvent::Log(format!("deleted {}", rel_path.display())))
                    .await;
            }
        }
    }
}

async fn reconcile(peer: PeerId, ctx: &Ctx) -> anyhow::Result<()> {
    let _ = ctx
        .ui
        .send(SyncEvent::Log(format!("reconciling with {peer}")))
        .await;

    let remote_hash = match ctx.net.request(peer, SyncRequest::GetRootHash).await? {
        SyncResponse::RootHash { hash } => hash,
        _ => return Err(anyhow::anyhow!("unexpected response")),
    };

    let local_hash = {
        let idx = ctx.index.lock().await;
        *idx.tree.hash()
    };

    if local_hash == remote_hash {
        let _ = ctx.ui.send(SyncEvent::Log("already in sync".into())).await;
        let _ = ctx
            .ui
            .send(SyncEvent::ReconciliationComplete {
                peer,
                files_synced: 0,
            })
            .await;
        return Ok(());
    }

    let remote_tree = match ctx.net.request(peer, SyncRequest::GetTree).await? {
        SyncResponse::Tree { tree } => tree,
        _ => return Err(anyhow::anyhow!("unexpected response")),
    };

    let local_tree = {
        let idx = ctx.index.lock().await;
        idx.tree.clone()
    };

    let diffs = merkle::diff(&local_tree, &remote_tree);
    let diff_count = diffs.len();
    let _ = ctx
        .ui
        .send(SyncEvent::Log(format!("found {diff_count} differences")))
        .await;

    let mut files_synced = 0usize;
    for diff_entry in diffs {
        match diff_entry {
            DiffEntry::Added(path, node) if !ctx.no_pull => {
                fetch_node_recursive(peer, &path, &node, ctx).await;
                files_synced += count_files(&node);
            }
            DiffEntry::Removed(path, _) => {
                debug!("local-only file: {}", path.display());
            }
            DiffEntry::Modified(path, _, node) if !ctx.no_pull => {
                fetch_node_recursive(peer, &path, &node, ctx).await;
                files_synced += count_files(&node);
            }
            _ => {}
        }
    }

    let _ = ctx
        .ui
        .send(SyncEvent::Log(format!(
            "reconciliation with {peer} complete ({files_synced} files)"
        )))
        .await;
    let _ = ctx
        .ui
        .send(SyncEvent::ReconciliationComplete { peer, files_synced })
        .await;
    Ok(())
}

/// Recursively fetch all files in a node (handles both files and directories).
async fn fetch_node_recursive(peer: PeerId, base_path: &Path, node: &Node, ctx: &Ctx) {
    match node {
        Node::File { chunks, .. } => {
            let chunk_indices: Vec<usize> = (0..chunks.len()).collect();
            let origin = RemoteOrigin {
                clock: VectorClock::new(),
                timestamp: now_secs(),
                peer: swarm::short_peer_id(&peer),
            };
            fetch_and_apply(peer, base_path, &chunk_indices, ctx, origin).await;
        }
        Node::Directory { children, .. } => {
            for (name, child) in children {
                let child_path = base_path.join(name);
                Box::pin(fetch_node_recursive(peer, &child_path, child, ctx)).await;
            }
        }
    }
}

fn count_files(node: &Node) -> usize {
    match node {
        Node::File { .. } => 1,
        Node::Directory { children, .. } => children.values().map(count_files).sum(),
    }
}

fn find_node<'a>(tree: &'a Node, path: &Path) -> Option<&'a Node> {
    let mut current = tree;
    for component in path.components() {
        if let std::path::Component::Normal(s) = component {
            let name = s.to_str()?;
            match current {
                Node::Directory { children, .. } => current = children.get(name)?,
                Node::File { .. } => return None,
            }
        }
    }
    Some(current)
}

fn now_secs() -> u64 {
    p2sync_core::util::now_secs()
}

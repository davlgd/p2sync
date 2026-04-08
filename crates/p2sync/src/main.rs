mod cli;
mod tui;

use std::io::IsTerminal;

use clap::Parser;
use libp2p::identity::Keypair;
use libp2p::{Multiaddr, PeerId};
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;

use p2sync_net::sync_engine::{SyncConfig, SyncEvent, TransferDirection};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = cli::Cli::parse();
    let use_tui = !cli.no_tui && std::io::stdout().is_terminal();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            let base = if use_tui { "warn" } else { "info" };
            // Suppress noisy Kademlia bootstrap warnings when no bootstrap nodes are configured
            EnvFilter::new(format!("{base},libp2p_kad=error"))
        }))
        .with_target(false)
        .init();

    match cli.command {
        cli::Commands::Share {
            path,
            exclude,
            readonly,
            port,
        } => {
            // share --readonly: don't accept incoming changes (no_pull)
            run_sync(path, exclude, false, readonly, port, None, use_tui).await?;
        }
        cli::Commands::Sync {
            peer,
            path,
            exclude,
            readonly,
            port,
        } => {
            // sync --readonly: don't push local changes (no_push)
            let peer = cli::resolve_peer_id(peer)?;
            run_sync(path, exclude, readonly, false, port, Some(peer), use_tui).await?;
        }
        cli::Commands::Fetch {
            peer,
            path,
            exclude,
            port,
        } => {
            let peer = cli::resolve_peer_id(peer)?;
            run_fetch(path, exclude, port, peer).await?;
        }
        cli::Commands::Relay { port } => {
            run_relay(port).await?;
        }
    }

    Ok(())
}

fn build_config(
    path: &std::path::Path,
    exclude: Vec<String>,
    no_push: bool,
    no_pull: bool,
    group_id: String,
    app_config: &p2sync_core::config::P2SyncConfig,
) -> SyncConfig {
    let mut exclude_patterns = exclude;
    for default in &app_config.default_excludes {
        if !exclude_patterns.iter().any(|p| p == default) {
            exclude_patterns.push(default.clone());
        }
    }

    SyncConfig {
        root_path: path.to_path_buf(),
        group_id,
        exclude_patterns,
        no_push,
        no_pull,
    }
}

async fn run_sync(
    path: std::path::PathBuf,
    exclude: Vec<String>,
    no_push: bool,
    no_pull: bool,
    port: u16,
    dial_peer: Option<PeerId>,
    use_tui: bool,
) -> anyhow::Result<()> {
    let path = path.canonicalize().unwrap_or(path);
    if !path.is_dir() {
        anyhow::bail!("{} is not a directory", path.display());
    }

    let app_config = p2sync_core::config::load(&path);
    let keypair = Keypair::generate_ed25519();
    let local_peer_id = keypair.public().to_peer_id();
    let group_id = dial_peer
        .map(|p: PeerId| p.to_string())
        .unwrap_or_else(|| local_peer_id.to_string());

    eprintln!("Local peer: {local_peer_id}");
    if let Some(remote) = dial_peer {
        eprintln!("Remote peer: {remote}");
    }

    let listen_addr: Multiaddr = format!("/ip4/0.0.0.0/tcp/{port}").parse()?;
    let config = build_config(
        &path,
        exclude,
        no_push,
        no_pull,
        group_id.clone(),
        &app_config,
    );
    let tui_config = app_config.tui.clone();
    let (ui_tx, ui_rx) = mpsc::channel::<SyncEvent>(app_config.network.channel_capacity);

    let sync_handle = tokio::spawn(async move {
        if let Err(e) =
            p2sync_net::sync_engine::run(keypair, config, app_config, listen_addr, ui_tx).await
        {
            eprintln!("sync engine error: {e}");
        }
    });

    if use_tui {
        let app_state = tui::AppState::new(
            local_peer_id.to_string(),
            group_id,
            path.to_string_lossy().into_owned(),
            tui_config,
        );
        tui::run_tui(app_state, ui_rx).await?;
    } else {
        run_headless(ui_rx).await;
    }

    sync_handle.abort();
    Ok(())
}

async fn run_fetch(
    path: std::path::PathBuf,
    exclude: Vec<String>,
    port: u16,
    peer: PeerId,
) -> anyhow::Result<()> {
    let path = path.canonicalize().unwrap_or(path);
    if !path.exists() {
        std::fs::create_dir_all(&path)?;
    }
    if !path.is_dir() {
        anyhow::bail!("{} is not a directory", path.display());
    }

    let app_config = p2sync_core::config::load(&path);
    let keypair = Keypair::generate_ed25519();
    let local_peer_id = keypair.public().to_peer_id();
    let group_id = peer.to_string();

    eprintln!("Local peer:  {local_peer_id}");
    eprintln!("Remote peer: {peer}");

    let listen_addr: Multiaddr = format!("/ip4/0.0.0.0/tcp/{port}").parse()?;
    // fetch: download only, don't push local changes
    let config = build_config(&path, exclude, true, false, group_id, &app_config);
    let (ui_tx, mut ui_rx) = mpsc::channel::<SyncEvent>(app_config.network.channel_capacity);

    let sync_handle = tokio::spawn(async move {
        if let Err(e) =
            p2sync_net::sync_engine::run(keypair, config, app_config, listen_addr, ui_tx).await
        {
            eprintln!("sync engine error: {e}");
        }
    });

    // Simple progress display: wait for reconciliation to complete, then exit
    let mut files_downloaded = 0usize;
    let mut reconciliation_done = false;
    let mut target_connected = false;

    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    // Timeout: 60s to connect to peer
    let timeout = tokio::time::sleep(std::time::Duration::from_secs(60));
    tokio::pin!(timeout);

    loop {
        tokio::select! {
            Some(event) = ui_rx.recv() => {
                match event {
                    SyncEvent::Listening(addr) => {
                        eprintln!("  listening on {addr}");
                    }
                    SyncEvent::PeerConnected(p) if p == peer => {
                        target_connected = true;
                        eprintln!("  connected, syncing...");
                    }
                    SyncEvent::TransferComplete { path, direction: TransferDirection::Download } => {
                        files_downloaded += 1;
                        eprint!("\r\x1b[2K  {files_downloaded} files  (latest: {})", path.display());
                    }
                    SyncEvent::ReconciliationComplete { peer: p, files_synced } if p == peer => {
                        reconciliation_done = true;
                        if files_downloaded > 0 {
                            eprintln!();
                        }
                        eprintln!("  done: {files_synced} files fetched");
                        break;
                    }
                    SyncEvent::Log(msg) if msg.contains("failed") && target_connected => {
                        eprintln!("  error: {msg}");
                        break;
                    }
                    _ => {}
                }
            }
            _ = &mut timeout => {
                eprintln!("\n  timeout: could not connect to peer within 60s");
                break;
            }
            _ = &mut ctrl_c => {
                eprintln!("\n  interrupted");
                break;
            }
        }
    }

    sync_handle.abort();

    if reconciliation_done {
        Ok(())
    } else {
        anyhow::bail!("fetch did not complete")
    }
}

async fn run_headless(mut event_rx: mpsc::Receiver<SyncEvent>) {
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    loop {
        tokio::select! {
            Some(event) = event_rx.recv() => {
                match event {
                    SyncEvent::IndexReady { file_count, total_size } => {
                        eprintln!("{file_count} files indexed ({total_size} bytes)");
                    }
                    SyncEvent::PeerConnected(peer) => {
                        eprintln!("peer connected: {peer}");
                    }
                    SyncEvent::PeerDisconnected(peer) => {
                        eprintln!("peer disconnected: {peer}");
                    }
                    SyncEvent::Listening(addr) => {
                        eprintln!("listening on {addr}");
                    }
                    SyncEvent::TransferComplete { path, direction } => {
                        eprintln!("{direction:?} complete: {}", path.display());
                    }
                    SyncEvent::Conflict { path, conflict_path } => {
                        eprintln!("CONFLICT: {} -> {}", path.display(), conflict_path.display());
                    }
                    SyncEvent::RemoteDelete { path } => {
                        eprintln!("synced deletion {}", path.display());
                    }
                    SyncEvent::ReconciliationComplete { peer, files_synced } => {
                        eprintln!("reconciliation with {peer} complete ({files_synced} files)");
                    }
                    SyncEvent::Log(msg) => {
                        eprintln!("{msg}");
                    }
                    _ => {}
                }
            }
            _ = &mut ctrl_c => {
                eprintln!("shutting down...");
                return;
            }
        }
    }
}

async fn run_relay(port: u16) -> anyhow::Result<()> {
    use libp2p::futures::StreamExt;
    use libp2p::swarm::SwarmEvent;
    use libp2p::{identify, noise, relay, yamux};

    let keypair = Keypair::generate_ed25519();
    let local_peer_id = keypair.public().to_peer_id();

    let mut swarm = libp2p::SwarmBuilder::with_existing_identity(keypair)
        .with_tokio()
        .with_tcp(
            libp2p::tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_behaviour(|key| {
            Ok(RelayServerBehaviour {
                relay: relay::Behaviour::new(key.public().to_peer_id(), Default::default()),
                identify: identify::Behaviour::new(identify::Config::new(
                    "/p2sync-relay/0.1.0".to_string(),
                    key.public(),
                )),
            })
        })?
        .with_swarm_config(|c| c.with_idle_connection_timeout(std::time::Duration::from_secs(600)))
        .build();

    let listen_addr: Multiaddr = format!("/ip4/0.0.0.0/tcp/{port}").parse()?;
    swarm.listen_on(listen_addr)?;

    eprintln!("Relay node: {local_peer_id}");
    eprintln!("Waiting for listen addresses...");

    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    loop {
        tokio::select! {
            event = swarm.select_next_some() => {
                match event {
                    SwarmEvent::NewListenAddr { address, .. } => {
                        let full_addr = format!("{address}/p2p/{local_peer_id}");
                        eprintln!("Relay listening on: {full_addr}");
                        eprintln!("  Add to .p2sync.toml:");
                        eprintln!("  [network]");
                        eprintln!("  relay = \"{full_addr}\"");
                    }
                    SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                        eprintln!("peer connected: {peer_id}");
                    }
                    SwarmEvent::ConnectionClosed { peer_id, .. } => {
                        eprintln!("peer disconnected: {peer_id}");
                    }
                    _ => {}
                }
            }
            _ = &mut ctrl_c => {
                eprintln!("relay shutting down...");
                return Ok(());
            }
        }
    }
}

#[derive(libp2p::swarm::NetworkBehaviour)]
struct RelayServerBehaviour {
    relay: libp2p::relay::Behaviour,
    identify: libp2p::identify::Behaviour,
}

use std::io::IsTerminal;
use std::path::PathBuf;

use clap::{Parser, Subcommand};

const PEER_ENV_VAR: &str = "P2SYNC_PEER_ID";

#[derive(Parser)]
#[command(
    name = "p2sync",
    version,
    about = "P2P real-time folder synchronization"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    /// Disable TUI, log to stderr instead
    #[arg(long, global = true)]
    pub no_tui: bool,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Create a sync group and display the Peer ID to share
    Share {
        /// Path to the folder to sync
        path: PathBuf,

        /// Exclude files/directories matching these patterns (substring match, repeatable)
        #[arg(long, short = 'e')]
        exclude: Vec<String>,

        /// Reject incoming changes from peers (outbound sharing only)
        #[arg(long)]
        readonly: bool,

        /// Port to listen on (0 = random)
        #[arg(long, short = 'p', default_value = "0")]
        port: u16,
    },

    /// Join a sync group and sync continuously (bidirectional)
    Sync {
        /// Path to sync into
        path: PathBuf,

        /// Peer ID from 'p2sync share' output (or set P2SYNC_PEER_ID env var, or omit for interactive prompt)
        #[arg(long, env = PEER_ENV_VAR)]
        peer: Option<String>,

        /// Exclude files/directories matching these patterns (substring match, repeatable)
        #[arg(long, short = 'e')]
        exclude: Vec<String>,

        /// Receive only: don't push local changes to peers
        #[arg(long)]
        readonly: bool,

        /// Port to listen on (0 = random)
        #[arg(long, short = 'p', default_value = "0")]
        port: u16,
    },

    /// Download from a sync group, then exit
    Fetch {
        /// Path to download into (created if it doesn't exist)
        path: PathBuf,

        /// Peer ID from 'p2sync share' output (or set P2SYNC_PEER_ID env var, or omit for interactive prompt)
        #[arg(long, env = PEER_ENV_VAR)]
        peer: Option<String>,

        /// Exclude files/directories matching these patterns (substring match, repeatable)
        #[arg(long, short = 'e')]
        exclude: Vec<String>,

        /// Port to listen on (0 = random)
        #[arg(long, short = 'p', default_value = "0")]
        port: u16,
    },

    /// Run a relay node for peers behind NAT (no sync, just routing)
    Relay {
        /// Port to listen on (default: 4001)
        #[arg(long, short = 'p', default_value = "4001")]
        port: u16,
    },
}

/// Resolve peer ID from: --peer flag / P2SYNC_PEER_ID env var (handled by clap) > interactive prompt.
pub fn resolve_peer_id(explicit: Option<String>) -> anyhow::Result<libp2p::PeerId> {
    let raw = if let Some(id) = explicit {
        id
    } else if std::io::stdin().is_terminal() {
        eprintln!("Tip: the PeerId is shown when running 'p2sync share <path>'");
        let input: String = dialoguer::Password::new()
            .with_prompt("Peer ID")
            .interact()?;
        input
    } else {
        anyhow::bail!(
            "no peer ID provided.\n\
             Run 'p2sync share <path>' on the source machine to get a PeerId, then:\n  \
             - pass it with --peer <PEER_ID>\n  \
             - or set {PEER_ENV_VAR} environment variable\n  \
             - or run interactively (TTY required)"
        );
    };

    let trimmed = raw.trim();
    trimmed.parse().map_err(|_| {
        anyhow::anyhow!(
            "invalid peer ID format.\n\
             A PeerId looks like: 12D3KooWRk... (base58-encoded, starts with '12D3Koo')\n\
             Got: '{}'",
            p2sync_core::util::truncate_end(trimmed, 23)
        )
    })
}

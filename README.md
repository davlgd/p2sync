# p2sync

P2P real-time folder synchronization, powered by [libp2p](https://libp2p.io/).

Sync directories between machines — bidirectionally, in real-time, without any central server. All traffic is end-to-end encrypted. No cloud, no account, no sign-up.

## Features

- **Direct P2P sync** — files transfer directly between machines, no intermediary
- **Real-time** — changes propagate instantly via filesystem watching
- **Bidirectional** — any peer can modify, changes sync everywhere
- **Multi-peer** — sync across N machines simultaneously
- **Encrypted** — all connections use Noise protocol (E2E encryption)
- **LAN discovery** — mDNS for automatic local network discovery
- **Efficient** — Merkle tree diffing, configurable chunking, only modified chunks transfer
- **Conflict resolution** — vector clocks with automatic conflict detection
- **TUI dashboard** — real-time view of peers, transfers, and events
- **Configurable** — `.p2sync.toml` for chunk size, timeouts, excludes, and more

## Quick start

```bash
# Build from source
cargo build --release

# Machine A — share a folder (displays a PeerId to share)
p2sync share ./my-project

# Machine B — sync continuously (bidirectional)
p2sync sync ./my-project --peer <PEER_ID>

# Machine B — or just fetch a snapshot (one-shot download, then exit)
p2sync fetch ./backup --peer <PEER_ID>
```

The PeerId can also be set via the `P2SYNC_PEER_ID` environment variable, or entered interactively when omitted.

## How it works

```
Machine A                                Machine B
┌──────────┐                          ┌──────────┐
│ Folder   │                          │ Folder   │
│    ↕     │                          │    ↕     │
│ Watcher  │                          │ Watcher  │
│    ↕     │                          │    ↕     │
│ Merkle   │                          │ Merkle   │
│  Tree    │                          │  Tree    │
│    ↕     │                          │    ↕     │
│ libp2p   │◄─── TCP+Noise+Yamux ────►│ libp2p   │
│  swarm   │   (direct connection)    │  swarm   │
└──────────┘                          └──────────┘
```

1. Each peer indexes its folder into a Merkle tree (configurable chunk size, Blake3 hashes)
2. Peers discover each other via mDNS on the local network
3. They compare Merkle tree roots — if different, they walk the tree to find divergent chunks
4. Only modified chunks are transferred, not entire files
5. A filesystem watcher detects local changes and notifies all peers via GossipSub
6. Conflicts (concurrent edits) are detected via vector clocks and resolved with backup copies

## CLI reference

```
p2sync share <path>                     Create a sync group and display the Peer ID to share
  -e, --exclude <pattern>               Exclude files/directories matching pattern (repeatable)
  --readonly                             Reject incoming changes from peers (outbound sharing only)
  -p, --port <port>                      Port to listen on (0 = random)

p2sync sync <path> --peer <id>          Join a sync group and sync continuously (bidirectional)
  -e, --exclude <pattern>               Exclude files/directories matching pattern (repeatable)
  --readonly                             Receive only: don't push local changes to peers
  -p, --port <port>                      Port to listen on (0 = random)

p2sync fetch <path> --peer <id>         Download from a sync group, then exit
  -e, --exclude <pattern>               Exclude files/directories matching pattern (repeatable)
  -p, --port <port>                      Port to listen on (0 = random)

Global options:
  --no-tui                               Disable TUI, log to stderr instead

The `--peer` flag accepts a Peer ID from `p2sync share` output. It can also be set
via the `P2SYNC_PEER_ID` environment variable, or entered interactively when omitted.
```

## Configuration

Place a `.p2sync.toml` in the sync root to customize behavior:

```toml
# Files/directories always excluded from sync
default_excludes = [".git", ".DS_Store", ".p2sync.toml"]

[storage]
chunk_size = 262144        # 256 KB
max_chunk_index = 16384    # Max chunk index per file
max_file_size = 1073741824 # 1 GB max receivable file size
tombstone_ttl_secs = 3600  # 1 hour

[network]
max_request_size = 1048576       # 1 MiB
max_response_size = 104857600    # 100 MiB
request_timeout_secs = 120
idle_connection_timeout_secs = 300
gossipsub_heartbeat_secs = 5
channel_capacity = 256

[watcher]
poll_interval_ms = 1000
write_guard_ttl_secs = 3
debounce_ms = 500          # Ignore duplicate fs events within this window

[tui]
max_log_lines = 100
refresh_interval_ms = 50
```

All values are optional — defaults are used for anything not specified.

## Architecture

The project is organized as a Cargo workspace:

- **`p2sync-core`** — Chunking, Merkle tree, file indexing, conflict resolution, filesystem watcher, configuration
- **`p2sync-net`** — libp2p networking: swarm, protocols, peer discovery, data transfer, sync engine
- **`p2sync`** — CLI, TUI, and headless mode

## License

Apache 2.0 — see [LICENSE](LICENSE).

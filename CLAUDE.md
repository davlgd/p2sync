# p2sync

## Project Overview

P2P real-time folder synchronization tool built on libp2p. Syncs directories between N machines, bidirectionally, over LAN or WAN, with files of any size, in real-time, without any central server.

- **Repository**: github.com/davlgd/p2sync
- **License**: Apache 2.0 (davlgd, 2026)
- **Version**: 0.2.0
- **Rust edition**: 2024
- **Minimum Rust version**: 1.90.0

## Architecture

Workspace with 3 crates:

- `p2sync-core`: Configuration, Merkle tree, chunking (configurable, default 256 KB, Blake3), file indexing, conflict resolution (vector clocks), tombstones, shared utilities
- `p2sync-net`: libp2p swarm (TCP + DNS + Noise + Yamux + Relay), mDNS, Kademlia, GossipSub, Request-Response, sync engine
- `p2sync` (binary): CLI (clap) + TUI (ratatui/crossterm) + headless mode + relay server, glues core + net

## Key Design Decisions

- Chunk size configurable via `.p2sync.toml` (default 256 KB), hashed with Blake3
- Merkle tree covers the full directory recursively
- Sync is bidirectional and multi-peer
- Conflicts use vector clocks; last-writer-wins with `.conflict.<short_peer_id>` backup
- Deletions propagated via tombstones with configurable TTL
- GossipSub for change notifications, Request-Response for data transfer
- Group isolation via GossipSub topic matching (peers only sync within their group)
- Discovery: mDNS (LAN, default) + Kademlia DHT (WAN, opt-in via `discovery = "wan"`)
- Relay: none (default), explicit multiaddr, or auto (third-party, with warning)
- `no_push` / `no_pull` flags control sync direction (share --readonly = no_pull, sync --readonly = no_push)
- Path traversal protection via `safe_path()`, symlink skipping, max file size limit
- Default excludes: `.git`, `.DS_Store`, `.p2sync.toml`
- EventGuard for write suppression and change debouncing

## Commands

```bash
# Build
cargo build

# Test
cargo test --workspace

# Run
cargo run -- share ./some-folder
cargo run -- sync ./some-folder --peer <peer_id>
cargo run -- fetch ./some-folder --peer <peer_id>
cargo run -- relay -p 4001

# Clippy
cargo clippy --workspace -- -D warnings

# Format
cargo fmt --all

# Publish (requires Rust 1.90+)
cargo publish --workspace
```

## Code Review Checklist

- [ ] DRY: no duplicated logic (check util.rs for shared helpers)
- [ ] KISS: simplest solution that works
- [ ] SOLID: single responsibility, open/closed, etc.
- [ ] Idiomatic Rust: proper error handling, no unwrap in lib code
- [ ] Security: input validation, no path traversal, no unbounded allocations, max file size enforced
- [ ] UX/DX: clear error messages, intuitive CLI, responsive TUI
- [ ] Tests: meaningful coverage, real assertions, no trivial tests

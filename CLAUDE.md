# p2sync

## Project Overview

P2P real-time folder synchronization tool built on libp2p. Syncs directories between N machines, bidirectionally, over LAN, with files of any size, in real-time, without any central server.

- **Repository**: github.com/davlgd/p2sync
- **License**: Apache 2.0 (davlgd, 2026)
- **Version**: 0.1.0 (pre-release, no tags yet)
- **Rust edition**: 2024

## Architecture

Workspace with 3 crates:

- `p2sync-core`: Configuration, Merkle tree, chunking (configurable, default 256 KB, Blake3), file indexing, conflict resolution (vector clocks), tombstones
- `p2sync-net`: libp2p swarm (TCP + Noise + Yamux), mDNS, Kademlia, GossipSub, Request-Response, sync engine
- `p2sync` (binary): CLI (clap) + TUI (ratatui/crossterm) + headless mode, glues core + net

## Key Design Decisions

- Chunk size configurable via `.p2sync.toml` (default 256 KB), hashed with Blake3
- Merkle tree covers the full directory recursively
- Sync is bidirectional and multi-peer
- Conflicts use vector clocks; last-writer-wins with `.conflict.<short_peer_id>` backup
- Deletions propagated via tombstones with configurable TTL
- GossipSub for change notifications, Request-Response for data transfer
- mDNS for LAN discovery, Kademlia DHT for WAN
- `no_push` / `no_pull` flags control sync direction (share --readonly = no_pull, sync --readonly = no_push)
- Path traversal protection via `safe_path()`, symlink skipping, max file size limit
- Default excludes: `.git`, `.DS_Store`, `.p2sync.toml`

## Development Principles

- **TDD**: Write tests first, then implement until tests pass
- **DRY/KISS/SOLID**: No premature abstractions, no duplication, single responsibility per module
- **Idiomatic Rust 2024**: Use current idioms, leverage the type system, prefer `Result` over panics
- **Dependencies**: Always use latest stable versions
- **Security**: No unsafe code. All network data validated. No unwrap in library code.

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

# Clippy
cargo clippy --workspace -- -D warnings

# Format
cargo fmt --all
```

## Code Review Checklist

- [ ] DRY: no duplicated logic
- [ ] KISS: simplest solution that works
- [ ] SOLID: single responsibility, open/closed, etc.
- [ ] Idiomatic Rust: proper error handling, no unwrap in lib code
- [ ] Security: input validation, no path traversal, no unbounded allocations, max file size enforced
- [ ] UX/DX: clear error messages, intuitive CLI, responsive TUI
- [ ] Tests: meaningful coverage, real assertions, no trivial tests

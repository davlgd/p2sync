# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.0] - 2026-04-08

### Added

- WAN discovery via Kademlia DHT with IPFS bootstrap nodes (`discovery = "wan"`)
- Relay support for peers behind NAT (`relay` config: `none`, explicit multiaddr, or `auto`)
- `p2sync relay` command to run a dedicated relay node
- DNS transport for resolving dnsaddr multiaddrs

### Changed

- Minimum Rust version bumped to 1.90.0 (for `cargo publish --workspace`)
- Default discovery mode is `lan` (unchanged behavior from 0.1.0)

## [0.1.0] - 2026-04-08

Initial public release. P2P real-time folder synchronization over libp2p.

See [README.md](README.md) for features and usage.

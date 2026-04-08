use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use libp2p::StreamProtocol;
use libp2p::gossipsub;
use libp2p::kad;
use libp2p::mdns;
use libp2p::relay;
use libp2p::request_response;
use libp2p::swarm::NetworkBehaviour;

use p2sync_core::config::NetworkConfig;

use crate::protocol::{SyncRequest, SyncResponse};

/// The composed network behaviour for p2sync.
#[derive(NetworkBehaviour)]
pub struct SyncBehaviour {
    pub request_response: request_response::cbor::Behaviour<SyncRequest, SyncResponse>,
    pub gossipsub: gossipsub::Behaviour,
    pub mdns: mdns::tokio::Behaviour,
    pub kademlia: kad::Behaviour<kad::store::MemoryStore>,
    pub relay_client: relay::client::Behaviour,
}

/// Build the sync behaviour from a libp2p keypair and network config.
pub fn build(
    key: &libp2p::identity::Keypair,
    net_config: &NetworkConfig,
    relay_behaviour: relay::client::Behaviour,
) -> anyhow::Result<SyncBehaviour> {
    let peer_id = key.public().to_peer_id();

    // Request-Response: CBOR-encoded sync protocol
    let codec = request_response::cbor::codec::Codec::<SyncRequest, SyncResponse>::default()
        .set_request_size_maximum(net_config.max_request_size)
        .set_response_size_maximum(net_config.max_response_size);
    let request_response = request_response::Behaviour::with_codec(
        codec,
        [(
            StreamProtocol::new("/p2sync/sync/1"),
            request_response::ProtocolSupport::Full,
        )],
        request_response::Config::default().with_request_timeout(net_config.request_timeout()),
    );

    // GossipSub: change notifications
    let message_id_fn = |message: &gossipsub::Message| {
        let mut hasher = DefaultHasher::new();
        message.data.hash(&mut hasher);
        message.source.hash(&mut hasher);
        gossipsub::MessageId::from(hasher.finish().to_string())
    };

    let gossipsub_config = gossipsub::ConfigBuilder::default()
        .heartbeat_interval(net_config.gossipsub_heartbeat())
        .validation_mode(gossipsub::ValidationMode::Strict)
        .message_id_fn(message_id_fn)
        .build()
        .map_err(|e| anyhow::anyhow!("gossipsub config error: {e}"))?;

    let gossipsub = gossipsub::Behaviour::new(
        gossipsub::MessageAuthenticity::Signed(key.clone()),
        gossipsub_config,
    )
    .map_err(|e| anyhow::anyhow!("gossipsub error: {e}"))?;

    // mDNS: local discovery
    let mdns = mdns::tokio::Behaviour::new(mdns::Config::default(), peer_id)?;

    // Kademlia: WAN discovery
    let mut kademlia = kad::Behaviour::new(peer_id, kad::store::MemoryStore::new(peer_id));
    if net_config.is_wan() {
        kademlia.set_mode(Some(kad::Mode::Server));
    } else {
        // LAN mode: disable periodic bootstrap to avoid warnings
        kademlia.set_mode(Some(kad::Mode::Client));
    }

    Ok(SyncBehaviour {
        request_response,
        gossipsub,
        mdns,
        kademlia,
        relay_client: relay_behaviour,
    })
}

/// IPFS bootstrap nodes resolved via dnsaddr.
pub const IPFS_BOOTSTRAP_NODES: &[&str] = &[
    "/dnsaddr/bootstrap.libp2p.io/p2p/QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN",
    "/dnsaddr/bootstrap.libp2p.io/p2p/QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa",
    "/dnsaddr/bootstrap.libp2p.io/p2p/QmbLHAnMoJPWSCR5Zhtx6BHJX9KiKNN6tpvbUcqanj75Nb",
    "/dnsaddr/bootstrap.libp2p.io/p2p/QmcZf59bWwK5XFi76CZX8cbJ4BhTzzA3gU1ZjYZcYW3dwt",
];

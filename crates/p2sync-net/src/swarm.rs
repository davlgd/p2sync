use futures::StreamExt;
use libp2p::swarm::SwarmEvent;
use libp2p::{Multiaddr, PeerId, Swarm, gossipsub, kad, mdns, request_response};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::behaviour::{self, SyncBehaviour, SyncBehaviourEvent};
use crate::protocol::{self, SyncNotification, SyncRequest, SyncResponse};
use p2sync_core::conflict;

/// Commands sent from the application to the network event loop.
#[derive(Debug)]
pub enum Command {
    /// Start listening on a multiaddr.
    Listen(Multiaddr),

    /// Dial a specific peer.
    Dial(PeerId),

    /// Dial a multiaddr directly.
    DialAddr(Multiaddr),

    /// Add a Kademlia bootstrap address and trigger bootstrap.
    AddKadAddress(PeerId, Multiaddr),

    /// Trigger Kademlia bootstrap.
    Bootstrap,

    /// Search for a peer via Kademlia DHT.
    SearchPeer(PeerId),

    /// Subscribe to a sync group topic.
    Subscribe(String),

    /// Send a sync request to a peer.
    SendRequest {
        peer: PeerId,
        request: SyncRequest,
        reply: tokio::sync::oneshot::Sender<anyhow::Result<SyncResponse>>,
    },

    /// Broadcast a notification via GossipSub.
    Notify(SyncNotification),

    /// Respond to an inbound request.
    Respond {
        channel: request_response::ResponseChannel<SyncResponse>,
        response: SyncResponse,
    },

    /// Shutdown the event loop.
    Shutdown,
}

/// Events emitted from the network to the application.
#[derive(Debug)]
pub enum Event {
    /// A new peer was discovered.
    PeerDiscovered(PeerId),

    /// A peer disconnected.
    PeerDisconnected(PeerId),

    /// We started listening on an address.
    Listening(Multiaddr),

    /// An inbound sync request from a peer.
    InboundRequest {
        peer: PeerId,
        request: SyncRequest,
        channel: request_response::ResponseChannel<SyncResponse>,
    },

    /// A GossipSub notification from a peer.
    Notification {
        source: PeerId,
        notification: SyncNotification,
    },
}

/// The network event loop, driving the libp2p swarm.
pub struct NetworkLoop {
    swarm: Swarm<SyncBehaviour>,
    command_rx: mpsc::Receiver<Command>,
    event_tx: mpsc::Sender<Event>,
    pending_requests: std::collections::HashMap<
        request_response::OutboundRequestId,
        tokio::sync::oneshot::Sender<anyhow::Result<SyncResponse>>,
    >,
    topic: Option<gossipsub::IdentTopic>,
}

/// Handle for sending commands to the network loop.
#[derive(Clone)]
pub struct NetworkHandle {
    command_tx: mpsc::Sender<Command>,
}

impl NetworkHandle {
    /// Start listening on a multiaddr.
    pub async fn listen(&self, addr: Multiaddr) -> anyhow::Result<()> {
        self.command_tx.send(Command::Listen(addr)).await?;
        Ok(())
    }

    /// Dial a peer by ID.
    pub async fn dial(&self, peer: PeerId) -> anyhow::Result<()> {
        self.command_tx.send(Command::Dial(peer)).await?;
        Ok(())
    }

    /// Dial a multiaddr directly.
    pub async fn dial_addr(&self, addr: Multiaddr) -> anyhow::Result<()> {
        self.command_tx.send(Command::DialAddr(addr)).await?;
        Ok(())
    }

    /// Add a Kademlia bootstrap node address.
    pub async fn add_kad_address(&self, peer: PeerId, addr: Multiaddr) -> anyhow::Result<()> {
        self.command_tx
            .send(Command::AddKadAddress(peer, addr))
            .await?;
        Ok(())
    }

    /// Trigger Kademlia bootstrap.
    pub async fn bootstrap(&self) -> anyhow::Result<()> {
        self.command_tx.send(Command::Bootstrap).await?;
        Ok(())
    }

    /// Search for a peer via Kademlia DHT.
    pub async fn search_peer(&self, peer: PeerId) -> anyhow::Result<()> {
        self.command_tx.send(Command::SearchPeer(peer)).await?;
        Ok(())
    }

    /// Subscribe to a sync group.
    pub async fn subscribe(&self, group_id: &str) -> anyhow::Result<()> {
        self.command_tx
            .send(Command::Subscribe(group_id.to_string()))
            .await?;
        Ok(())
    }

    /// Send a request and wait for the response.
    pub async fn request(
        &self,
        peer: PeerId,
        request: SyncRequest,
    ) -> anyhow::Result<SyncResponse> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.command_tx
            .send(Command::SendRequest {
                peer,
                request,
                reply: tx,
            })
            .await?;
        rx.await?
    }

    /// Broadcast a notification.
    pub async fn notify(&self, notification: SyncNotification) -> anyhow::Result<()> {
        self.command_tx.send(Command::Notify(notification)).await?;
        Ok(())
    }

    /// Respond to an inbound request.
    pub async fn respond(
        &self,
        channel: request_response::ResponseChannel<SyncResponse>,
        response: SyncResponse,
    ) -> anyhow::Result<()> {
        self.command_tx
            .send(Command::Respond { channel, response })
            .await?;
        Ok(())
    }

    /// Shutdown the network loop.
    pub async fn shutdown(&self) -> anyhow::Result<()> {
        self.command_tx.send(Command::Shutdown).await?;
        Ok(())
    }
}

/// Create the swarm and return the network loop + handle + event receiver.
pub fn build(
    keypair: libp2p::identity::Keypair,
    net_config: &p2sync_core::config::NetworkConfig,
) -> anyhow::Result<(NetworkLoop, NetworkHandle, mpsc::Receiver<Event>)> {
    let idle_timeout = net_config.idle_connection_timeout();
    let channel_capacity = net_config.channel_capacity;
    let net_config_clone = net_config.clone();

    let swarm = libp2p::SwarmBuilder::with_existing_identity(keypair)
        .with_tokio()
        .with_tcp(
            libp2p::tcp::Config::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )?
        .with_dns()?
        .with_relay_client(libp2p::noise::Config::new, libp2p::yamux::Config::default)?
        .with_behaviour(|key, relay_behaviour| {
            behaviour::build(key, &net_config_clone, relay_behaviour)
                .map_err(|e| Box::<dyn std::error::Error + Send + Sync>::from(e.to_string()))
        })?
        .with_swarm_config(|c| c.with_idle_connection_timeout(idle_timeout))
        .build();

    let (command_tx, command_rx) = mpsc::channel(channel_capacity);
    let (event_tx, event_rx) = mpsc::channel(channel_capacity);

    let network_loop = NetworkLoop {
        swarm,
        command_rx,
        event_tx,
        pending_requests: Default::default(),
        topic: None,
    };

    let handle = NetworkHandle { command_tx };

    Ok((network_loop, handle, event_rx))
}

impl NetworkLoop {
    /// Run the event loop until shutdown.
    pub async fn run(mut self) {
        loop {
            tokio::select! {
                event = self.swarm.select_next_some() => {
                    self.handle_swarm_event(event).await;
                }
                command = self.command_rx.recv() => {
                    match command {
                        Some(Command::Shutdown) | None => {
                            info!("network loop shutting down");
                            return;
                        }
                        Some(cmd) => self.handle_command(cmd).await,
                    }
                }
            }
        }
    }

    async fn handle_command(&mut self, cmd: Command) {
        match cmd {
            Command::Listen(addr) => match self.swarm.listen_on(addr) {
                Ok(_) => {}
                Err(e) => warn!("listen error: {e}"),
            },
            Command::Dial(peer) => match self.swarm.dial(peer) {
                Ok(_) => debug!("dialing {peer}"),
                Err(e) => warn!("dial error: {e}"),
            },
            Command::DialAddr(addr) => {
                if let Err(e) = self.swarm.dial(addr) {
                    warn!("dial addr error: {e}");
                }
            }
            Command::AddKadAddress(peer, addr) => {
                self.swarm.behaviour_mut().kademlia.add_address(&peer, addr);
            }
            Command::Bootstrap => match self.swarm.behaviour_mut().kademlia.bootstrap() {
                Ok(_) => info!("Kademlia bootstrap started"),
                Err(e) => debug!("Kademlia bootstrap skipped: {e}"),
            },
            Command::SearchPeer(peer_id) => {
                debug!("searching DHT for peer {peer_id}");
                self.swarm
                    .behaviour_mut()
                    .kademlia
                    .get_closest_peers(peer_id);
            }
            Command::Subscribe(group_id) => {
                let topic_name = protocol::sync_topic(&group_id);
                let topic = gossipsub::IdentTopic::new(&topic_name);
                match self.swarm.behaviour_mut().gossipsub.subscribe(&topic) {
                    Ok(_) => {
                        info!("subscribed to {topic_name}");
                        self.topic = Some(topic);
                    }
                    Err(e) => warn!("subscribe error: {e}"),
                }
            }
            Command::SendRequest {
                peer,
                request,
                reply,
            } => {
                let req_id = self
                    .swarm
                    .behaviour_mut()
                    .request_response
                    .send_request(&peer, request);
                self.pending_requests.insert(req_id, reply);
            }
            Command::Notify(notification) => {
                if let Some(topic) = &self.topic {
                    match serde_json::to_vec(&notification) {
                        Ok(data) => {
                            if let Err(e) = self
                                .swarm
                                .behaviour_mut()
                                .gossipsub
                                .publish(topic.clone(), data)
                            {
                                debug!("gossipsub publish error: {e}");
                            }
                        }
                        Err(e) => warn!("serialize notification error: {e}"),
                    }
                }
            }
            Command::Respond { channel, response } => {
                if let Err(resp) = self
                    .swarm
                    .behaviour_mut()
                    .request_response
                    .send_response(channel, response)
                {
                    warn!("send response error (channel closed): {resp:?}");
                }
            }
            Command::Shutdown => unreachable!(),
        }
    }

    async fn handle_swarm_event(&mut self, event: SwarmEvent<SyncBehaviourEvent>) {
        match event {
            // mDNS discovery
            SwarmEvent::Behaviour(SyncBehaviourEvent::Mdns(mdns::Event::Discovered(peers))) => {
                for (peer_id, addr) in peers {
                    debug!("mDNS discovered: {peer_id} at {addr}");
                    self.swarm
                        .behaviour_mut()
                        .gossipsub
                        .add_explicit_peer(&peer_id);
                    self.swarm
                        .behaviour_mut()
                        .kademlia
                        .add_address(&peer_id, addr.clone());
                    // Dial the peer to establish a connection
                    let _ = self.swarm.dial(addr);
                }
            }
            SwarmEvent::Behaviour(SyncBehaviourEvent::Mdns(mdns::Event::Expired(peers))) => {
                for (peer_id, _) in peers {
                    debug!("mDNS expired: {peer_id}");
                    self.swarm
                        .behaviour_mut()
                        .gossipsub
                        .remove_explicit_peer(&peer_id);
                }
            }

            // Request-Response
            SwarmEvent::Behaviour(SyncBehaviourEvent::RequestResponse(
                request_response::Event::Message { peer, message, .. },
            )) => match message {
                request_response::Message::Request {
                    request, channel, ..
                } => {
                    debug!("inbound request from {peer}: {request:?}");
                    let _ = self
                        .event_tx
                        .send(Event::InboundRequest {
                            peer,
                            request,
                            channel,
                        })
                        .await;
                }
                request_response::Message::Response {
                    request_id,
                    response,
                } => {
                    if let Some(reply) = self.pending_requests.remove(&request_id) {
                        let _ = reply.send(Ok(response));
                    }
                }
            },
            SwarmEvent::Behaviour(SyncBehaviourEvent::RequestResponse(
                request_response::Event::OutboundFailure {
                    request_id, error, ..
                },
            )) => {
                if let Some(reply) = self.pending_requests.remove(&request_id) {
                    let _ = reply.send(Err(anyhow::anyhow!("request failed: {error}")));
                }
            }

            // GossipSub
            SwarmEvent::Behaviour(SyncBehaviourEvent::Gossipsub(gossipsub::Event::Message {
                propagation_source,
                message,
                ..
            })) => match serde_json::from_slice::<SyncNotification>(&message.data) {
                Ok(notification) => {
                    let _ = self
                        .event_tx
                        .send(Event::Notification {
                            source: propagation_source,
                            notification,
                        })
                        .await;
                }
                Err(e) => {
                    warn!("invalid gossipsub message: {e}");
                }
            },

            // Kademlia query results: dial found peers
            SwarmEvent::Behaviour(SyncBehaviourEvent::Kademlia(
                kad::Event::OutboundQueryProgressed {
                    result: kad::QueryResult::GetClosestPeers(Ok(ok)),
                    ..
                },
            )) => {
                for peer_info in &ok.peers {
                    debug!("DHT found peer: {}", peer_info.peer_id);
                    if let Err(e) = self.swarm.dial(peer_info.peer_id) {
                        debug!("dial from DHT failed: {e}");
                    }
                }
            }

            // GossipSub subscription: peer joined our topic = same sync group
            SwarmEvent::Behaviour(SyncBehaviourEvent::Gossipsub(
                gossipsub::Event::Subscribed { peer_id, topic },
            )) => {
                if self.topic.as_ref().is_some_and(|t| t.hash() == topic) {
                    info!("peer {peer_id} joined sync group");
                    let _ = self.event_tx.send(Event::PeerDiscovered(peer_id)).await;
                }
            }

            // Connection events (for disconnect tracking only)
            SwarmEvent::ConnectionEstablished {
                peer_id,
                num_established,
                ..
            } => {
                if num_established.get() == 1 {
                    debug!("connection established with {peer_id}");
                }
            }
            SwarmEvent::NewListenAddr { address, .. } => {
                info!("listening on {address}");
                let _ = self.event_tx.send(Event::Listening(address)).await;
            }
            SwarmEvent::ConnectionClosed {
                peer_id,
                num_established,
                ..
            } => {
                if num_established == 0 {
                    let _ = self.event_tx.send(Event::PeerDisconnected(peer_id)).await;
                }
            }

            _ => {}
        }
    }
}

/// Extract a short PeerId (8 bytes) for use in conflict resolution.
pub fn short_peer_id(peer_id: &PeerId) -> conflict::PeerId {
    let bytes = peer_id.to_bytes();
    let mut short = [0u8; 8];
    let len = bytes.len().min(8);
    short[..len].copy_from_slice(&bytes[..len]);
    short
}

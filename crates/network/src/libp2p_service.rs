//! libp2p-based NetworkService implementation.
//!
//! Uses TCP + Noise + Yamux transport with GossipSub for message
//! broadcast, mDNS for local peer discovery, and Kademlia DHT for
//! global peer discovery.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use libp2p::futures::StreamExt;
use libp2p::gossipsub::{self, IdentTopic, PeerScoreParams, PeerScoreThresholds, TopicScoreParams};
use libp2p::kad;
use libp2p::swarm::behaviour::toggle::Toggle;
use libp2p::swarm::SwarmEvent;
use libp2p::{identify, mdns, noise, tcp, yamux, Multiaddr, PeerId as Libp2pPeerId, Swarm, SwarmBuilder};
use tokio::sync::{mpsc, oneshot};
use tokio::time::interval;
use tracing::{debug, info, warn};

use crate::config::NetworkConfig;
use crate::error::NetworkError;
use crate::message::{NetworkEvent, NetworkMessage, PeerId};
use crate::service::NetworkService;

/// Topic category for gossipsub routing.
enum TopicKind {
    Blocks,
    Transactions,
}

/// Commands sent to the Swarm background task.
enum SwarmCommand {
    Publish { topic: TopicKind, data: Vec<u8> },
    /// Request a snapshot of current peer scores.
    PeerScores {
        reply: oneshot::Sender<Vec<(PeerId, f64)>>,
    },
    Shutdown,
}

/// Combined libp2p network behaviour for shell-chain.
#[derive(libp2p::swarm::NetworkBehaviour)]
struct ShellBehaviour {
    gossipsub: gossipsub::Behaviour,
    kademlia: Toggle<kad::Behaviour<kad::store::MemoryStore>>,
    mdns: Toggle<mdns::tokio::Behaviour>,
    identify: identify::Behaviour,
}

/// Production P2P network service backed by libp2p.
///
/// Spawns a background task running the libp2p Swarm event loop.
/// Communication with the swarm is via async channels.
pub struct Libp2pNetwork {
    cmd_tx: mpsc::Sender<SwarmCommand>,
    event_rx: mpsc::Receiver<NetworkEvent>,
    peer_count: Arc<AtomicUsize>,
}

impl Libp2pNetwork {
    /// Create and start the libp2p network.
    ///
    /// Begins listening on `config.listen_addr` and dials any boot nodes.
    /// Peer discovery via mDNS starts automatically.
    pub async fn new(config: &NetworkConfig) -> Result<Self, NetworkError> {
        let (cmd_tx, cmd_rx) = mpsc::channel(256);
        let (event_tx, event_rx) = mpsc::channel(256);
        let peer_count = Arc::new(AtomicUsize::new(0));

        let mut swarm = build_swarm(config)?;

        // Listen on configured address.
        let listen_addr: Multiaddr = format!(
            "/ip4/{}/tcp/{}",
            config.listen_addr.ip(),
            config.listen_addr.port()
        )
        .parse()
        .map_err(|e: libp2p::multiaddr::Error| NetworkError::Transport(e.to_string()))?;

        swarm
            .listen_on(listen_addr)
            .map_err(|e| NetworkError::Transport(e.to_string()))?;

        // Dial boot nodes and seed Kademlia routing table.
        for addr_str in &config.boot_nodes {
            match addr_str.parse::<Multiaddr>() {
                Ok(addr) => {
                    info!("Dialing boot node: {addr}");
                    // Extract PeerId from /p2p/<peer_id> component for Kademlia.
                    if let Some(peer_id) = extract_peer_id(&addr) {
                        if let Some(kad) = swarm.behaviour_mut().kademlia.as_mut() {
                            kad.add_address(&peer_id, addr.clone());
                        }
                    }
                    if let Err(e) = swarm.dial(addr) {
                        warn!("Failed to dial boot node: {e}");
                    }
                }
                Err(e) => warn!("Invalid boot node address '{addr_str}': {e}"),
            }
        }

        // Trigger initial Kademlia bootstrap if we have boot nodes.
        if !config.boot_nodes.is_empty() {
            if let Some(kad) = swarm.behaviour_mut().kademlia.as_mut() {
                if let Err(e) = kad.bootstrap() {
                    warn!("Kademlia bootstrap failed: {e:?}");
                }
            }
        }

        let blocks_topic = IdentTopic::new(&config.blocks_topic);
        let txs_topic = IdentTopic::new(&config.txs_topic);
        let pc = peer_count.clone();

        tokio::spawn(swarm_loop(
            swarm, cmd_rx, event_tx, pc, blocks_topic, txs_topic,
        ));

        Ok(Self {
            cmd_tx,
            event_rx,
            peer_count,
        })
    }

    /// Return a snapshot of all known peer scores.
    ///
    /// Sends a request to the swarm background task and awaits the reply.
    /// Returns an empty vec if the channel is closed or scoring is disabled.
    pub async fn peer_scores(&self) -> Vec<(PeerId, f64)> {
        let (tx, rx) = oneshot::channel();
        if self.cmd_tx.send(SwarmCommand::PeerScores { reply: tx }).await.is_err() {
            return Vec::new();
        }
        rx.await.unwrap_or_default()
    }
}

fn build_swarm(config: &NetworkConfig) -> Result<Swarm<ShellBehaviour>, NetworkError> {
    let enable_mdns = config.enable_mdns;
    let enable_kademlia = config.enable_kademlia;
    let enable_peer_scoring = config.enable_peer_scoring;
    let blocks_topic_name = config.blocks_topic.clone();
    let txs_topic_name = config.txs_topic.clone();

    // Deterministic message ID: blake3 hash of payload.
    // CRITICAL: Do NOT use DefaultHasher — its random per-process seed
    // makes MessageIds differ across nodes, breaking dedup (F-031).
    let message_id_fn = |msg: &gossipsub::Message| {
        let hash = blake3::hash(&msg.data);
        gossipsub::MessageId::from(hash.to_hex().as_str().to_owned())
    };

    let gs_config = gossipsub::ConfigBuilder::default()
        .heartbeat_interval(Duration::from_secs(1))
        .validation_mode(gossipsub::ValidationMode::Strict)
        .message_id_fn(message_id_fn)
        .max_transmit_size(4 * 1024 * 1024) // 4 MiB — PQ blocks can be large
        .build()
        .map_err(|e| NetworkError::Transport(format!("gossipsub config: {e}")))?;

    let swarm = SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )
        .map_err(|e| NetworkError::Transport(format!("transport: {e}")))?
        .with_dns()
        .map_err(|e| NetworkError::Transport(format!("dns transport: {e}")))?
        .with_behaviour(|key| {
            let peer_id = key.public().to_peer_id();

            let mut gossipsub = gossipsub::Behaviour::new(
                gossipsub::MessageAuthenticity::Signed(key.clone()),
                gs_config,
            )
            .map_err(|e| format!("gossipsub: {e}"))?;

            // Configure peer scoring to penalise misbehaving peers and
            // reward timely block/tx delivery.
            if enable_peer_scoring {
                let blocks_topic_params = TopicScoreParams {
                    topic_weight: 1.0,
                    time_in_mesh_weight: 0.5,
                    time_in_mesh_quantum: Duration::from_secs(1),
                    time_in_mesh_cap: 3600.0,
                    first_message_deliveries_weight: 5.0,
                    first_message_deliveries_cap: 100.0,
                    first_message_deliveries_decay: 0.99,
                    invalid_message_deliveries_weight: -100.0,
                    invalid_message_deliveries_decay: 0.5,
                    mesh_message_deliveries_weight: 0.0,
                    mesh_failure_penalty_weight: 0.0,
                    ..Default::default()
                };

                let txs_topic_params = TopicScoreParams {
                    topic_weight: 0.5,
                    time_in_mesh_weight: 0.3,
                    time_in_mesh_quantum: Duration::from_secs(1),
                    time_in_mesh_cap: 3600.0,
                    first_message_deliveries_weight: 2.0,
                    first_message_deliveries_cap: 1000.0,
                    first_message_deliveries_decay: 0.99,
                    invalid_message_deliveries_weight: -50.0,
                    invalid_message_deliveries_decay: 0.5,
                    mesh_message_deliveries_weight: 0.0,
                    mesh_failure_penalty_weight: 0.0,
                    ..Default::default()
                };

                let blocks_hash = IdentTopic::new(&blocks_topic_name).hash();
                let txs_hash = IdentTopic::new(&txs_topic_name).hash();

                let mut topic_scores = HashMap::new();
                topic_scores.insert(blocks_hash, blocks_topic_params);
                topic_scores.insert(txs_hash, txs_topic_params);

                let peer_score_params = PeerScoreParams {
                    topics: topic_scores,
                    ..Default::default()
                };

                let thresholds = PeerScoreThresholds {
                    gossip_threshold: -100.0,
                    publish_threshold: -200.0,
                    graylist_threshold: -300.0,
                    accept_px_threshold: 100.0,
                    opportunistic_graft_threshold: 5.0,
                };

                gossipsub
                    .with_peer_score(peer_score_params, thresholds)
                    .map_err(|e| format!("peer scoring: {e}"))?;
            }

            let kademlia = if enable_kademlia {
                let store = kad::store::MemoryStore::new(peer_id);
                let mut kad_config =
                    kad::Config::new(libp2p::StreamProtocol::new("/shell-chain/kad/1.0.0"));
                kad_config.set_query_timeout(Duration::from_secs(60));
                let mut behaviour = kad::Behaviour::with_config(peer_id, store, kad_config);
                behaviour.set_mode(Some(kad::Mode::Server));
                Some(behaviour)
            } else {
                None
            };

            let mdns = if enable_mdns {
                Some(
                    mdns::tokio::Behaviour::new(mdns::Config::default(), peer_id)
                        .map_err(|e| format!("mdns: {e}"))?,
                )
            } else {
                None
            };

            let identify = identify::Behaviour::new(identify::Config::new(
                "/shell-chain/1.0.0".into(),
                key.public(),
            ));

            Ok(ShellBehaviour {
                gossipsub,
                kademlia: kademlia.into(),
                mdns: mdns.into(),
                identify,
            })
        })
        .map_err(|e| NetworkError::Transport(format!("behaviour: {e}")))?
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
        .build();

    if enable_peer_scoring {
        info!("GossipSub peer scoring enabled");
    }
    if enable_kademlia {
        info!("Kademlia DHT peer discovery enabled");
    }
    if enable_mdns {
        info!("mDNS peer discovery enabled");
    } else {
        info!("mDNS peer discovery disabled (production mode)");
    }

    Ok(swarm)
}

/// Background task that drives the libp2p Swarm.
async fn swarm_loop(
    mut swarm: Swarm<ShellBehaviour>,
    mut cmd_rx: mpsc::Receiver<SwarmCommand>,
    event_tx: mpsc::Sender<NetworkEvent>,
    peer_count: Arc<AtomicUsize>,
    blocks_topic: IdentTopic,
    txs_topic: IdentTopic,
) {
    // Subscribe to gossipsub topics.
    if let Err(e) = swarm.behaviour_mut().gossipsub.subscribe(&blocks_topic) {
        warn!("Failed to subscribe to blocks topic: {e}");
    }
    if let Err(e) = swarm.behaviour_mut().gossipsub.subscribe(&txs_topic) {
        warn!("Failed to subscribe to txs topic: {e}");
    }

    // Periodic Kademlia bootstrap refresh (every 5 minutes).
    let mut kad_bootstrap_interval = interval(Duration::from_secs(300));
    // Skip the first immediate tick — bootstrap was already triggered on startup.
    kad_bootstrap_interval.tick().await;

    // Periodic peer score logging (every 60 seconds).
    let mut score_log_interval = interval(Duration::from_secs(60));
    score_log_interval.tick().await;

    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(SwarmCommand::Publish { topic, data }) => {
                        let ident = match topic {
                            TopicKind::Blocks => blocks_topic.clone(),
                            TopicKind::Transactions => txs_topic.clone(),
                        };
                        if let Err(e) = swarm
                            .behaviour_mut()
                            .gossipsub
                            .publish(ident, data)
                        {
                            debug!("Gossipsub publish error: {e}");
                        }
                    }
                    Some(SwarmCommand::PeerScores { reply }) => {
                        let scores = collect_peer_scores(&swarm);
                        let _ = reply.send(scores);
                    }
                    Some(SwarmCommand::Shutdown) | None => {
                        info!("libp2p swarm shutting down");
                        break;
                    }
                }
            }
            event = swarm.select_next_some() => {
                handle_swarm_event(
                    event,
                    &mut swarm,
                    &event_tx,
                    &peer_count,
                ).await;
            }
            _ = kad_bootstrap_interval.tick() => {
                if let Some(kad) = swarm.behaviour_mut().kademlia.as_mut() {
                    debug!("Periodic Kademlia bootstrap");
                    let _ = kad.bootstrap();
                }
            }
            _ = score_log_interval.tick() => {
                log_peer_scores(&swarm);
            }
        }
    }
}

/// Process a single SwarmEvent, forwarding relevant data as NetworkEvents.
async fn handle_swarm_event(
    event: SwarmEvent<ShellBehaviourEvent>,
    swarm: &mut Swarm<ShellBehaviour>,
    event_tx: &mpsc::Sender<NetworkEvent>,
    peer_count: &Arc<AtomicUsize>,
) {
    match event {
        // Gossipsub message received.
        SwarmEvent::Behaviour(ShellBehaviourEvent::Gossipsub(
            gossipsub::Event::Message {
                propagation_source,
                message,
                ..
            },
        )) => {
            let peer = PeerId(propagation_source.to_string());
            match serde_json::from_slice::<NetworkMessage>(&message.data) {
                Ok(msg) => {
                    let _ = event_tx
                        .send(NetworkEvent::MessageReceived {
                            peer,
                            message: msg,
                        })
                        .await;
                }
                Err(e) => {
                    debug!("Failed to deserialize gossipsub message: {e}");
                }
            }
        }
        // Kademlia routing table updated.
        SwarmEvent::Behaviour(ShellBehaviourEvent::Kademlia(
            kad::Event::RoutingUpdated { peer, .. },
        )) => {
            debug!("Kademlia routing updated: {peer}");
            // Add newly discovered peer to GossipSub mesh.
            swarm
                .behaviour_mut()
                .gossipsub
                .add_explicit_peer(&peer);
            let _ = event_tx
                .send(NetworkEvent::PeerConnected(PeerId(peer.to_string())))
                .await;
            // Emit routing table size update.
            if let Some(kad) = swarm.behaviour_mut().kademlia.as_mut() {
                let bucket_count: usize = kad.kbuckets().map(|b| b.num_entries()).sum();
                let _ = event_tx
                    .send(NetworkEvent::RoutingTableUpdated {
                        peer_count: bucket_count,
                    })
                    .await;
            }
        }
        // Kademlia query progress.
        SwarmEvent::Behaviour(ShellBehaviourEvent::Kademlia(
            kad::Event::OutboundQueryProgressed { result, .. },
        )) => {
            debug!("Kademlia query progress: {result:?}");
        }
        // Other Kademlia events.
        SwarmEvent::Behaviour(ShellBehaviourEvent::Kademlia(event)) => {
            debug!("Kademlia event: {event:?}");
        }
        // mDNS peer discovered.
        SwarmEvent::Behaviour(ShellBehaviourEvent::Mdns(mdns::Event::Discovered(peers))) => {
            for (peer_id, addr) in peers {
                info!("discovered peer on address peer={peer_id} address={addr}");
                swarm.add_peer_address(peer_id, addr);
                swarm
                    .behaviour_mut()
                    .gossipsub
                    .add_explicit_peer(&peer_id);
                let _ = event_tx
                    .send(NetworkEvent::PeerConnected(PeerId(peer_id.to_string())))
                    .await;
            }
            update_peer_count(swarm, peer_count);
        }
        // mDNS peer expired.
        SwarmEvent::Behaviour(ShellBehaviourEvent::Mdns(mdns::Event::Expired(peers))) => {
            for (peer_id, _addr) in peers {
                debug!("mDNS expired: {peer_id}");
                swarm
                    .behaviour_mut()
                    .gossipsub
                    .remove_explicit_peer(&peer_id);
                let _ = event_tx
                    .send(NetworkEvent::PeerDisconnected(PeerId(
                        peer_id.to_string(),
                    )))
                    .await;
            }
            update_peer_count(swarm, peer_count);
        }
        // New listen address bound.
        SwarmEvent::NewListenAddr { address, .. } => {
            let local_peer_id = *swarm.local_peer_id();
            info!("Listening on {address}/p2p/{local_peer_id}");
        }
        // Connection established.
        SwarmEvent::ConnectionEstablished { peer_id, .. } => {
            debug!("Connected to {peer_id}");
            update_peer_count(swarm, peer_count);
        }
        // Connection closed.
        SwarmEvent::ConnectionClosed { peer_id, .. } => {
            debug!("Disconnected from {peer_id}");
            update_peer_count(swarm, peer_count);
        }
        _ => {}
    }
}

fn update_peer_count(swarm: &Swarm<ShellBehaviour>, counter: &Arc<AtomicUsize>) {
    counter.store(swarm.connected_peers().count(), Ordering::Relaxed);
}

/// Collect current peer scores from the GossipSub behaviour.
fn collect_peer_scores(swarm: &Swarm<ShellBehaviour>) -> Vec<(PeerId, f64)> {
    swarm
        .behaviour()
        .gossipsub
        .all_peers()
        .filter_map(|(peer_id, _topics)| {
            swarm
                .behaviour()
                .gossipsub
                .peer_score(peer_id)
                .map(|score| (PeerId(peer_id.to_string()), score))
        })
        .collect()
}

/// Log peer scores, warning about peers below the gossip threshold.
fn log_peer_scores(swarm: &Swarm<ShellBehaviour>) {
    const GOSSIP_THRESHOLD: f64 = -100.0;

    for (peer_id, _topics) in swarm.behaviour().gossipsub.all_peers() {
        if let Some(score) = swarm.behaviour().gossipsub.peer_score(peer_id) {
            if score < GOSSIP_THRESHOLD {
                warn!(
                    %peer_id,
                    score,
                    "Peer score below gossip threshold"
                );
            } else {
                debug!(%peer_id, score, "Peer score");
            }
        }
    }
}

#[async_trait]
impl NetworkService for Libp2pNetwork {
    async fn broadcast(&self, msg: NetworkMessage) -> Result<(), NetworkError> {
        let topic = match &msg {
            NetworkMessage::NewBlock(_)
            | NetworkMessage::BlockRequest { .. }
            | NetworkMessage::BlockResponse { .. }
            | NetworkMessage::Ping
            | NetworkMessage::Pong => TopicKind::Blocks,
            NetworkMessage::NewTransaction(_) => TopicKind::Transactions,
        };

        let data =
            serde_json::to_vec(&msg).map_err(|e| NetworkError::Serialization(e.to_string()))?;

        self.cmd_tx
            .send(SwarmCommand::Publish { topic, data })
            .await
            .map_err(|_| NetworkError::ChannelClosed)?;

        Ok(())
    }

    async fn next_event(&mut self) -> Option<NetworkEvent> {
        self.event_rx.recv().await
    }

    async fn peer_count(&self) -> usize {
        self.peer_count.load(Ordering::Relaxed)
    }

    async fn shutdown(&self) -> Result<(), NetworkError> {
        self.cmd_tx
            .send(SwarmCommand::Shutdown)
            .await
            .map_err(|_| NetworkError::ChannelClosed)?;
        Ok(())
    }
}

/// Extract the libp2p PeerId from a multiaddr containing a `/p2p/<peer_id>` component.
fn extract_peer_id(addr: &Multiaddr) -> Option<Libp2pPeerId> {
    addr.iter().find_map(|proto| {
        if let libp2p::multiaddr::Protocol::P2p(peer_id) = proto {
            Some(peer_id)
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::NetworkConfig;

    #[test]
    fn config_defaults_enable_kademlia() {
        let config = NetworkConfig::default();
        assert!(config.enable_kademlia);
        assert!(!config.enable_mdns);
        assert_eq!(config.max_peers, 50);
    }

    #[test]
    fn config_defaults_enable_peer_scoring() {
        let config = NetworkConfig::default();
        assert!(config.enable_peer_scoring);
    }

    #[test]
    fn config_peer_scoring_disabled() {
        let config = NetworkConfig {
            enable_peer_scoring: false,
            ..Default::default()
        };
        assert!(!config.enable_peer_scoring);
    }

    #[test]
    fn build_swarm_with_peer_scoring() {
        let config = NetworkConfig {
            enable_peer_scoring: true,
            enable_mdns: false,
            enable_kademlia: false,
            ..Default::default()
        };
        let swarm = build_swarm(&config);
        assert!(swarm.is_ok(), "build_swarm should succeed with peer scoring enabled");
    }

    #[test]
    fn build_swarm_without_peer_scoring() {
        let config = NetworkConfig {
            enable_peer_scoring: false,
            enable_mdns: false,
            enable_kademlia: false,
            ..Default::default()
        };
        let swarm = build_swarm(&config);
        assert!(swarm.is_ok(), "build_swarm should succeed with peer scoring disabled");
    }

    #[test]
    fn extract_peer_id_from_valid_multiaddr() {
        // Generate a valid PeerId from a keypair.
        let keypair = libp2p::identity::Keypair::generate_ed25519();
        let peer_id = keypair.public().to_peer_id();
        let addr: Multiaddr = format!("/ip4/127.0.0.1/tcp/30303/p2p/{peer_id}")
            .parse()
            .unwrap();
        let extracted = extract_peer_id(&addr);
        assert_eq!(extracted, Some(peer_id));
    }

    #[test]
    fn extract_peer_id_missing_returns_none() {
        let addr: Multiaddr = "/ip4/127.0.0.1/tcp/30303".parse().unwrap();
        assert!(extract_peer_id(&addr).is_none());
    }

    #[test]
    fn routing_table_updated_event_variant() {
        let event = NetworkEvent::RoutingTableUpdated { peer_count: 42 };
        match event {
            NetworkEvent::RoutingTableUpdated { peer_count } => {
                assert_eq!(peer_count, 42);
            }
            _ => panic!("wrong event variant"),
        }
    }

    #[test]
    fn network_config_with_kademlia_disabled() {
        let config = NetworkConfig {
            enable_kademlia: false,
            ..Default::default()
        };
        assert!(!config.enable_kademlia);
    }
}

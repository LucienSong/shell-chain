//! libp2p-based NetworkService implementation.
//!
//! Uses TCP + Noise + Yamux transport with GossipSub for message
//! broadcast and mDNS for local peer discovery.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use libp2p::futures::StreamExt;
use libp2p::gossipsub::{self, IdentTopic};
use libp2p::swarm::SwarmEvent;
use libp2p::{identify, mdns, noise, tcp, yamux, Multiaddr, Swarm, SwarmBuilder};
use tokio::sync::mpsc;
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
    Shutdown,
}

/// Combined libp2p network behaviour for shell-chain.
#[derive(libp2p::swarm::NetworkBehaviour)]
struct ShellBehaviour {
    gossipsub: gossipsub::Behaviour,
    mdns: mdns::tokio::Behaviour,
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

        // Dial boot nodes.
        for addr_str in &config.boot_nodes {
            match addr_str.parse::<Multiaddr>() {
                Ok(addr) => {
                    info!("Dialing boot node: {addr}");
                    if let Err(e) = swarm.dial(addr) {
                        warn!("Failed to dial boot node: {e}");
                    }
                }
                Err(e) => warn!("Invalid boot node address '{addr_str}': {e}"),
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
}

fn build_swarm(config: &NetworkConfig) -> Result<Swarm<ShellBehaviour>, NetworkError> {
    let _ = config; // NetworkConfig fields used by caller, not here directly.

    let message_id_fn = |msg: &gossipsub::Message| {
        let mut hasher = DefaultHasher::new();
        msg.data.hash(&mut hasher);
        gossipsub::MessageId::from(hasher.finish().to_string())
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
        .with_behaviour(|key| {
            let peer_id = key.public().to_peer_id();

            let gossipsub = gossipsub::Behaviour::new(
                gossipsub::MessageAuthenticity::Signed(key.clone()),
                gs_config,
            )
            .map_err(|e| format!("gossipsub: {e}"))?;

            let mdns = mdns::tokio::Behaviour::new(mdns::Config::default(), peer_id)
                .map_err(|e| format!("mdns: {e}"))?;

            let identify = identify::Behaviour::new(identify::Config::new(
                "/shell-chain/1.0.0".into(),
                key.public(),
            ));

            Ok(ShellBehaviour {
                gossipsub,
                mdns,
                identify,
            })
        })
        .map_err(|e| NetworkError::Transport(format!("behaviour: {e}")))?
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
        .build();

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
        // mDNS peer discovered.
        SwarmEvent::Behaviour(ShellBehaviourEvent::Mdns(mdns::Event::Discovered(peers))) => {
            for (peer_id, addr) in peers {
                debug!("mDNS discovered: {peer_id} at {addr}");
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

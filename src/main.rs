use anyhow::anyhow;
use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use libp2p::gossipsub::MessageId;
use libp2p::gossipsub::PublishError;
use libp2p::swarm::DialError;
use libp2p::swarm::{dial_opts::DialOpts, ConnectionId};
use libp2p::{
    connection_limits::{self, ConnectionLimits},
    futures::StreamExt,
    gossipsub,
    identify::{self, Behaviour as IdentifyBehaviour},
    identity::ed25519,
    identity::Keypair,
    kad::{self, store::MemoryStore, Behaviour as KademliaBehaviour},
    mdns,
    swarm::{behaviour::toggle::Toggle, NetworkBehaviour, SwarmEvent},
    Multiaddr, StreamProtocol, Swarm,
};
use std::net::ToSocketAddrs;
use std::future::Future;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::{hash::DefaultHasher, io::Error, time::Duration};
use std::{
    hash::{Hash, Hasher},
    process::Command,
};
use tokio::{
    select,
    sync::{broadcast, mpsc},
    time::sleep,
};
use tracing::warn;
use tracing::{error, info};

static NEXT_CORRELATION_ID: AtomicUsize = AtomicUsize::new(1);

#[derive(NetworkBehaviour)]
pub struct NodeBehaviour {
    gossipsub: gossipsub::Behaviour,
    kademlia: KademliaBehaviour<MemoryStore>,
    connection_limits: connection_limits::Behaviour,
    mdns: Toggle<mdns::tokio::Behaviour>,
    identify: IdentifyBehaviour,
}

fn create_mdns_kad_behaviour(
    enable_mdns: bool,
    key: &Keypair,
) -> std::result::Result<NodeBehaviour, Box<dyn std::error::Error + Send + Sync + 'static>> {
    let connection_limits = connection_limits::Behaviour::new(ConnectionLimits::default());
    let identify_config = IdentifyBehaviour::new(
        identify::Config::new("/kad/1.0.0".into(), key.public())
            .with_interval(Duration::from_secs(60)),
    );

    let kad_config = kad::Config::new(StreamProtocol::new("/kad/1.0.0"));

    let message_id_fn = |message: &gossipsub::Message| {
        let mut s = DefaultHasher::new();
        message.data.hash(&mut s);
        gossipsub::MessageId::from(s.finish().to_string())
    };

    let gossipsub_config = gossipsub::ConfigBuilder::default()
        .mesh_n(3)
        .mesh_n_low(2)
        .mesh_outbound_min(1)
        .heartbeat_interval(Duration::from_secs(1))
        .validation_mode(gossipsub::ValidationMode::Strict)
        .message_id_fn(message_id_fn)
        .build()
        .map_err(|msg| Error::new(std::io::ErrorKind::Other, msg))?;

    let gossipsub = gossipsub::Behaviour::new(
        gossipsub::MessageAuthenticity::Signed(key.clone()),
        gossipsub_config,
    )?;

    let mdns = if enable_mdns {
        Toggle::from(Some(mdns::tokio::Behaviour::new(
            mdns::Config::default(),
            key.public().to_peer_id(),
        )?))
    } else {
        Toggle::from(None)
    };

    Ok(NodeBehaviour {
        gossipsub,
        kademlia: KademliaBehaviour::with_config(
            key.public().to_peer_id(),
            MemoryStore::new(key.public().to_peer_id()),
            kad_config,
        ),
        mdns,
        connection_limits,
        identify: identify_config,
    })
}

fn resolve_ipv4(domain: &str) -> Result<String> {
    let addr = format!("{}:0", domain)
        .to_socket_addrs()?
        .find(|addr| addr.ip().is_ipv4())
        .context("no IPv4 addresses found")?;
    Ok(addr.ip().to_string())
}

fn resolve_ipv6(domain: &str) -> Result<String> {
    let addr = format!("{}:0", domain)
        .to_socket_addrs()?
        .find(|addr| addr.ip().is_ipv6())
        .context("no IPv6 addresses found")?;
    Ok(addr.ip().to_string())
}

/// Retries an async operation with exponential backoff
///
/// # Arguments
/// * `operation` - Async function to retry
/// * `max_attempts` - Maximum number of retry attempts
/// * `initial_delay_ms` - Initial delay between retries in milliseconds
///
/// # Returns
/// * `Result<()>` - Ok if the operation succeeded, Err if all retries failed
pub async fn retry_with_backoff<F, Fut>(
    operation: F,
    max_attempts: u32,
    initial_delay_ms: u64,
) -> Result<()>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<()>>,
{
    let mut current_attempt = 1;
    let mut delay_ms = initial_delay_ms;

    loop {
        match operation().await {
            Ok(_) => return Ok(()),
            Err(e) => {
                if current_attempt >= max_attempts {
                    return Err(anyhow::anyhow!(
                        "Operation failed after {} attempts. Last error: {}",
                        max_attempts,
                        e
                    ));
                }

                println!(
                    "Attempt {}/{} failed, retrying in {}ms: {}",
                    current_attempt, max_attempts, delay_ms, e
                );

                sleep(Duration::from_millis(delay_ms)).await;
                current_attempt += 1;
                delay_ms *= 2; // Exponential backoff
            }
        }
    }
}

async fn attempt_connection(
    cmd_tx: &mpsc::Sender<NetworkPeerCommand>,
    event_tx: &broadcast::Sender<NetworkPeerEvent>,
    domain: &str,
) -> Result<()> {
    let mut event_rx = event_tx.subscribe();
    let ip = resolve_ipv4(domain)?;
    println!("Resolved '{}' to {}", domain, &ip);

    let addr = format!("/ip4/{}/udp/4001/quic-v1", ip);
    println!("addr:{}", addr);

    let multi: Multiaddr = addr.parse()?;
    println!("Dialing: {}...", multi);

    let opts: DialOpts = multi.clone().into();
    let dial_connection = opts.connection_id();
    println!("Dialing {} with connection {}", multi, dial_connection);

    cmd_tx.send(NetworkPeerCommand::Dial(opts)).await?;

    wait_for_connection(&mut event_rx, dial_connection).await
}

async fn wait_for_connection(
    event_rx: &mut broadcast::Receiver<NetworkPeerEvent>,
    dial_connection: ConnectionId,
) -> Result<()> {
    loop {
        match event_rx.recv().await? {
            NetworkPeerEvent::ConnectionEstablished { connection_id } => {
                if connection_id == dial_connection {
                    println!("Connection Established");
                    return Ok(());
                }
            }
            NetworkPeerEvent::OutgoingConnectionError { connection_id, error } => {
                if connection_id == dial_connection {
                    println!("Connection {} failed because of error {}. Retrying...", connection_id, error);
                    // sleep(Duration::from_secs(2)).await;
                    return Err(anyhow!("Connection failed"));
                }
            }
            _ => (),
        }
    }
}

async fn attempt_gossip_publish(
    cmd_tx: &mpsc::Sender<NetworkPeerCommand>,
    event_tx: &broadcast::Sender<NetworkPeerEvent>,
    topic: &str,
    data: Vec<u8>,
) -> Result<()> {
    let mut event_rx = event_tx.subscribe();
    let correlation_id = NEXT_CORRELATION_ID.fetch_add(1, Ordering::SeqCst);
    cmd_tx
        .send(NetworkPeerCommand::GossipPublish {
            topic: topic.to_string(),
            data,
            correlation_id,
        })
        .await?; // this should be sent over gossipsub to all the nodes
    wait_for_publish_confirmation(&mut event_rx, correlation_id).await
}

async fn wait_for_publish_confirmation(
    event_rx: &mut broadcast::Receiver<NetworkPeerEvent>,
    correlation_id: usize,
) -> Result<()> {
    loop {
        match event_rx.recv().await? {
            NetworkPeerEvent::GossipPublished {
                correlation_id: published_id,
                message_id,
            } => {
                if correlation_id == published_id {
                    println!("Publish has been confirmed with id: {}", message_id);
                    return Ok(());
                }
            }
            NetworkPeerEvent::GossipPublishError {
                correlation_id: published_id,
                error
            } => {
                if correlation_id == published_id {
                    return Err(anyhow!("Publishing failed {}", error));
                }
            }
            _ => (),
        }
    }
}

const BACKOFF_DELAY: u64 = 500;
const BACKOFF_MAX_RETRIES: u32 = 10;

// This will dial domain with retry and exponential backoff
async fn dial_domain(
    cmd_tx: &mpsc::Sender<NetworkPeerCommand>,
    event_tx: &broadcast::Sender<NetworkPeerEvent>,
    domain: &str,
) -> Result<()> {
    println!("Now dialing in to {}", domain);
    retry_with_backoff(|| attempt_connection(cmd_tx, event_tx, domain), BACKOFF_MAX_RETRIES, BACKOFF_DELAY).await?;
    Ok(())
}

async fn gossip_data(
    cmd_tx: &mpsc::Sender<NetworkPeerCommand>,
    event_tx: &broadcast::Sender<NetworkPeerEvent>,
    topic: &str,
    data: Vec<u8>,
) -> Result<()> {
    println!("Now publishing data {:?} on topic {}", data, topic);
    retry_with_backoff(
        || attempt_gossip_publish(cmd_tx, event_tx, topic, data.clone()),
        BACKOFF_MAX_RETRIES,
        BACKOFF_DELAY,
    )
    .await?;
    Ok(())
}

enum NetworkPeerCommand {
    GossipPublish {
        topic: String,
        data: Vec<u8>,
        correlation_id: usize,
    },
    Dial(DialOpts),
}

#[derive(Clone, Debug)]
enum NetworkPeerEvent {
    GossipData(Vec<u8>),
    GossipPublishError {
        // TODO: return an error here? DialError is not Clonable so we have
        // avoided passing it on
        correlation_id: usize,
        error: Arc<PublishError>
    },
    GossipPublished {
        correlation_id: usize,
        message_id: MessageId,
    },
    ConnectionEstablished {
        connection_id: ConnectionId,
    },
    OutgoingConnectionError {
        connection_id: ConnectionId,
        error: Arc<DialError>
    },
}

// This is what the "peer" role will do
async fn peer_behaviour(
    cmd_tx: &mpsc::Sender<NetworkPeerCommand>,
    event_tx: &broadcast::Sender<NetworkPeerEvent>,
) -> Result<()> {
    dial_domain(cmd_tx, event_tx, "bootstrap").await?;
    Ok(())
}

// This is what the "sender" role will do
async fn sender_behaviour(
    cmd_tx: &mpsc::Sender<NetworkPeerCommand>,
    event_tx: &broadcast::Sender<NetworkPeerEvent>,
    topic: &str,
) -> Result<()> {
    dial_domain(&cmd_tx, event_tx, "peer").await?;
    println!("Sending message 1,2,3,4...");
    gossip_data(cmd_tx, event_tx, topic, vec![1, 2, 3, 4]).await?;
    println!("Sent and array of bytes 1,2,3,4 to be gossiped");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let (mut event_tx, _) = broadcast::channel::<NetworkPeerEvent>(100); // TODO : tune this param
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<NetworkPeerCommand>(100); // TODO : tune this param

    info!("STARTING!");
    let enable_mdns = false;
    let topic_str = "some_topic";
    let ed25519_keypair = ed25519::Keypair::generate();

    let keypair: libp2p::identity::Keypair = ed25519_keypair.try_into()?;
    let mut swarm = libp2p::SwarmBuilder::with_existing_identity(keypair)
        .with_tokio()
        .with_quic()
        .with_behaviour(|key| create_mdns_kad_behaviour(enable_mdns, key))?
        .build();

    let topic = gossipsub::IdentTopic::new(topic_str);

    swarm.behaviour_mut().gossipsub.subscribe(&topic)?;
    let addr = "/ip4/0.0.0.0/udp/4001/quic-v1".to_string();
    swarm.listen_on(addr.parse()?)?;

    // Specialized role behaviours
    tokio::spawn({
        let event_tx = event_tx.clone();
        async move {
            let role = std::env::var("ROLE").unwrap_or_default();
            match role.as_str() {
                "peer" => peer_behaviour(&cmd_tx, &event_tx).await?,
                "sender" => sender_behaviour(&cmd_tx, &event_tx, topic_str).await?,
                _ => (),
            };
            anyhow::Ok(())
        }
    });

    // Print any messages received
    // This might represent the event bus in a broader application
    tokio::spawn({
        let event_tx = event_tx.clone();
        async move {
            loop {
                let mut event_rx = event_tx.subscribe();
                select! {
                    Ok(event) = event_rx.recv() => {
                        match event {
                            NetworkPeerEvent::GossipData(data) => {
                                println!("Received raw message data: {:?}", data);
                            },
                            _ => ()
                        }
                    }
                }
            }
        }
    });

    loop {
        select! {
            // Process commands
            Some(command) = cmd_rx.recv() => {
                match command {
                    NetworkPeerCommand::GossipPublish { data, topic, correlation_id } => {
                        let gossipsub_behaviour = &mut swarm.behaviour_mut().gossipsub;
                        match gossipsub_behaviour
                            .publish(gossipsub::IdentTopic::new(topic), data) {
                            Ok(message_id) => {
                                event_tx.send(NetworkPeerEvent::GossipPublished { correlation_id, message_id })?;
                            },
                            Err(e) => {
                                warn!(error=?e, "Could not publish to swarm. Retrying...");
                                event_tx.send(NetworkPeerEvent::GossipPublishError { correlation_id, error: Arc::new(e) })?;
                            }
                        }
                    },
                    NetworkPeerCommand::Dial(multi) => {
                        swarm.dial(multi)?;
                    }
                }
            }
            // Process events
            event = swarm.select_next_some() =>  {
                process_swarm_event(&mut swarm, &mut event_tx, event).await?
            }
        }
    }
}

async fn process_swarm_event(
    swarm: &mut Swarm<NodeBehaviour>,
    event_tx: &mut broadcast::Sender<NetworkPeerEvent>,
    event: SwarmEvent<NodeBehaviourEvent>,
) -> Result<()> {
    match event {
        SwarmEvent::ConnectionEstablished {
            peer_id,
            endpoint,
            connection_id,
            ..
        } => {
            info!("Connected to {peer_id}");
            let remote_addr = endpoint.get_remote_address().clone();
            swarm
                .behaviour_mut()
                .kademlia
                .add_address(&peer_id, remote_addr.clone());

            info!("Added address to kademlia {}", remote_addr);
            swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer_id);
            info!("Added peer to gossipsub {}", remote_addr);
            event_tx.send(NetworkPeerEvent::ConnectionEstablished { connection_id })?;
        }

        SwarmEvent::OutgoingConnectionError {
            peer_id,
            error,
            connection_id,
        } => {
            info!("Failed to dial {peer_id:?}: {error}");
            event_tx.send(NetworkPeerEvent::OutgoingConnectionError { connection_id, error: Arc::new(error) })?;
        }

        SwarmEvent::IncomingConnectionError { error, .. } => {
            info!("{:#}", anyhow::Error::from(error))
        }

        SwarmEvent::Behaviour(NodeBehaviourEvent::Kademlia(kad::Event::InboundRequest {
            request,
        })) => {
            info!("Inbound Kademlia request: {:?}", request);
        }

        SwarmEvent::Behaviour(NodeBehaviourEvent::Kademlia(e)) => {
            info!("Other Kademlia event: {:?}", e);
        }

        // SwarmEvent::Behaviour(NodeBehaviourEvent::Mdns(mdns::Event::Discovered(list))) => {
        //     for (peer_id, _multiaddr) in list {
        //         info!("mDNS discovered a new peer: {peer_id}");
        //         swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer_id);
        //     }
        // }
        //
        // SwarmEvent::Behaviour(NodeBehaviourEvent::Mdns(mdns::Event::Expired(list))) => {
        //     for (peer_id, _multiaddr) in list {
        //         info!("mDNS discover peer has expired: {peer_id}");
        //         swarm
        //             .behaviour_mut()
        //             .gossipsub
        //             .remove_explicit_peer(&peer_id);
        //     }
        // }
        SwarmEvent::Behaviour(NodeBehaviourEvent::Gossipsub(gossipsub::Event::Message {
            propagation_source: peer_id,
            message_id: id,
            message,
        })) => {
            info!("Got message with id: {id} from peer: {peer_id}",);
            // info!("{:?}", message);
            event_tx.send(NetworkPeerEvent::GossipData(message.data))?;
        }
        SwarmEvent::NewListenAddr { address, .. } => {
            info!("Local node is listening on {address}");
        }
        _ => {}
    };
    Ok(())
}

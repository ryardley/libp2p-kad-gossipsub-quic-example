use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
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
use std::{hash::DefaultHasher, io::Error, time::Duration};
use std::{
    hash::{Hash, Hasher},
    process::Command,
};
use tokio::{
    select,
    sync::mpsc::{channel, Sender},
    time::sleep,
};
use tracing::{error, info};

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
        .heartbeat_interval(Duration::from_secs(10))
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

fn resolve_ip(domain: &str) -> Result<String> {
    let output = Command::new("dig")
        .arg("+short")
        .arg(domain)
        .output()
        .context("failed to execute dig")?;
    let ip = String::from_utf8(output.stdout)
        .context("invalid utf8")?
        .trim()
        .to_string();

    if !ip.len() == 0 {
        bail!("IP was not detected by dig")
    }

    Ok(ip)
}

fn dial_node(swarm: &mut Swarm<NodeBehaviour>, domain: &str) -> Result<()> {
    println!("Now dialing in to {}", domain);
    let ip = resolve_ip(domain)?;
    println!("Resolved '{}' to {}", domain, &ip);
    let addr = format!("/ip4/{}/udp/4001/quic-v1", ip).to_string();
    println!("addr:{}", addr);
    let multi: Multiaddr = addr.parse()?;
    println!("Dialing: {}...", multi);
    swarm.dial(multi.clone())?;
    println!("Finished dialing: {}", multi);

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("STARTING!");
    let enable_mdns = false;
    let topic = "some_topic";
    let ed25519_keypair = ed25519::Keypair::generate();

    let keypair: libp2p::identity::Keypair = ed25519_keypair.try_into()?;
    let mut swarm = libp2p::SwarmBuilder::with_existing_identity(keypair)
        .with_tokio()
        .with_quic()
        .with_behaviour(|key| create_mdns_kad_behaviour(enable_mdns, key))?
        // .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
        .build();

    let topic = gossipsub::IdentTopic::new(topic);

    swarm.behaviour_mut().gossipsub.subscribe(&topic)?;
    let addr = "/ip4/0.0.0.0/udp/4001/quic-v1".to_string();
    swarm.listen_on(addr.parse()?)?;
    let (mut to_bus_tx, mut from_net_rx) = channel::<Vec<u8>>(100); // TODO : tune this param
    let (to_net_tx, mut from_bus_rx) = channel::<Vec<u8>>(100); // TODO : tune this param

    if std::env::var("ROLE").unwrap_or_default() == "peer" {
        println!("Waiting 10 seconds before dialing...");
        sleep(Duration::from_secs(10)).await;
        dial_node(&mut swarm, "bootstrap")?;
    } else if std::env::var("ROLE").unwrap_or_default() == "sender" {
        println!("Waiting 11 seconds before dialing...");
        sleep(Duration::from_secs(11)).await;
        dial_node(&mut swarm, "peer")?;
        tokio::spawn(async move {
            println!("Waiting for 10 seconds for network to settle...");
            sleep(Duration::from_secs(10)).await;
            println!("Finished waiting!");
            println!("Sending message 1,2,3,4");
            to_net_tx.send(vec![1, 2, 3, 4]).await?; // this should be sent over gossipsub to all the nodes
            println!("Sent and array of bytes 1,2,3,4 to be gossiped");
            anyhow::Ok(())
        });
    }

    tokio::spawn(async move {
        loop {
            select! {
                Some(line) = from_net_rx.recv() => {
                    println!("Received raw message data: {:?}", line);
                }
            }
        }
    });

    loop {
        select! {
            Some(line) = from_bus_rx.recv() => {
                if let Err(e) = swarm
                    .behaviour_mut().gossipsub
                    .publish(topic.clone(), line) {
                    error!(error=?e, "Error publishing line to swarm");
                }
            }

            event = swarm.select_next_some() =>  {
                process_swarm_event(&mut swarm, &mut to_bus_tx, event).await?
            }
        }
    }
}

async fn process_swarm_event(
    swarm: &mut Swarm<NodeBehaviour>,
    to_bus_tx: &mut Sender<Vec<u8>>,
    event: SwarmEvent<NodeBehaviourEvent>,
) -> Result<()> {
    match event {
        SwarmEvent::ConnectionEstablished {
            peer_id, endpoint, ..
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
        }

        SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
            info!("Failed to dial {peer_id:?}: {error}");
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
            to_bus_tx.send(message.data).await?;
        }
        SwarmEvent::NewListenAddr { address, .. } => {
            info!("Local node is listening on {address}");
        }
        _ => {}
    };
    Ok(())
}

use futures::prelude::*;
use libp2p::gossipsub::{IdentTopic as Topic, TopicHash};
use libp2p::swarm::{NetworkBehaviour, Swarm, SwarmEvent};
use libp2p::{gossipsub, multiaddr::Protocol, noise, tcp, yamux, Multiaddr};
use rand::{Rng, RngCore, SeedableRng};
use rand_chacha::ChaCha20Rng;
use serde::{Deserialize, Serialize};
use std::{error::Error, net::Ipv4Addr, sync::Arc, time::SystemTime};
use tokio::sync::mpsc;
use tokio::time::{Duration, Instant};

// Ethereum network configs
const SLOTS_PER_EPOCH: u64 = 4;
const SECONDS_PER_SLOT: u64 = 12;
// Sat Jan 01 2000 00:00:00 GMT+0000
const GENESIS_DURATION_SINCE_UNIX_EPOCH: Duration = Duration::from_secs(946684800);

const BASE_IP_ADDRESS: Ipv4Addr = Ipv4Addr::new(1, 0, 0, 1);
const PORT: u16 = 9000;

const PUBLISH_BUFFER_SIZE: usize = 3;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Usage: ./test_libp2p_eth [graph-file] [node-id]

    let graph_file = std::env::args().nth(1).ok_or("no graph file provided")?;
    let node_id: ValidatorId = std::env::args()
        .nth(2)
        .ok_or("no node id provided")?
        .parse::<usize>()?
        .into();
    let content = tokio::fs::read_to_string(graph_file).await?;

    let mut swarm = libp2p::SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_behaviour(|key| {
            // To content-address message, we can take the hash of message and use it as an ID.
            let message_id_fn = |message: &gossipsub::Message| {
                let message = Message::decode(message.data.as_slice());
                message.id()
            };

            // Set a custom gossipsub configuration
            let gossipsub_config = gossipsub::ConfigBuilder::default()
                .max_transmit_size(10485760) // 10MB
                .heartbeat_interval(Duration::from_secs(10))
                .validation_mode(gossipsub::ValidationMode::Strict)
                .message_id_fn(message_id_fn)
                .build()
                .map_err(|msg| tokio::io::Error::new(tokio::io::ErrorKind::Other, msg))?;

            // Build a gossipsub network behaviour
            let gossipsub = gossipsub::Behaviour::new(
                gossipsub::MessageAuthenticity::Signed(key.clone()),
                gossipsub_config,
            )?;

            Ok(EthBehaviour { gossipsub })
        })?
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
        .build();

    let mut addr: Multiaddr = "/ip4/0.0.0.0".parse()?;
    addr.push(Protocol::Tcp(PORT));

    // Listen for the incoming peers.
    swarm.listen_on(addr)?;

    // Read the graph and connect to the nodes to which it's supposed to connect
    let line = content
        .lines()
        .nth(usize::from(node_id))
        .ok_or("node id is too high for the graph")?;

    // The ids of nodes to which it's supposed to connect
    let peer_ids: Vec<ValidatorId> = line
        .split(' ')
        .map(|s| s.parse::<usize>().expect("non-numeric peer id").into())
        .collect();

    for peer_id in peer_ids {
        // Don't connect to itself
        if node_id == peer_id {
            continue;
        }
        let ipaddr = Ipv4Addr::from(u32::from(BASE_IP_ADDRESS) + usize::from(peer_id) as u32);
        let remote = Multiaddr::from_iter([Protocol::Ip4(ipaddr), Protocol::Tcp(PORT)]);
        swarm.dial(remote.clone())?;
    }

    let num_validators = content.lines().count();
    println!("Found {num_validators} nodes/validators on the network");

    let node = EthNode::new(node_id, EthNetwork::new(num_validators));
    Arc::new(node).run(swarm).await
}

macro_rules! wrapper_impl {
    ($ident:ident, $ty:ty) => {
        #[derive(Serialize, Deserialize, PartialEq, Eq, Copy, Clone)]
        struct $ident($ty);

        impl From<$ty> for $ident {
            fn from(v: $ty) -> Self {
                Self(v)
            }
        }
        impl From<$ident> for $ty {
            fn from(v: $ident) -> Self {
                v.0
            }
        }
        impl std::fmt::Display for $ident {
            fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

wrapper_impl!(ValidatorId, usize);
wrapper_impl!(CommitteeId, usize);
wrapper_impl!(Slot, u64);
wrapper_impl!(Epoch, u64);

impl From<Slot> for Epoch {
    fn from(slot: Slot) -> Self {
        Self(slot.0 / SLOTS_PER_EPOCH)
    }
}

#[derive(Copy, Clone)]
enum GossipTopic {
    BeaconBlock,
}

const BEACON_BLOCK_TOPIC: &str = "beacon_block";

impl std::fmt::Display for GossipTopic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let topic = match self {
            GossipTopic::BeaconBlock => String::from(BEACON_BLOCK_TOPIC),
        };
        write!(f, "{}", topic)
    }
}

impl From<GossipTopic> for String {
    fn from(topic: GossipTopic) -> Self {
        topic.to_string()
    }
}
impl From<GossipTopic> for Topic {
    fn from(topic: GossipTopic) -> Self {
        Topic::new(topic)
    }
}
impl From<GossipTopic> for TopicHash {
    fn from(topic: GossipTopic) -> Self {
        TopicHash::from(Topic::from(topic))
    }
}

#[derive(Serialize, Deserialize)]
struct BeaconBlock {
    proposer: ValidatorId,
    slot: Slot,
}
#[derive(Serialize, Deserialize)]
enum MessageHeader {
    BeaconBlock(BeaconBlock),
}
impl From<BeaconBlock> for MessageHeader {
    fn from(block: BeaconBlock) -> Self {
        Self::BeaconBlock(block)
    }
}
struct Message {
    header: MessageHeader,
    padding: Vec<u8>,
}

const BEACON_BLOCK_PADDING_SIZE: usize = 128 << 10; // 128KB

impl std::fmt::Display for Message {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let serialized = serde_json::to_string(&self.header).unwrap();
        write!(f, "{}", serialized)
    }
}

impl Message {
    fn new(header: MessageHeader) -> Self {
        let size = match header {
            MessageHeader::BeaconBlock(_) => BEACON_BLOCK_PADDING_SIZE,
        };
        let padding = {
            let mut padding = vec![0; size];
            rand::thread_rng().fill(padding.as_mut_slice());
            padding
        };
        Self { header, padding }
    }
    fn id(&self) -> gossipsub::MessageId {
        let encoded = self.encode();
        let hash = ring::digest::digest(&ring::digest::SHA256, encoded.as_slice());
        gossipsub::MessageId::from(hash.as_ref())
    }
    fn topic(&self) -> GossipTopic {
        match self.header {
            MessageHeader::BeaconBlock(_) => GossipTopic::BeaconBlock,
        }
    }
    fn encode(&self) -> Vec<u8> {
        let mut result = Vec::new();
        // Serialize the header
        let header_bytes = serde_json::to_string(&self.header).unwrap().into_bytes();
        // Put the length of the header first, so that we will know how long it is
        let header_len_bytes: [u8; 8] = (header_bytes.len() as u64).to_be_bytes();
        result.extend_from_slice(&header_len_bytes[..]);
        // Put the actual header
        result.extend_from_slice(header_bytes.as_slice());
        // Put the padding
        result.extend_from_slice(self.padding.as_slice());
        result
    }
    fn decode(bytes: &[u8]) -> Self {
        // Read the length of the header
        let header_len = u64::from_be_bytes((&bytes[..8]).try_into().unwrap());
        let bytes = &bytes[8..];
        // Read the header
        let header_bytes = String::from_utf8((&bytes[..header_len as usize]).into()).unwrap();
        let bytes = &bytes[header_len as usize..];
        let header = serde_json::from_str(&header_bytes).unwrap();
        // Read the padding
        let padding = Vec::from(&bytes[..]);
        Self { header, padding }
    }
}

#[derive(NetworkBehaviour)]
struct EthBehaviour {
    gossipsub: gossipsub::Behaviour,
}

// Represent a specific node in the network
struct EthNode {
    vid: ValidatorId,
    network: EthNetwork,
}

impl EthNode {
    fn new(vid: ValidatorId, network: EthNetwork) -> Self {
        Self { vid, network }
    }
    async fn propose(self: Arc<Self>, tx: mpsc::Sender<Message>) -> ! {
        let mut interval = tokio::time::interval_at(
            self.network.genesis_instant(),
            Duration::from_secs(SECONDS_PER_SLOT),
        );
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let slot = self.network.current_slot();
            // Figure out who is the next proposer
            let proposer_id = self.network.proposer(slot);
            // If it's itself, publish a block
            if proposer_id == self.vid {
                let message = Message::new(MessageHeader::from(BeaconBlock {
                    proposer: proposer_id,
                    slot,
                }));
                if let Err(e) = tx.send(message).await {
                    println!("Sending to publish error: {e:?}");
                }
            }
        }
    }
    async fn run(self: Arc<Self>, mut swarm: Swarm<EthBehaviour>) -> Result<(), Box<dyn Error>> {
        // All nodes are supposed to join the beacon_block topic
        let topic = GossipTopic::BeaconBlock;
        swarm.behaviour_mut().gossipsub.subscribe(&topic.into())?;

        // Create a channel to receive messages to publish
        let (tx, mut rx) = mpsc::channel::<Message>(PUBLISH_BUFFER_SIZE);
        let mut handles = Vec::new();

        // Spawn a job to keep seeing if it should publish blocks
        let cloned_node = Arc::clone(&self);
        let cloned_tx = tx.clone();
        handles.push(tokio::spawn(async move {
            cloned_node.propose(cloned_tx).await;
        }));

        let mut handles = futures::future::select_all(handles.into_iter());
        loop {
            tokio::select! {
                _ = &mut handles => {
                    panic!("some thread panicked");
                },
                Some(message) = rx.recv() => {
                    match swarm
                        .behaviour_mut()
                        .gossipsub
                        .publish(message.topic(), message.encode())
                    {
                        Ok(msgid) => println!("Published to {} msgid={} message={}",
                                              message.topic(), msgid, message),
                        Err(e) => println!("Publish error: {e:?}"),
                    }
                },
                event = swarm.select_next_some() => match event {
                    SwarmEvent::NewListenAddr { address, .. } => {
                        println!("Listening on {address}");
                    },
                    SwarmEvent::ConnectionEstablished { endpoint, .. } => {
                        println!("Connected to {} ({})",
                            endpoint.get_remote_address(),
                            if endpoint.is_dialer() { "outgoing" } else { "incoming" },
                        );
                    },
                    SwarmEvent::Behaviour(EthBehaviourEvent::Gossipsub(gossipsub::Event::Message {
                        message, ..
                    })) => {
                        let message = Message::decode(message.data.as_slice());
                        match &message.header {
                            MessageHeader::BeaconBlock(block) => {
                                println!("Got block from slot={} vid={} msgid={}",
                                     block.slot,
                                     block.proposer,
                                     message.id(),
                                );
                            },
                        }
                    },
                    _ => {},
                },
            }
        }
    }
}

// The overall Ethereum network view
struct EthNetwork {
    genesis_instant: Instant,
    num_validators: usize,
}

impl EthNetwork {
    fn new(num_validators: usize) -> Self {
        let genesis_time = std::time::UNIX_EPOCH + GENESIS_DURATION_SINCE_UNIX_EPOCH;
        let genesis_instant = match SystemTime::now().duration_since(genesis_time) {
            Ok(duration) => Instant::now().checked_sub(duration).unwrap(),
            Err(e) => Instant::now().checked_add(e.duration()).unwrap(),
        };

        Self {
            genesis_instant,
            num_validators,
        }
    }
    fn current_slot(&self) -> Slot {
        self.slot_of(Instant::now())
    }
    fn slot_of(&self, instant: Instant) -> Slot {
        let duration = instant.duration_since(self.genesis_instant());
        Slot::from(duration.as_secs() / SECONDS_PER_SLOT)
    }
    fn genesis_instant(&self) -> Instant {
        self.genesis_instant
    }
    // Get a proposer for a given slot
    fn proposer(&self, slot: Slot) -> ValidatorId {
        let mut rng = DigestRng::new(Digest::Proposer(slot));
        let id = rng.gen_range(0..self.num_validators);
        ValidatorId(id)
    }
}

#[derive(PartialEq, Eq, Copy, Clone)]
enum Digest {
    Proposer(Slot),
}

impl From<Digest> for [u8; 32] {
    fn from(digest: Digest) -> Self {
        let prehash = match digest {
            Digest::Proposer(slot) => format!("proposer/{}", slot),
        };
        let hash = ring::digest::digest(&ring::digest::SHA256, prehash.as_bytes());
        hash.as_ref()[..32].try_into().unwrap()
    }
}

struct DigestRng(ChaCha20Rng);

impl DigestRng {
    fn new(digest: Digest) -> Self {
        let rng = ChaCha20Rng::from_seed(digest.into());
        Self(rng)
    }
}

impl RngCore for DigestRng {
    fn next_u32(&mut self) -> u32 {
        self.0.next_u32()
    }
    fn next_u64(&mut self) -> u64 {
        self.0.next_u64()
    }
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        self.0.fill_bytes(dest)
    }
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand::Error> {
        self.0.try_fill_bytes(dest)
    }
}

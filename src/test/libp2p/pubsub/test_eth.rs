use futures::prelude::*;
use libp2p::gossipsub::{IdentTopic as Topic, TopicHash};
use libp2p::swarm::{NetworkBehaviour, Swarm, SwarmEvent};
use libp2p::{gossipsub, multiaddr::Protocol, noise, tcp, yamux, Multiaddr};
use rand::seq::SliceRandom;
use rand::{Rng, RngCore, SeedableRng};
use rand_chacha::ChaCha20Rng;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::{error::Error, net::Ipv4Addr, sync::Arc, time::SystemTime};
use tokio::sync::{mpsc, RwLock};
use tokio::time::{Duration, Instant};

// Ethereum network configs
const SLOTS_PER_EPOCH: u64 = 4;
const SECONDS_PER_SLOT: u64 = 12;
const MAX_COMMITTEES_PER_SLOT: usize = 2;
const TARGET_AGGREGATORS_PER_COMMITTEE: usize = 1;
const MAX_BLOBS_PER_BLOCK: usize = 6;
// Sat Jan 01 2000 00:01:00 GMT+0000
const GENESIS_DURATION_SINCE_UNIX_EPOCH: Duration = Duration::from_secs(946684860);

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
        #[derive(
            Serialize, Deserialize, Hash, Debug, PartialOrd, Ord, PartialEq, Eq, Copy, Clone,
        )]
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
        impl std::ops::Add<$ty> for $ident {
            type Output = Self;

            fn add(self, other: $ty) -> Self {
                Self(<$ty>::from(self) + other)
            }
        }
        impl std::ops::Sub<$ty> for $ident {
            type Output = Self;

            fn sub(self, other: $ty) -> Self {
                Self(<$ty>::from(self) - other)
            }
        }
    };
}

wrapper_impl!(ValidatorId, usize);
wrapper_impl!(CommitteeId, usize);
wrapper_impl!(BlobId, usize);
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
    BeaconAggregateAndProof,
    // Subnets are determined by the committee ids
    Attestation(CommitteeId),
    // Subnets are determined by the blob ids
    BlobSidecar(BlobId),
}

const BEACON_BLOCK_TOPIC: &str = "beacon_block";
const BEACON_AGGREGATE_AND_PROOF_TOPIC: &str = "beacon_aggregate_and_proof";
const BEACON_ATTESTATION_PREFIX: &str = "beacon_attestation_";
const BLOB_SIDECAR_PREFIX: &str = "blob_sidecar_";

impl std::fmt::Display for GossipTopic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let topic = match self {
            GossipTopic::BeaconBlock => BEACON_BLOCK_TOPIC.into(),
            GossipTopic::BeaconAggregateAndProof => BEACON_AGGREGATE_AND_PROOF_TOPIC.into(),
            GossipTopic::Attestation(index) => format!("{}{}", BEACON_ATTESTATION_PREFIX, index),
            GossipTopic::BlobSidecar(index) => format!("{}{}", BLOB_SIDECAR_PREFIX, index),
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

#[derive(Serialize, Deserialize, Clone)]
struct BeaconBlock {
    proposer: ValidatorId,
    slot: Slot,
    num_attestors: usize,
    num_blobs: usize,
}
#[derive(Serialize, Deserialize, Clone)]
struct Attestation {
    attestor: ValidatorId,
    slot: Slot,
    cmid: CommitteeId,
    block_slot: Slot,
}
#[derive(Serialize, Deserialize, Clone)]
struct Aggregate {
    aggregator: ValidatorId,
    slot: Slot,
    num_attestors: usize,
    cmid: CommitteeId,
    block_slot: Slot,
}
#[derive(Serialize, Deserialize, Clone)]
struct BlobSidecar {
    proposer: ValidatorId,
    slot: Slot,
    index: BlobId,
}
#[derive(Serialize, Deserialize)]
enum MessageHeader {
    BeaconBlock(BeaconBlock),
    Attestation(Attestation),
    Aggregate(Aggregate),
    BlobSidecar(BlobSidecar),
}
impl From<BeaconBlock> for MessageHeader {
    fn from(block: BeaconBlock) -> Self {
        Self::BeaconBlock(block)
    }
}
impl From<Attestation> for MessageHeader {
    fn from(attestation: Attestation) -> Self {
        Self::Attestation(attestation)
    }
}
impl From<Aggregate> for MessageHeader {
    fn from(aggregate: Aggregate) -> Self {
        Self::Aggregate(aggregate)
    }
}
impl From<BlobSidecar> for MessageHeader {
    fn from(blob_sidecar: BlobSidecar) -> Self {
        Self::BlobSidecar(blob_sidecar)
    }
}
struct Message {
    header: MessageHeader,
    padding: Vec<u8>,
}

const BEACON_BLOCK_PADDING_SIZE: usize = 128 << 10; // 128KB
const ATTESTATION_PADDING_SIZE: usize = 256;
const AGGREGATE_PADDING_SIZE: usize = 256;
const BLOB_SIDECAR_PADDING_SIZE: usize = 128 << 10; // 128KB;

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
            MessageHeader::Attestation(_) => ATTESTATION_PADDING_SIZE,
            MessageHeader::Aggregate(_) => AGGREGATE_PADDING_SIZE,
            MessageHeader::BlobSidecar(_) => BLOB_SIDECAR_PADDING_SIZE,
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
            MessageHeader::Attestation(Attestation { cmid, .. }) => GossipTopic::Attestation(cmid),
            MessageHeader::Aggregate(_) => GossipTopic::BeaconAggregateAndProof,
            MessageHeader::BlobSidecar(BlobSidecar { index, .. }) => {
                GossipTopic::BlobSidecar(index)
            }
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
    latest_received_block: RwLock<Option<BeaconBlock>>,
    latest_attestation: RwLock<Option<Attestation>>,
    seen_attestations: RwLock<Vec<Attestation>>,
    seen_aggregates: RwLock<Vec<Aggregate>>,

    latest_seen_blob_sidecar_slot: RwLock<Slot>,
    seen_blob_sidecars: RwLock<BTreeMap<BlobId, BlobSidecar>>,
}

impl EthNode {
    fn new(vid: ValidatorId, network: EthNetwork) -> Self {
        Self {
            vid,
            network,
            latest_received_block: RwLock::new(None),
            latest_attestation: RwLock::new(None),
            seen_attestations: RwLock::new(Vec::new()),
            seen_aggregates: RwLock::new(Vec::new()),

            latest_seen_blob_sidecar_slot: RwLock::new(Slot(0)),
            seen_blob_sidecars: RwLock::new(BTreeMap::new()),
        }
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
                // Collect all the aggregates the node has seen to count
                // the number of attestors
                let seen_aggregates = self.seen_aggregates.read().await;
                let mut collected = vec![false; MAX_COMMITTEES_PER_SLOT];
                let mut num_attestors = 0;
                for aggregate in seen_aggregates.as_slice() {
                    if aggregate.slot != slot - 1 {
                        continue;
                    }
                    let cmid = aggregate.cmid;
                    if !collected[usize::from(cmid)] {
                        collected[usize::from(cmid)] = true;
                        num_attestors += aggregate.num_attestors;
                    }
                }
                drop(seen_aggregates);
                // Clear the seen aggregates
                (*self.seen_aggregates.write().await).clear();

                let num_blobs = MAX_BLOBS_PER_BLOCK;
                // Create and send blobs
                // for blob in 0..num_blobs {
                //     let blob_sidecar = BlobSidecar {
                //         proposer: proposer_id,
                //         slot,
                //         index: BlobId::from(blob),
                //     };
                //     self.clone().on_blob_sidecar(&blob_sidecar).await;
                //     let message = Message::new(MessageHeader::from(blob_sidecar));
                //     if let Err(e) = tx.send(message).await {
                //         println!("Sending to publish error: {e:?}");
                //     }
                // }
                // Create and send the block
                let block = BeaconBlock {
                    proposer: proposer_id,
                    slot,
                    num_attestors,
                    num_blobs,
                };
                self.clone().on_block(&block).await;
                let message = Message::new(MessageHeader::from(block));
                if let Err(e) = tx.send(message).await {
                    println!("Sending to publish error: {e:?}");
                }
            }
        }
    }
    async fn attest(self: Arc<Self>, tx: mpsc::Sender<Message>) -> ! {
        let slot_duration = Duration::from_secs(SECONDS_PER_SLOT);
        let mut interval = tokio::time::interval_at(
            self.network.genesis_instant() + slot_duration / 3,
            slot_duration,
        );
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let current_slot = self.network.current_slot();
            // Figure out if the node is supposed to publish an attestation in this slot
            let (slot, cmid) = self.network.committee(self.vid, Epoch::from(current_slot));
            if slot == current_slot {
                let Some(ref latest_block) = *self.latest_received_block.read().await else {
                    continue;
                };
                // Check if the blobs of the latest block are available
                // if latest_block.slot < *self.latest_seen_blob_sidecar_slot.read().await {
                //     println!(
                //         "Not attesting: latest received blob is newer than the just received block"
                //     );
                //     continue;
                // }
                // if latest_block.slot > *self.latest_seen_blob_sidecar_slot.read().await {
                //     println!("Not attesting: no blob is available");
                //     continue;
                // }
                // let seen_blobs = self.seen_blob_sidecars.read().await;
                // let mut available = true;
                // for (index, blob) in seen_blobs.keys().enumerate() {
                //     if BlobId(index) != *blob {
                //         available = false;
                //         break;
                //     }
                // }
                // if seen_blobs.len() != latest_block.num_blobs {
                //     available = false;
                // }
                // if !available {
                //     println!("Not attesting: some blobs are not available");
                //     continue;
                // }
                // drop(seen_blobs);

                // Create and send the attestation
                let attestation = Attestation {
                    attestor: self.vid,
                    slot,
                    cmid,
                    block_slot: latest_block.slot,
                };
                // Save the latest attestation that it makes
                *self.latest_attestation.write().await = Some(attestation.clone());
                self.clone().on_attestation(&attestation).await;
                let message = Message::new(MessageHeader::from(attestation));
                if let Err(e) = tx.send(message).await {
                    println!("Sending to publish error: {e:?}");
                }
            }
        }
    }
    async fn aggregate(self: Arc<Self>, tx: mpsc::Sender<Message>) -> ! {
        let slot_duration = Duration::from_secs(SECONDS_PER_SLOT);
        let mut interval = tokio::time::interval_at(
            self.network.genesis_instant() + 2 * slot_duration / 3,
            slot_duration,
        );
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let Some(ref latest_attestation) = *self.latest_attestation.read().await else {
                continue;
            };
            let current_slot = self.network.current_slot();
            // Check if we already made an attestation in the current slot
            if latest_attestation.slot != current_slot {
                continue;
            }
            // Check if the node is supposed to be an aggregator
            let aggregators = self
                .network
                .aggregators(current_slot, latest_attestation.cmid);
            if !aggregators.contains(&self.vid) {
                continue;
            }
            // Filter only the seen attestations that matches the attestation it made
            let seen_attestations = self.seen_attestations.read().await;
            let num_attestors = seen_attestations
                .as_slice()
                .iter()
                .filter(|attestation| {
                    attestation.slot == latest_attestation.slot
                        && attestation.cmid == latest_attestation.cmid
                        && attestation.block_slot == latest_attestation.block_slot
                })
                .count();
            drop(seen_attestations);
            // Clear the seen attestations
            (*self.seen_attestations.write().await).clear();

            // Create and send the aggregate
            let aggregate = Aggregate {
                aggregator: self.vid,
                num_attestors,
                slot: latest_attestation.slot,
                cmid: latest_attestation.cmid,
                block_slot: latest_attestation.slot,
            };
            self.clone().on_aggregate(&aggregate).await;
            let message = Message::new(MessageHeader::from(aggregate));
            if let Err(e) = tx.send(message).await {
                println!("Sending to publish error: {e:?}");
            }
        }
    }
    async fn on_block(self: Arc<Self>, block: &BeaconBlock) {
        // Check if the received block is not older than the latest received block
        if let Some(ref latest_block) = *self.latest_received_block.read().await {
            if block.slot < latest_block.slot {
                println!("Received older block");
            }
        }
        *self.latest_received_block.write().await = Some(block.clone());
    }
    async fn on_attestation(self: Arc<Self>, attestation: &Attestation) {
        let current_slot = self.network.current_slot();
        let (slot, cmid) = self.network.committee(self.vid, Epoch::from(current_slot));
        // Check if the attestation is from the correct slot and committee
        if slot != attestation.slot || cmid != attestation.cmid {
            return;
        }
        // Check if the node is supposed to be an aggregator
        let aggregators = self.network.aggregators(slot, cmid);
        if !aggregators.contains(&self.vid) {
            return;
        }
        (*self.seen_attestations.write().await).push(attestation.clone());
    }
    async fn on_aggregate(self: Arc<Self>, aggregate: &Aggregate) {
        let current_slot = self.network.current_slot();
        let next_slot = current_slot + 1;
        // Check if the node is supposed to be a proposer in the next slot
        if self.network.proposer(next_slot) == self.vid {
            (*self.seen_aggregates.write().await).push(aggregate.clone());
        }
    }
    async fn on_blob_sidecar(self: Arc<Self>, blob_sidecar: &BlobSidecar) {
        // let latest_blob_slot = *self.latest_seen_blob_sidecar_slot.read().await;
        // if blob_sidecar.slot < latest_blob_slot {
        //     println!("Received older blob");
        // }
        // let mut seen_blob_sidecars = self.seen_blob_sidecars.write().await;
        // // If receive a newer blob, clear the cache
        // if blob_sidecar.slot > latest_blob_slot {
        //     (*seen_blob_sidecars).clear();
        // }
        // (*seen_blob_sidecars).insert(blob_sidecar.index, blob_sidecar.clone());
        // (*self.latest_seen_blob_sidecar_slot.write().await) = blob_sidecar.slot;
    }
    async fn run(self: Arc<Self>, mut swarm: Swarm<EthBehaviour>) -> Result<(), Box<dyn Error>> {
        // All nodes are supposed to join the beacon_block topic
        let topic = GossipTopic::BeaconBlock;
        swarm.behaviour_mut().gossipsub.subscribe(&topic.into())?;

        // FIXME: We don't have a peer discovery yet so we will join all the
        // topics even if we are not supposed to
        for cmid in 0..MAX_COMMITTEES_PER_SLOT {
            let cmid = CommitteeId::from(cmid);
            let topic = GossipTopic::Attestation(cmid);
            swarm.behaviour_mut().gossipsub.subscribe(&topic.into())?;
        }

        // FIXME: We are supposed to join this topic only when we are supposed to propose a block
        let topic = GossipTopic::BeaconAggregateAndProof;
        swarm.behaviour_mut().gossipsub.subscribe(&topic.into())?;

        // All nodes are supposed to join all the blob sidecar topics
        for index in 0..MAX_BLOBS_PER_BLOCK {
            let topic = GossipTopic::BlobSidecar(BlobId::from(index));
            swarm.behaviour_mut().gossipsub.subscribe(&topic.into())?;
        }

        // Create a channel to receive messages to publish
        let (tx, mut rx) = mpsc::channel::<Message>(PUBLISH_BUFFER_SIZE);
        let mut handles = Vec::new();

        // Spawn a job to keep seeing if it should publish blocks
        let cloned_node = Arc::clone(&self);
        let cloned_tx = tx.clone();
        handles.push(tokio::spawn(async move {
            cloned_node.propose(cloned_tx).await;
        }));
        // Spawn a job to publish attestations
        let cloned_node = Arc::clone(&self);
        let cloned_tx = tx.clone();
        handles.push(tokio::spawn(async move {
            cloned_node.attest(cloned_tx).await;
        }));
        // Spawn a job to aggregate attestations
        let cloned_node = Arc::clone(&self);
        let cloned_tx = tx.clone();
        handles.push(tokio::spawn(async move {
            cloned_node.aggregate(cloned_tx).await;
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
                                self.clone().on_block(block).await;
                                println!("Got block from slot={} vid={} msgid={}",
                                     block.slot,
                                     block.proposer,
                                     message.id(),
                                );
                            },
                            MessageHeader::Attestation(attestation) => {
                                self.clone().on_attestation(attestation).await;
                            },
                            MessageHeader::Aggregate(aggregate) => {
                                self.clone().on_aggregate(aggregate).await;
                            },
                            MessageHeader::BlobSidecar(blob_sidecar) => {
                                self.clone().on_blob_sidecar(blob_sidecar).await;
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
    // Get aggregators of a committee for the given slot committee id
    fn aggregators(&self, slot: Slot, cmid: CommitteeId) -> Vec<ValidatorId> {
        let mut members = self.committee_members(slot, cmid);
        let mut rng = DigestRng::new(Digest::Aggregator(slot, cmid));
        // Shuffle the members
        members.as_mut_slice().shuffle(&mut rng);
        // Assign only the first as aggregators
        members.truncate(TARGET_AGGREGATORS_PER_COMMITTEE);
        members
    }
    // Get members of a committee for the given slot committee id
    fn committee_members(&self, slot: Slot, cmid: CommitteeId) -> Vec<ValidatorId> {
        let epoch = Epoch::from(slot);
        let mut rng = DigestRng::new(Digest::Committee(epoch));

        // Shuffle all the validators
        let mut permutation: Vec<usize> = (0..self.num_validators).collect::<Vec<_>>();
        permutation.as_mut_slice().shuffle(&mut rng);

        let mut reverted_perm = vec![0; self.num_validators];
        for (vid, idx) in permutation.into_iter().enumerate() {
            reverted_perm[idx] = vid;
        }

        let mut result = vec![];
        let slot_in_epoch = (u64::from(slot) % SLOTS_PER_EPOCH) as usize;
        let mut vid = usize::from(cmid) + slot_in_epoch * MAX_COMMITTEES_PER_SLOT;
        while vid < self.num_validators {
            result.push(ValidatorId::from(reverted_perm[vid]));
            vid += MAX_COMMITTEES_PER_SLOT * SLOTS_PER_EPOCH as usize;
        }
        // FIXME: The following for loop is very slow. Remove it when you have this project
        // outside of the Shadow tests
        for &vid in &result {
            let (actual_slot, actual_cmid) = self.committee(vid, epoch);
            assert_eq!(actual_slot, slot);
            assert_eq!(actual_cmid, cmid);
        }
        result
    }
    // Every validator is supposed to make an attestation in every epoch, so it should return the
    // slot and the committee id
    fn committee(&self, vid: ValidatorId, epoch: Epoch) -> (Slot, CommitteeId) {
        let mut rng = DigestRng::new(Digest::Committee(epoch));
        let vid = usize::from(vid);

        // Shuffle all the validators
        let mut permutation: Vec<usize> = (0..self.num_validators).collect::<Vec<_>>();
        permutation.as_mut_slice().shuffle(&mut rng);

        let idx = permutation[vid] % (MAX_COMMITTEES_PER_SLOT * SLOTS_PER_EPOCH as usize);
        let slot_in_epoch = (idx / MAX_COMMITTEES_PER_SLOT) as u64;
        let slot = Slot::from(slot_in_epoch + u64::from(epoch) * SLOTS_PER_EPOCH);
        let cmid = CommitteeId::from(idx % MAX_COMMITTEES_PER_SLOT);
        (slot, cmid)
    }
}

#[derive(PartialEq, Eq, Copy, Clone)]
enum Digest {
    Proposer(Slot),
    Committee(Epoch),
    Aggregator(Slot, CommitteeId),
}

impl From<Digest> for [u8; 32] {
    fn from(digest: Digest) -> Self {
        let prehash = match digest {
            Digest::Proposer(slot) => format!("proposer/{}", slot),
            Digest::Committee(epoch) => format!("committee/{}", epoch),
            Digest::Aggregator(slot, cmid) => format!("aggregator/{}/{}", slot, cmid),
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

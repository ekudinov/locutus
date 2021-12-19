//! The main node data type which encapsulates all the behaviour for maintaining a connection
//! and performing operations within the network.
//!
//! # Implementations
//! Node comes with different underlying implementations that can be used upon construction.
//! Those implementations are:
//! - libp2p: all the connection is handled by libp2p.
//! - In memory: a simplifying node used for emulation pourpouses mainly.

use std::{fmt::Display, net::IpAddr, sync::Arc};

use libp2p::{
    core::PublicKey,
    identity::{self, Keypair},
    multiaddr::Protocol,
    Multiaddr, PeerId,
};

#[cfg(test)]
use self::in_memory::NodeInMemory;
use self::{libp2p_impl::NodeLibP2P, event_listener::{EventListener, EventLog}};
use crate::{
    config::{GlobalExecutor, CONF},
    contract::{CHandlerImpl, ContractError, ContractStoreError},
    message::{Maintenance, Message},
    operations::{get, put, subscribe, OpError, join_ring},
    ring::{Location, PeerKeyLocation},
    user_events::{UserEvent, UserEventsProxy},
};
#[cfg(test)]
use crate::{contract::SimStoreError, user_events::test_utils::MemoryEventsGen};

pub(crate) use conn_manager::{ConnectionBridge, ConnectionError};
pub(crate) use op_state::OpManager;

mod conn_manager;
mod event_listener;
#[cfg(test)]
mod in_memory;
mod libp2p_impl;
mod op_state;
#[cfg(test)]
pub(crate) mod test_utils;

pub struct Node<CErr>(NodeImpl<CErr>);

#[cfg(test)]
impl<CErr> Node<CErr>
where
    CErr: std::error::Error + Send + Sync + 'static,
{
    pub async fn run(mut self) -> Result<(), anyhow::Error> {
        match self.0 {
            NodeImpl::LibP2P(node) => node.run_node().await,
            NodeImpl::InMemory(ref mut node) => {
                let (_usr_ev_controller, rcv_copy) =
                    tokio::sync::watch::channel((0, PeerKey::random()));
                let user_events = MemoryEventsGen::new(rcv_copy, node.peer_key);
                node.run_node(user_events).await
            }
        }
    }
}

#[cfg(not(test))]
impl<CErr> Node<CErr>
where
    CErr: std::error::Error + Send + Sync + 'static,
{
    pub async fn run(self) -> Result<(), anyhow::Error> {
        match self.0 {
            NodeImpl::LibP2P(node) => node.run_node().await,
        }
    }
}

#[cfg(test)]
enum NodeImpl<CErr> {
    LibP2P(Box<NodeLibP2P<CErr>>),
    InMemory(Box<NodeInMemory<CErr>>),
}

#[cfg(not(test))]
enum NodeImpl<CErr> {
    LibP2P(Box<NodeLibP2P<CErr>>),
}

/// When instancing a node you can either join an existing network or bootstrap a new network with a listener
/// which will act as the initial provider. This initial peer will be listening at the provided port and assigned IP.
/// If those are not free the instancing process will return an error.
///
/// In order to bootstrap a new network the following arguments are required to be provided to the builder:
/// - ip: IP associated to the initial node.
/// - port: listening port of the initial node.
///
/// If both are provided but also additional peers are added via the [`Self::add_provider()`] method, this node will
/// be listening but also try to connect to an existing peer.
#[derive(Clone)]
pub struct NodeConfig {
    /// local peer private key in
    pub(crate) local_key: identity::Keypair,
    // optional local info, in case this is an initial bootstrap node
    /// IP to bind to the listener
    pub(crate) local_ip: Option<IpAddr>,
    /// socket port to bind to the listener
    pub(crate) local_port: Option<u16>,
    /// At least an other running listener node is required for joining the network.
    /// Not necessary if this is an initial node.
    pub(crate) remote_nodes: Vec<InitPeerNode>,
    /// the location of this node, used for gateways.
    pub(crate) location: Option<Location>,
    pub(crate) max_hops_to_live: Option<usize>,
    pub(crate) rnd_if_htl_above: Option<usize>,
    pub(crate) max_number_conn: Option<usize>,
    pub(crate) min_number_conn: Option<usize>,
}

impl NodeConfig {
    pub fn new() -> NodeConfig {
        let local_key = if let Some(key) = &CONF.local_peer_keypair {
            key.clone()
        } else {
            identity::Keypair::generate_ed25519()
        };
        NodeConfig {
            local_key,
            remote_nodes: Vec::with_capacity(1),
            local_ip: None,
            local_port: None,
            location: None,
            max_hops_to_live: None,
            rnd_if_htl_above: None,
            max_number_conn: None,
            min_number_conn: None,
        }
    }

    pub fn max_hops_to_live(&mut self, num_hops: usize) -> &mut Self {
        self.max_hops_to_live = Some(num_hops);
        self
    }

    pub fn rnd_if_htl_above(&mut self, num_hops: usize) -> &mut Self {
        self.rnd_if_htl_above = Some(num_hops);
        self
    }

    pub fn max_number_of_connections(&mut self, num: usize) -> &mut Self {
        self.max_number_conn = Some(num);
        self
    }

    pub fn min_number_of_connections(&mut self, num: usize) -> &mut Self {
        self.min_number_conn = Some(num);
        self
    }

    pub fn with_port(&mut self, port: u16) -> &mut Self {
        self.local_port = Some(port);
        self
    }

    pub fn with_ip<T: Into<IpAddr>>(&mut self, ip: T) -> &mut Self {
        self.local_ip = Some(ip.into());
        self
    }

    /// Optional identity key of this node.
    /// If not provided it will be either obtained from the configuration or freshly generated.
    pub fn with_key(&mut self, key: identity::Keypair) -> &mut Self {
        self.local_key = key;
        self
    }

    pub fn with_location(&mut self, loc: Location) -> &mut Self {
        self.location = Some(loc);
        self
    }

    /// Connection info for an already existing peer. Required in case this is not a gateway node.
    pub fn add_gateway(&mut self, peer: InitPeerNode) -> &mut Self {
        self.remote_nodes.push(peer);
        self
    }

    /// Builds a node using libp2p as backend connection manager.
    pub fn build_libp2p(self) -> Result<Node<ContractStoreError>, anyhow::Error> {
        let node = NodeLibP2P::<ContractStoreError>::build::<CHandlerImpl>(self)?;
        Ok(Node(NodeImpl::LibP2P(Box::new(node))))
    }

    #[cfg(test)]
    /// Builds a node using in-memory transport. Used for testing pourpouses.
    pub(crate) fn build_in_memory(self) -> Result<Node<SimStoreError>, anyhow::Error> {
        use self::event_listener::TestEventListener;
        use crate::contract::MemoryContractHandler;

        let listener = Box::new(TestEventListener::new());
        let in_mem =
            NodeInMemory::<SimStoreError>::build::<MemoryContractHandler>(self, Some(listener))?;
        Ok(Node(NodeImpl::InMemory(Box::new(in_mem))))
    }

    /// Returns all specified gateways for this peer. Returns an error if the peer is not a gateway
    /// and no gateways are specified.
    fn get_gateways(&self) -> Result<Vec<PeerKeyLocation>, anyhow::Error> {
        let peer = PeerKey::from(self.local_key.public());
        let gateways: Vec<_> = self
            .remote_nodes
            .iter()
            .filter_map(|node| {
                if node.addr.is_some() {
                    Some(PeerKeyLocation {
                        peer: PeerKey::from(node.identifier),
                        location: Some(node.location),
                    })
                } else {
                    None
                }
            })
            .filter(|pkloc| pkloc.peer != peer)
            .collect();
        if (self.local_ip.is_none() || self.local_port.is_none()) && gateways.is_empty() {
            anyhow::bail!(
                        "At least one remote gateway is required to join an existing network for non-gateway nodes."
                    )
        } else {
            Ok(gateways)
        }
    }
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Gateway node to bootstrap the network.
#[derive(Clone)]
pub struct InitPeerNode {
    addr: Option<Multiaddr>,
    identifier: PeerId,
    location: Location,
}

impl InitPeerNode {
    pub fn new(identifier: PeerId, location: Location) -> Self {
        Self {
            addr: None,
            identifier,
            location,
        }
    }

    /// Given a byte array decode into a PeerId data type.
    ///
    /// # Panic
    /// Will panic if is not a valid representation.
    pub fn decode_peer_id<T: AsMut<[u8]>>(mut bytes: T) -> PeerId {
        PeerId::from_public_key(
            &identity::Keypair::Ed25519(
                identity::ed25519::Keypair::decode(bytes.as_mut()).unwrap(),
            )
            .public(),
        )
    }

    /// IP which will be assigned to this node.
    pub fn listening_ip<T: Into<IpAddr>>(mut self, ip: T) -> Self {
        if let Some(addr) = &mut self.addr {
            addr.push(Protocol::from(ip.into()));
        } else {
            self.addr = Some(Multiaddr::from(ip.into()));
        }
        self
    }

    /// TCP listening port (only required in case of using TCP as transport).
    /// If not specified port 7800 will be used as default.
    pub fn listening_port(mut self, port: u16) -> Self {
        if let Some(addr) = &mut self.addr {
            addr.push(Protocol::Tcp(port));
        } else {
            self.addr = Some(Multiaddr::from(Protocol::Tcp(port)));
        }
        self
    }
}

/// Process user events.
async fn user_event_handling<UsrEv, CErr>(op_storage: Arc<OpManager<CErr>>, mut user_events: UsrEv)
where
    UsrEv: UserEventsProxy + Send + Sync + 'static,
    CErr: std::error::Error + Send + Sync + 'static,
{
    loop {
        let ev = user_events.recv().await;
        if let UserEvent::Shutdown = ev {
            if let Err(err) = op_storage
                .notify_maintenance_op(Message::Maintenance(Maintenance::ShutdownNode))
                .await
            {
                log::error!("{}", err);
            }
            break;
        }

        let op_storage_cp = op_storage.clone();
        GlobalExecutor::spawn(async move {
            match ev {
                UserEvent::Put { value, contract } => {
                    // Initialize a put op.
                    log::debug!(
                        "Received put from user event @ {}",
                        &op_storage_cp.ring.peer_key
                    );
                    let op = put::PutOp::start_op(
                        contract,
                        value,
                        op_storage_cp.ring.max_hops_to_live,
                        &op_storage_cp.ring.peer_key,
                    );
                    if let Err(err) = put::request_put(&op_storage_cp, op).await {
                        log::error!("{}", err);
                    }
                }
                UserEvent::Get { key, contract } => {
                    // Initialize a get op.
                    log::debug!(
                        "Received get from user event @ {}",
                        &op_storage_cp.ring.peer_key
                    );
                    let op = get::GetOp::start_op(key, contract, &op_storage_cp.ring.peer_key);
                    if let Err(err) = get::request_get(&op_storage_cp, op).await {
                        log::error!("{}", err);
                    }
                }
                UserEvent::Subscribe { key } => {
                    // Initialize a subscribe op.
                    loop {
                        // fixme: this will block the event loop until the subscribe op succeeds
                        // instead the op should be deferred for later execution
                        let op =
                            subscribe::SubscribeOp::start_op(key, &op_storage_cp.ring.peer_key);
                        match subscribe::request_subscribe(&op_storage_cp, op).await {
                            Err(OpError::ContractError(ContractError::ContractNotFound(key))) => {
                                log::warn!("Trying to subscribe to a contract not present: {}, requesting it first", key);
                                let get_op =
                                    get::GetOp::start_op(key, true, &op_storage_cp.ring.peer_key);
                                if let Err(err) = get::request_get(&op_storage_cp, get_op).await {
                                    log::error!("Failed getting the contract `{}` while previously trying to subscribe; bailing: {}", key, err);
                                }
                            }
                            Err(err) => {
                                log::error!("{}", err);
                                break;
                            }
                            Ok(()) => break,
                        }
                    }
                }
                UserEvent::Shutdown => unreachable!(),
            }
        });
    }
}

macro_rules! log_handling_msg {
    ($op:expr, $id:expr, $op_storage:ident) => {
        log::debug!(
            concat!("Handling ", $op, " get request @ {} (tx: {})"),
            $op_storage.ring.peer_key,
            $id
        );
    };
}

#[inline(always)]
fn report_result<CErr>(op_result: Result<(), OpError<CErr>>)
where
    CErr: std::error::Error,
{
    if let Err(err) = op_result {
        log::debug!("Finished tx w/ error: {}", err)
    }
}

async fn process_message<CErr, CB>(
    msg: Result<Message, ConnectionError>,
    op_storage: Arc<OpManager<CErr>>,
    mut conn_manager: CB,
    event_listener: Option<Box<dyn EventListener + Send + Sync>>,
) where
    CB: ConnectionBridge,
    CErr: std::error::Error + Sync + Send + 'static,
{
    match msg {
        Ok(msg) => {
            if let Some(mut listener) = event_listener {
                listener.event_received(EventLog::new(&msg, &op_storage));
            }
            match msg {
                Message::JoinRing(op) => {
                    log_handling_msg!("join", op.id(), op_storage);
                    let op_result =
                        join_ring::handle_join_ring(&op_storage, &mut conn_manager, op).await;
                    report_result(op_result);
                }
                Message::Put(op) => {
                    log_handling_msg!("put", op.id(), op_storage);
                    let op_result =
                        put::handle_put_request(&op_storage, &mut conn_manager, op).await;
                    report_result(op_result);
                }
                Message::Get(op) => {
                    log_handling_msg!("get", op.id(), op_storage);
                    let op_result =
                        get::handle_get_request(&op_storage, &mut conn_manager, op).await;
                    report_result(op_result);
                }
                Message::Subscribe(op) => {
                    log_handling_msg!("subscribe", op.id(), op_storage);
                    let op_result =
                        subscribe::handle_subscribe_response(&op_storage, &mut conn_manager, op)
                            .await;
                    report_result(op_result);
                }
                _ => {}
            }
        }
        Err(err) => {
            report_result::<CErr>(Err(err.into()));
        }
    }
}

#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy, PartialOrd, Ord)]
pub struct PeerKey(PeerId);

impl PeerKey {
    pub fn random() -> Self {
        PeerKey::from(Keypair::generate_ed25519().public())
    }

    pub fn to_bytes(self) -> Vec<u8> {
        self.0.to_bytes()
    }
}

impl Display for PeerKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<PublicKey> for PeerKey {
    fn from(val: PublicKey) -> Self {
        PeerKey(PeerId::from(val))
    }
}

impl From<PeerId> for PeerKey {
    fn from(val: PeerId) -> Self {
        PeerKey(val)
    }
}

mod serialization {
    use libp2p::PeerId;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::PeerKey;

    impl Serialize for PeerKey {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            serializer.serialize_bytes(&self.0.to_bytes())
        }
    }

    impl<'de> Deserialize<'de> for PeerKey {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: Deserializer<'de>,
        {
            let bytes: Vec<u8> = Deserialize::deserialize(deserializer)?;
            Ok(PeerKey(
                PeerId::from_bytes(&bytes).expect("failed deserialization of PeerKey"),
            ))
        }
    }
}

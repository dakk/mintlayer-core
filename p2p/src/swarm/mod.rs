// Copyright (c) 2022 RBB S.r.l
// opensource@mintlayer.org
// SPDX-License-Identifier: MIT
// Licensed under the MIT License;
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// 	http://spdx.org/licenses/MIT
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//
// Author(s): A. Altonen
use crate::{
    error::{self, FatalError, P2pError, ProtocolError},
    event,
    net::{self, ConnectivityService, NetworkingService},
};
use common::chain::ChainConfig;
use futures::FutureExt;
use logging::log;
use std::{
    collections::{HashMap, HashSet},
    fmt::Debug,
    str::FromStr,
    sync::Arc,
};
use tokio::sync::mpsc;

const MAX_ACTIVE_CONNECTIONS: usize = 32;

// TODO: store active address
// TODO: store other discovered addresses
#[derive(Debug)]
struct PeerContext<T>
where
    T: NetworkingService,
{
    _info: net::PeerInfo<T>,
}

enum PeerAddrInfo<T>
where
    T: NetworkingService,
{
    Raw {
        /// Hashset of IPv4 addresses
        ip4: HashSet<Arc<T::Address>>,

        /// Hashset of IPv6 addresses
        ip6: HashSet<Arc<T::Address>>,
    },
}

pub struct PeerManager<T>
where
    T: NetworkingService,
{
    /// Chain config
    config: Arc<ChainConfig>,

    /// Handle for sending/receiving connectivity events
    handle: T::ConnectivityHandle,

    /// Hashmap for peer information
    peers: HashMap<T::PeerId, PeerContext<T>>,

    /// Hashmap of discovered peers we don't have an active connection with
    discovered: HashMap<T::PeerId, PeerAddrInfo<T>>,

    /// RX channel for receiving control events
    rx_swarm: mpsc::Receiver<event::SwarmEvent<T>>,

    /// TX channel for sending events to SyncManager
    tx_sync: mpsc::Sender<event::SyncControlEvent<T>>,
}

impl<T> PeerManager<T>
where
    T: NetworkingService + 'static,
    T::ConnectivityHandle: ConnectivityService<T>,
    <T as NetworkingService>::Address: FromStr,
    <<T as NetworkingService>::Address as FromStr>::Err: Debug,
{
    pub fn new(
        config: Arc<ChainConfig>,
        handle: T::ConnectivityHandle,
        rx_swarm: mpsc::Receiver<event::SwarmEvent<T>>,
        tx_sync: mpsc::Sender<event::SyncControlEvent<T>>,
    ) -> Self {
        Self {
            config,
            handle,
            rx_swarm,
            tx_sync,
            peers: HashMap::with_capacity(MAX_ACTIVE_CONNECTIONS),
            discovered: HashMap::new(),
        }
    }

    /// Handle swarm control event
    async fn on_swarm_control_event(
        &mut self,
        event: Option<event::SwarmEvent<T>>,
    ) -> error::Result<()> {
        match event.ok_or(P2pError::ChannelClosed)? {
            event::SwarmEvent::Connect(addr, response) => {
                log::debug!(
                    "try to establish outbound connection to peer at address {:?}",
                    addr
                );

                match self.handle.connect(addr.clone()).await {
                    Ok(_info) => {
                        let peer_id = _info.peer_id;
                        match self.peers.insert(peer_id, PeerContext { _info }) {
                            Some(_) => {
                                log::error!("peer already exists");
                                response
                                    .send(Err(P2pError::PeerExists))
                                    .map_err(|_| P2pError::ChannelClosed)
                            }
                            None => {
                                log::warn!("peer count: {:?}", self.peers.len());
                                log::info!(
                                    "connection established successfully to peer {:?}",
                                    addr
                                );
                                self.tx_sync
                                    .send(event::SyncControlEvent::Connected(peer_id))
                                    .await
                                    .map_err(P2pError::from)?;
                                response.send(Ok(())).map_err(|_| P2pError::ChannelClosed)
                            }
                        }
                    }
                    Err(err) => {
                        log::error!("failed to establish outbound connection: {:?}", err);
                        response.send(Err(err)).map_err(|_| P2pError::ChannelClosed)
                    }
                }
            }
            // TODO: pending disconnection events
            event::SwarmEvent::Disconnect(peer_id, response) => response
                .send(self.handle.disconnect(peer_id).await)
                .map_err(|_| P2pError::ChannelClosed),
            event::SwarmEvent::GetPeerCount(response) => {
                response.send(self.peers.len()).map_err(|_| P2pError::ChannelClosed)
            }
            event::SwarmEvent::GetBindAddress(response) => response
                .send(self.handle.local_addr().to_string())
                .map_err(|_| P2pError::ChannelClosed),
            event::SwarmEvent::GetPeerId(response) => response
                .send(self.handle.peer_id().to_string())
                .map_err(|_| P2pError::ChannelClosed),
            event::SwarmEvent::GetConnectedPeers(response) => {
                let peers = self.peers.iter().map(|(id, _)| id.to_string()).collect::<Vec<_>>();
                response.send(peers).map_err(|_| P2pError::ChannelClosed)
            }
        }
    }

    /// Try to establish new outbound connections if the total number of
    /// active connections the local node has is below threshold
    ///
    // TODO: ugly, refactor
    // TODO: move this to its own file?
    #[allow(dead_code)]
    async fn auto_connect(&mut self) -> error::Result<()> {
        // we have enough active connections
        if self.peers.len() >= MAX_ACTIVE_CONNECTIONS {
            return Ok(());
        }
        log::debug!("try to establish more outbound connections");

        // we don't know of any peers
        if self.discovered.is_empty() {
            log::error!(
                "# of connections below threshold ({} < {}) but no peers",
                self.peers.len(),
                MAX_ACTIVE_CONNECTIONS,
            );
            return Err(P2pError::NoPeers);
        }

        let npeers = std::cmp::min(
            self.discovered.len(),
            MAX_ACTIVE_CONNECTIONS - self.peers.len(),
        );

        // TODO: improve peer selection
        let mut iter = self.discovered.iter();

        #[allow(clippy::needless_collect)]
        let peers: Vec<(T::PeerId, Arc<T::Address>)> = (0..npeers)
            .map(|i| {
                let peer_info = iter.nth(i).expect("Peer to exist");

                let (ip4, ip6) = match peer_info.1 {
                    PeerAddrInfo::Raw { ip4, ip6 } => (ip4, ip6),
                };
                assert!(!ip4.is_empty() || !ip6.is_empty());

                // TODO: let user specify their preference?
                let addr = if ip6.is_empty() {
                    Arc::clone(ip4.iter().next().expect("ip4 empty"))
                } else {
                    Arc::clone(ip6.iter().next().expect("ip6 empty"))
                };

                (*peer_info.0, addr)
            })
            .collect::<_>();

        for (id, addr) in peers {
            log::trace!("try to connect to peer {:?}, address {:?}", id, addr);

            // TODO: don't remove entry but modify it
            self.discovered.remove(&id);
            self.handle
                .connect((*addr).clone())
                .await
                .map(|_info| {
                    let id = _info.peer_id;
                    match self.peers.insert(id, PeerContext { _info }) {
                        Some(_) => panic!("peer already exists"),
                        None => {}
                    }
                })
                .map_err(|err| {
                    log::error!("failed to establish outbound connection: {:?}", err);
                    err
                })?;
        }

        Ok(())
    }

    /// Update the list of peers we know about or update a known peers list of addresses
    fn peer_discovered(&mut self, peers: &[net::AddrInfo<T>]) -> error::Result<()> {
        log::info!("discovered {} new peers", peers.len());

        for info in peers.iter() {
            // TODO: update peer stats
            if self.peers.contains_key(&info.id) {
                continue;
            }

            match self.discovered.entry(info.id).or_insert_with(|| PeerAddrInfo::Raw {
                ip4: HashSet::new(),
                ip6: HashSet::new(),
            }) {
                PeerAddrInfo::Raw { ip4, ip6 } => {
                    log::trace!("discovered ipv4 {:#?}, ipv6 {:#?}", ip4, ip6);

                    ip4.extend(info.ip4.clone());
                    ip6.extend(info.ip6.clone());
                }
            }
        }

        Ok(())
    }

    // TODO: implement
    fn peer_expired(&mut self, _peers: &[net::AddrInfo<T>]) -> error::Result<()> {
        Ok(())
    }

    /// Handle network event received from the network service provider
    async fn on_network_event(&mut self, event: net::ConnectivityEvent<T>) -> error::Result<()> {
        match event {
            net::ConnectivityEvent::IncomingConnection { peer_info, addr } => {
                let peer_id = peer_info.peer_id;
                log::debug!(
                    "incoming connection from peer {:?}, address {:?}",
                    peer_id,
                    addr
                );

                if self.peers.get(&peer_id).is_some() {
                    log::error!("peer {:?} re-established connection", peer_id);
                    return self.handle.disconnect(peer_id).await;
                }

                if self.peers.len() == MAX_ACTIVE_CONNECTIONS {
                    log::warn!("maximum number of connections reached, close new connection with peer {:?}", peer_id);
                    // TODO: save peer information for later?
                    // TODO: i.e., consider this a peer discovery event?
                    return self.handle.disconnect(peer_id).await;
                }

                if peer_info.magic_bytes != *self.config.magic_bytes() {
                    log::error!(
                        "peer {:?} is in different network, ours {:?}, theirs {:?}",
                        peer_id,
                        peer_info.magic_bytes,
                        self.config.chain_type()
                    );
                    return Err(P2pError::ProtocolError(ProtocolError::DifferentNetwork));
                }

                // TODO: check supported protocols
                // TODO: check version

                self.peers.insert(peer_id, PeerContext { _info: peer_info });
                self.tx_sync
                    .send(event::SyncControlEvent::Connected(peer_id))
                    .await
                    .map_err(P2pError::from)
            }
            net::ConnectivityEvent::ConnectionAccepted { peer_info } => {
                let peer_id = peer_info.peer_id;
                log::debug!("outbound connection accepted by peer {:?}", peer_id);

                if self.peers.get(&peer_id).is_some() {
                    log::error!("peer {:?} re-established connection", peer_id);
                    return self.handle.disconnect(peer_id).await;
                }

                if self.peers.len() == MAX_ACTIVE_CONNECTIONS {
                    log::warn!("maximum number of connections reached, close new connection with peer {:?}", peer_id);
                    // TODO: save peer information for later?
                    // TODO: i.e., consider this a peer discovery event?
                    return self.handle.disconnect(peer_id).await;
                }

                if peer_info.magic_bytes != *self.config.magic_bytes() {
                    log::error!(
                        "peer {:?} is in different network, ours {:?}, theirs {:?}",
                        peer_id,
                        peer_info.magic_bytes,
                        self.config.chain_type()
                    );
                    return Err(P2pError::ProtocolError(ProtocolError::DifferentNetwork));
                }

                // TODO: check supported protocols
                // TODO: check version

                self.peers.insert(peer_id, PeerContext { _info: peer_info });
                self.tx_sync
                    .send(event::SyncControlEvent::Connected(peer_id))
                    .await
                    .map_err(P2pError::from)
            }
            net::ConnectivityEvent::ConnectionClosed { peer_id } => {
                log::debug!("connection closed for peer {:?}", peer_id);
                self.tx_sync
                    .send(event::SyncControlEvent::Disconnected(peer_id))
                    .await
                    .map_err(P2pError::from)?;
                self.peers.remove(&peer_id);
                Ok(())
            }
            net::ConnectivityEvent::Discovered { peers } => self.peer_discovered(&peers),
            net::ConnectivityEvent::Expired { peers } => self.peer_expired(&peers),
            net::ConnectivityEvent::Disconnected { .. } => Ok(()),
            net::ConnectivityEvent::Misbehaved { .. } => Ok(()),
            net::ConnectivityEvent::Error { .. } => Ok(()),
        }
    }

    /// PeerManager event loop
    pub async fn run(&mut self) -> error::Result<()> {
        loop {
            tokio::select! {
                event = self.rx_swarm.recv().fuse() => {
                    self.on_swarm_control_event(event).await.map_fatal_err()?;
                }
                event = self.handle.poll_next() => match event {
                    Ok(event) => self.on_network_event(event).await.map_fatal_err()?,
                    Err(e) => {
                        log::error!("failed to read network event: {:?}", e);
                        return Err(e);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{error::P2pError, event};
    use common::chain::config;
    use libp2p::{multiaddr::Protocol, Multiaddr, PeerId};
    use net::{libp2p::Libp2pService, mock::MockService, ConnectivityService};
    use std::net::SocketAddr;
    use tokio::sync::oneshot;

    async fn make_swarm_manager<T>(
        addr: T::Address,
        config: Arc<common::chain::ChainConfig>,
    ) -> PeerManager<T>
    where
        T: NetworkingService + 'static,
        T::ConnectivityHandle: ConnectivityService<T>,
        <T as NetworkingService>::Address: FromStr,
        <<T as NetworkingService>::Address as FromStr>::Err: Debug,
    {
        let (conn, _, _) = T::start(
            addr,
            &[],
            &[],
            Arc::clone(&config),
            std::time::Duration::from_secs(10),
        )
        .await
        .unwrap();
        let (_, rx) = tokio::sync::mpsc::channel(16);
        let (tx_sync, mut rx_sync) = tokio::sync::mpsc::channel(16);

        tokio::spawn(async move {
            loop {
                let _ = rx_sync.recv().await;
            }
        });

        PeerManager::<T>::new(Arc::clone(&config), conn, rx, tx_sync)
    }

    // try to connect to an address that no one listening on and verify it fails
    #[tokio::test]
    async fn test_swarm_connect_mock() {
        let addr: SocketAddr = test_utils::make_address("[::1]:");
        let config = Arc::new(config::create_mainnet());
        let mut swarm = make_swarm_manager::<MockService>(addr, config).await;
        let (tx, rx) = oneshot::channel();

        let addr: SocketAddr = "[::1]:1".parse().unwrap();
        swarm
            .on_swarm_control_event(Some(event::SwarmEvent::Connect(addr, tx)))
            .await
            .unwrap();
        assert_eq!(
            rx.await.unwrap(),
            Err(P2pError::SocketError(std::io::ErrorKind::ConnectionRefused))
        );
    }

    // try to connect to an address that no one listening on and verify it fails
    #[tokio::test]
    async fn test_swarm_connect_libp2p() {
        let addr: Multiaddr = test_utils::make_address("/ip6/::1/tcp/");
        let config = Arc::new(config::create_mainnet());
        let mut swarm = make_swarm_manager::<Libp2pService>(addr, config).await;
        let (tx, rx) = oneshot::channel();

        let addr: Multiaddr =
            "/ip6/::1/tcp/6666/p2p/12D3KooWRn14SemPVxwzdQNg8e8Trythiww1FWrNfPbukYBmZEbJ"
                .parse()
                .unwrap();
        swarm
            .on_swarm_control_event(Some(event::SwarmEvent::Connect(addr, tx)))
            .await
            .unwrap();
        assert_eq!(
            rx.await.unwrap(),
            Err(P2pError::SocketError(std::io::ErrorKind::ConnectionRefused))
        );
    }

    #[tokio::test]
    async fn test_peer_discovered_libp2p() {
        let addr: Multiaddr = test_utils::make_address("/ip6/::1/tcp/");
        let config = Arc::new(config::create_mainnet());
        let mut swarm = make_swarm_manager::<Libp2pService>(addr, config).await;

        let id_1: libp2p::PeerId =
            "12D3KooWRn14SemPVxwzdQNg8e8Trythiww1FWrNfPbukYBmZEbJ".parse().unwrap();
        let id_2: libp2p::PeerId =
            "12D3KooWE3kBRAnn6jxZMdK1JMWx1iHtR1NKzXSRv5HLTmfD9u9c".parse().unwrap();
        let id_3: libp2p::PeerId =
            "12D3KooWGK4RzvNeioS9aXdzmYXU3mgDrRPjQd8SVyXCkHNxLbWN".parse().unwrap();

        // check that peer with `id` has the correct ipv4 and ipv6 addresses
        let check_peer =
            |discovered: &HashMap<
                <Libp2pService as NetworkingService>::PeerId,
                PeerAddrInfo<Libp2pService>,
            >,
             id: libp2p::PeerId,
             ip4: Vec<Arc<<Libp2pService as NetworkingService>::Address>>,
             ip6: Vec<Arc<<Libp2pService as NetworkingService>::Address>>| {
                let (p_ip4, p_ip6) = match discovered.get(&id).unwrap() {
                    PeerAddrInfo::Raw { ip4, ip6 } => (ip4, ip6),
                };

                assert_eq!(ip4.len(), p_ip4.len());
                assert_eq!(ip6.len(), p_ip6.len());

                for ip in ip4.iter() {
                    assert!(p_ip4.contains(ip));
                }

                for ip in ip6.iter() {
                    assert!(p_ip6.contains(ip));
                }
            };

        // first add two new peers, both with ipv4 and ipv6 address
        swarm
            .peer_discovered(&[
                net::AddrInfo {
                    id: id_1,
                    ip4: vec![Arc::new("/ip4/127.0.0.1/tcp/9090".parse().unwrap())],
                    ip6: vec![Arc::new("/ip6/::1/tcp/9091".parse().unwrap())],
                },
                net::AddrInfo {
                    id: id_2,
                    ip4: vec![Arc::new("/ip4/127.0.0.1/tcp/9092".parse().unwrap())],
                    ip6: vec![Arc::new("/ip6/::1/tcp/9093".parse().unwrap())],
                },
            ])
            .unwrap();

        assert_eq!(swarm.peers.len(), 0);
        assert_eq!(swarm.discovered.len(), 2);

        check_peer(
            &swarm.discovered,
            id_1,
            vec![Arc::new("/ip4/127.0.0.1/tcp/9090".parse().unwrap())],
            vec![Arc::new("/ip6/::1/tcp/9091".parse().unwrap())],
        );

        check_peer(
            &swarm.discovered,
            id_2,
            vec![Arc::new("/ip4/127.0.0.1/tcp/9092".parse().unwrap())],
            vec![Arc::new("/ip6/::1/tcp/9093".parse().unwrap())],
        );

        // then discover one new peer and two additional ipv6 addresses for peer 1
        swarm
            .peer_discovered(&[
                net::AddrInfo {
                    id: id_1,
                    ip4: vec![],
                    ip6: vec![
                        Arc::new("/ip6/::1/tcp/9094".parse().unwrap()),
                        Arc::new("/ip6/::1/tcp/9095".parse().unwrap()),
                    ],
                },
                net::AddrInfo {
                    id: id_3,
                    ip4: vec![Arc::new("/ip4/127.0.0.1/tcp/9096".parse().unwrap())],
                    ip6: vec![Arc::new("/ip6/::1/tcp/9097".parse().unwrap())],
                },
            ])
            .unwrap();

        check_peer(
            &swarm.discovered,
            id_1,
            vec![Arc::new("/ip4/127.0.0.1/tcp/9090".parse().unwrap())],
            vec![
                Arc::new("/ip6/::1/tcp/9091".parse().unwrap()),
                Arc::new("/ip6/::1/tcp/9094".parse().unwrap()),
                Arc::new("/ip6/::1/tcp/9095".parse().unwrap()),
            ],
        );

        check_peer(
            &swarm.discovered,
            id_3,
            vec![Arc::new("/ip4/127.0.0.1/tcp/9096".parse().unwrap())],
            vec![Arc::new("/ip6/::1/tcp/9097".parse().unwrap())],
        );
    }

    // verify that if the node is aware of any peers on the network,
    // call to `auto_connect()` will establish a connection with them
    #[tokio::test]
    async fn test_auto_connect_mock() {
        let config = Arc::new(config::create_mainnet());
        let addr: Multiaddr = test_utils::make_address("/ip6/::1/tcp/");
        let mut swarm = make_swarm_manager::<Libp2pService>(addr, config.clone()).await;
        let mut swarm2 =
            make_swarm_manager::<Libp2pService>(test_utils::make_address("/ip6/::1/tcp/"), config)
                .await;

        let addr = swarm2.handle.local_addr().clone();
        let id: PeerId = if let Some(Protocol::P2p(peer)) = addr.iter().last() {
            PeerId::from_multihash(peer).unwrap()
        } else {
            panic!("invalid multiaddr");
        };

        tokio::spawn(async move {
            log::debug!("staring libp2p service");
            loop {
                assert!(swarm2.handle.poll_next().await.is_ok());
            }
        });

        // "discover" the other libp2p service
        swarm
            .peer_discovered(&[net::AddrInfo {
                id,
                ip4: vec![],
                ip6: vec![Arc::new(addr)],
            }])
            .unwrap();
        swarm.auto_connect().await.unwrap();
        assert_eq!(swarm.peers.len(), 1);
    }

    #[tokio::test]
    async fn connect_outbound_same_network() {
        let config = Arc::new(config::create_mainnet());
        let mut swarm1 = make_swarm_manager::<Libp2pService>(
            test_utils::make_address("/ip6/::1/tcp/"),
            config.clone(),
        )
        .await;
        let mut swarm2 =
            make_swarm_manager::<Libp2pService>(test_utils::make_address("/ip6/::1/tcp/"), config)
                .await;

        let (conn1_res, _conn2_res) = tokio::join!(
            swarm1.handle.connect(swarm2.handle.local_addr().clone()),
            swarm2.handle.poll_next()
        );

        assert_eq!(
            swarm1
                .on_network_event(net::ConnectivityEvent::ConnectionAccepted {
                    peer_info: conn1_res.unwrap()
                },)
                .await,
            Ok(())
        );
    }

    #[tokio::test]
    async fn connect_outbound_different_network() {
        let _swarm1 = make_swarm_manager::<Libp2pService>(
            test_utils::make_address("/ip6/::1/tcp/"),
            Arc::new(config::create_mainnet()),
        )
        .await;
        let mut swarm2 = make_swarm_manager::<Libp2pService>(
            test_utils::make_address("/ip6/::1/tcp/"),
            Arc::new(
                common::chain::config::TestChainConfig::new()
                    .with_magic_bytes([1, 2, 3, 4])
                    .build(),
            ),
        )
        .await;

        tokio::spawn(async move { swarm2.handle.poll_next().await.unwrap() });

        // TODO: implement connect properly
        // assert_eq!(
        //     swarm1.handle.connect(addr).await,
        //     Err(P2pError::ProtocolError(ProtocolError::UnknownNetwork)),
        // );
    }

    #[tokio::test]
    async fn connect_inbound_same_network() {
        let config = Arc::new(config::create_mainnet());
        let mut swarm1 = make_swarm_manager::<Libp2pService>(
            test_utils::make_address("/ip6/::1/tcp/"),
            config.clone(),
        )
        .await;
        let mut swarm2 =
            make_swarm_manager::<Libp2pService>(test_utils::make_address("/ip6/::1/tcp/"), config)
                .await;

        let (_conn1_res, conn2_res) = tokio::join!(
            swarm1.handle.connect(swarm2.handle.local_addr().clone()),
            swarm2.handle.poll_next()
        );
        let conn2_res: net::ConnectivityEvent<Libp2pService> = conn2_res.unwrap();
        assert!(std::matches!(
            conn2_res,
            net::ConnectivityEvent::IncomingConnection { .. }
        ));
        assert_eq!(swarm2.on_network_event(conn2_res).await, Ok(()));
    }

    #[tokio::test]
    async fn connect_inbound_different_network() {
        let mut swarm1 = make_swarm_manager::<Libp2pService>(
            test_utils::make_address("/ip6/::1/tcp/"),
            Arc::new(config::create_mainnet()),
        )
        .await;
        let mut swarm2 = make_swarm_manager::<Libp2pService>(
            test_utils::make_address("/ip6/::1/tcp/"),
            Arc::new(
                common::chain::config::TestChainConfig::new()
                    .with_magic_bytes([1, 2, 3, 4])
                    .build(),
            ),
        )
        .await;

        let (_conn1_res, conn2_res) = tokio::join!(
            swarm1.handle.connect(swarm2.handle.local_addr().clone()),
            swarm2.handle.poll_next()
        );
        let conn2_res: net::ConnectivityEvent<Libp2pService> = conn2_res.unwrap();
        assert!(std::matches!(
            conn2_res,
            net::ConnectivityEvent::IncomingConnection { .. }
        ));
        assert_eq!(
            swarm2.on_network_event(conn2_res).await,
            Err(P2pError::ProtocolError(ProtocolError::DifferentNetwork))
        );
    }

    #[tokio::test]
    async fn remote_closes_connection() {
        let mut swarm1 = make_swarm_manager::<Libp2pService>(
            test_utils::make_address("/ip6/::1/tcp/"),
            Arc::new(config::create_mainnet()),
        )
        .await;
        let mut swarm2 = make_swarm_manager::<Libp2pService>(
            test_utils::make_address("/ip6/::1/tcp/"),
            Arc::new(config::create_mainnet()),
        )
        .await;
        let (_conn1_res, conn2_res) = tokio::join!(
            swarm1.handle.connect(swarm2.handle.local_addr().clone()),
            swarm2.handle.poll_next()
        );
        let conn2_res: net::ConnectivityEvent<Libp2pService> = conn2_res.unwrap();
        assert!(std::matches!(
            conn2_res,
            net::ConnectivityEvent::IncomingConnection { .. }
        ));

        assert_eq!(
            swarm2.handle.disconnect(*swarm1.handle.peer_id()).await,
            Ok(())
        );
        assert!(std::matches!(
            swarm1.handle.poll_next().await,
            Ok(net::ConnectivityEvent::ConnectionClosed { .. })
        ));
    }
}

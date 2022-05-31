// Copyright (c) 2021-2022 RBB S.r.l
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
use libp2p::{
    gossipsub::error::PublishError as GossipsubPublishError,
    swarm::{handler::ConnectionHandlerUpgrErr, DialError::*},
};
use thiserror::Error;

#[derive(Error, Debug, PartialEq, Eq)]
pub enum ProtocolError {
    #[error("Peer is in different network")]
    DifferentNetwork,
    #[error("Peer has an unsupported version")]
    InvalidVersion,
    #[error("Peer sent an invalid message")]
    InvalidMessage,
    #[error("Peer is incompatible")] // TODO: remove?
    Incompatible,
    #[error("Peer is unresponsive")]
    Unresponsive,
    #[error("Peer uses an invalid protocol")] // TODO: remove?
    InvalidProtocol,
    #[error("Peer is an unknown network")] // TODO: remove?
    UnknownNetwork,
    #[error("Peer is in an invalid state to perform this operation")]
    InvalidState,
}

#[derive(Error, Debug, PartialEq, Eq)]
pub enum PeerError {
    #[error("Peer disconnected")]
    PeerDisconnected,
    #[error("No peers")]
    NoPeers,
    #[error("Peer doesn't exist")]
    PeerDoesntExist,
    #[error("Peer already exists")]
    PeerExists,
}

#[derive(Error, Debug, PartialEq, Eq)]
pub enum PublishError {
    #[error("Message has already been published")]
    Duplicate,
    #[error("Failed to sign message")]
    SigningFailed,
    #[error("Not enough peers in topic")]
    InsufficientPeers,
    #[error("Message is too large")]
    MessageTooLarge,
    #[error("Failed to compress the message")]
    TransformFailed,
}

#[derive(Error, Debug, PartialEq, Eq)]
pub enum DialError {
    #[error("Peer is banned")]
    Banned,
    #[error("Limit for outgoing connections reached")]
    ConnectionLimit,
    #[error("Tried to dial local node")]
    LocalPeerId,
    #[error("Peer doesn't have any known addresses")]
    NoAddresses,
    #[error("Peer state not correct for dialing")]
    DialPeerConditionFalse,
    #[error("Connection has been aborted")]
    Aborted,
    #[error("Invalid PeerId")]
    InvalidPeerId,
    #[error("PeerId doesn't match the PeerId of endpoint")]
    WrongPeerId,
    #[error("I/O error: `{0:?}`")]
    IoError(std::io::ErrorKind),
    #[error("Failed to negotiate transport protocol")]
    Transport,
}

#[derive(Error, Debug, PartialEq, Eq)]
pub enum ConnectionError {
    #[error("Timeout")]
    Timeout,
    #[error("Timer failed")]
    Timer,
    #[error("Failed to upgrade protocol")]
    Upgrade,
}

#[derive(Error, Debug, PartialEq, Eq)]
pub enum P2pError {
    #[error("Protocol violation: `{0:?}`")]
    ProtocolError(ProtocolError),
    #[error("Failed to publish message: `{0:?}`")]
    PublishError(PublishError),
    #[error("Failed to upgrade connection: `{0:?}`")]
    ConnectionError(ConnectionError),
    #[error("Failed to dial peer: `{0:?}`")]
    DialError(DialError),
    #[error("Connection to other task lost")]
    ChannelClosed,
    #[error("Peer-related error: `{0:?}`")]
    PeerError(PeerError),
    #[error("Invalid data: `{0}`")]
    InvalidData(&'static str),
    #[error("SubsystemFailure")]
    SubsystemFailure,
    #[error("ConsensusError: `{0:?}`")]
    ChainstateError(chainstate::ChainstateError),
    #[error("DatabaseFailure")]
    DatabaseFailure,
    #[error("Failed to convert data `{0}`")]
    ConversionError(&'static str),
    #[error("Other: `{0:?}`")]
    Other(&'static str),
}

pub trait FatalError {
    fn map_fatal_err(self) -> core::result::Result<(), P2pError>;
}

impl From<std::io::Error> for P2pError {
    fn from(e: std::io::Error) -> P2pError {
        P2pError::DialError(DialError::IoError(e.kind()))
    }
}

impl From<serialization::Error> for P2pError {
    fn from(_: serialization::Error) -> P2pError {
        P2pError::ConversionError("Failed to decode data")
    }
}

impl From<tokio::sync::oneshot::error::RecvError> for P2pError {
    fn from(_: tokio::sync::oneshot::error::RecvError) -> P2pError {
        P2pError::ChannelClosed
    }
}

impl<T> From<tokio::sync::mpsc::error::SendError<T>> for P2pError {
    fn from(_: tokio::sync::mpsc::error::SendError<T>) -> P2pError {
        P2pError::ChannelClosed
    }
}

impl From<subsystem::subsystem::CallError> for P2pError {
    fn from(_e: subsystem::subsystem::CallError) -> P2pError {
        P2pError::ChannelClosed
    }
}

impl From<chainstate::ChainstateError> for P2pError {
    fn from(e: chainstate::ChainstateError) -> P2pError {
        P2pError::ChainstateError(e)
    }
}

impl From<libp2p::gossipsub::error::PublishError> for P2pError {
    fn from(err: libp2p::gossipsub::error::PublishError) -> P2pError {
        match err {
            GossipsubPublishError::Duplicate => P2pError::PublishError(PublishError::Duplicate),
            GossipsubPublishError::SigningError(_) => {
                P2pError::PublishError(PublishError::SigningFailed)
            }
            GossipsubPublishError::InsufficientPeers => {
                P2pError::PublishError(PublishError::InsufficientPeers)
            }
            GossipsubPublishError::MessageTooLarge => {
                P2pError::PublishError(PublishError::MessageTooLarge)
            }
            GossipsubPublishError::TransformFailed(_) => {
                P2pError::PublishError(PublishError::TransformFailed)
            }
        }
    }
}

impl From<libp2p::swarm::DialError> for P2pError {
    fn from(err: libp2p::swarm::DialError) -> P2pError {
        match err {
            Banned => P2pError::DialError(DialError::Banned),
            ConnectionLimit(_) => P2pError::DialError(DialError::ConnectionLimit),
            LocalPeerId => P2pError::DialError(DialError::LocalPeerId),
            NoAddresses => P2pError::DialError(DialError::NoAddresses),
            DialPeerConditionFalse(_) => P2pError::DialError(DialError::DialPeerConditionFalse),
            Aborted => P2pError::DialError(DialError::Aborted),
            InvalidPeerId(_) => P2pError::DialError(DialError::InvalidPeerId),
            WrongPeerId { .. } => P2pError::DialError(DialError::WrongPeerId),
            ConnectionIo(error) => P2pError::DialError(DialError::IoError(error.kind())),
            Transport(_) => P2pError::DialError(DialError::Transport),
        }
    }
}

impl<T> From<libp2p::swarm::handler::ConnectionHandlerUpgrErr<T>> for P2pError {
    fn from(err: libp2p::swarm::handler::ConnectionHandlerUpgrErr<T>) -> P2pError {
        match err {
            ConnectionHandlerUpgrErr::Timeout => {
                P2pError::ConnectionError(ConnectionError::Timeout)
            }
            ConnectionHandlerUpgrErr::Timer => P2pError::ConnectionError(ConnectionError::Timer),
            ConnectionHandlerUpgrErr::Upgrade(_) => {
                P2pError::ConnectionError(ConnectionError::Upgrade)
            }
        }
    }
}

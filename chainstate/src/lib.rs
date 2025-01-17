// Copyright (c) 2021 RBB S.r.l
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
// Author(s): S. Afach, A. Sinitsyn

mod detail;

pub mod rpc;

pub mod chainstate_interface_impl;

pub mod chainstate_interface;

use std::sync::Arc;

pub use chainstate_interface_impl::ChainstateInterfaceImpl;
use common::{
    chain::{block::Block, ChainConfig},
    primitives::{BlockHeight, Id},
};
use chainstate_interface::ChainstateInterface;
pub use detail::BlockError;
pub use detail::{BlockSource, Chainstate};

#[derive(Debug, Clone)]
pub enum ChainstateEvent {
    NewTip(Id<Block>, BlockHeight),
}

#[derive(thiserror::Error, Debug, PartialEq, Eq)]
pub enum ChainstateError {
    #[error("Initialization error")]
    FailedToInitializeChainstate(String),
    #[error("Block processing failed: `{0}`")]
    ProcessBlockError(BlockError),
    #[error("Property read error: `{0}`")]
    FailedToReadProperty(BlockError),
}

impl subsystem::Subsystem for Box<dyn ChainstateInterface> {}

type ChainstateHandle = subsystem::Handle<Box<dyn ChainstateInterface>>;

pub fn make_chainstate(
    chain_config: Arc<ChainConfig>,
    blockchain_storage: blockchain_storage::Store,
    custom_orphan_error_hook: Option<Arc<detail::OrphanErrorHandler>>,
) -> Result<Box<dyn ChainstateInterface>, ChainstateError> {
    let cons = Chainstate::new(chain_config, blockchain_storage, custom_orphan_error_hook)?;
    let cons_interface = ChainstateInterfaceImpl::new(cons);
    Ok(Box::new(cons_interface))
}

#[cfg(test)]
mod test;

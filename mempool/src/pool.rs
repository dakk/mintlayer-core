use std::cmp::Ord;
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::fmt::Debug;
use std::rc::Rc;

use parity_scale_codec::Encode;
use thiserror::Error;

use common::chain::transaction::Transaction;
use common::chain::transaction::TxInput;
use common::chain::OutPoint;
use common::primitives::amount::Amount;
use common::primitives::Id;
use common::primitives::Idable;
use common::primitives::H256;

// TODO this willbe defined elsewhere (some of limits.rs file)
const MAX_BLOCK_SIZE_BYTES: usize = 1_000_000;

const MEMPOOL_MAX_TXS: usize = 1_000_000;

impl<C: ChainState> TryGetFee for MempoolImpl<C> {
    fn try_get_fee(&self, tx: &Transaction) -> Result<Amount, TxValidationError> {
        let inputs = tx
            .get_inputs()
            .iter()
            .map(|input| {
                let outpoint = input.get_outpoint();
                self.chain_state
                    .get_outpoint_value(outpoint)
                    .or_else(|_| self.store.get_unconfirmed_outpoint_value(outpoint))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let sum_inputs = inputs
            .iter()
            .cloned()
            .sum::<Option<_>>()
            .ok_or(TxValidationError::TransactionFeeOverflow)?;
        let sum_outputs = tx
            .get_outputs()
            .iter()
            .map(|output| output.get_value())
            .sum::<Option<_>>()
            .ok_or(TxValidationError::TransactionFeeOverflow)?;
        (sum_inputs - sum_outputs).ok_or(TxValidationError::TransactionFeeOverflow)
    }
}

pub trait Mempool<C> {
    fn create(chain_state: C) -> Self;
    fn add_transaction(&mut self, tx: Transaction) -> Result<(), MempoolError>;
    fn get_all(&self) -> Vec<&Transaction>;
    fn contains_transaction(&self, tx: &Id<Transaction>) -> bool;
    fn drop_transaction(&mut self, tx: &Id<Transaction>);
    fn new_tip_set(&mut self) -> Result<(), MempoolError>;
}

pub trait ChainState {
    fn contains_outpoint(&self, outpoint: &OutPoint) -> bool;
    fn get_outpoint_value(&self, outpoint: &OutPoint) -> Result<Amount, anyhow::Error>;
}

#[derive(Debug, PartialEq, Eq, Clone)]
struct TxMempoolEntry {
    tx: Transaction,
    fee: Amount,
    parents: BTreeSet<Rc<TxMempoolEntry>>,
    children: BTreeSet<Rc<TxMempoolEntry>>,
}

trait TryGetFee {
    fn try_get_fee(&self, tx: &Transaction) -> Result<Amount, TxValidationError>;
}

impl TxMempoolEntry {
    fn new(tx: Transaction, fee: Amount, parents: BTreeSet<Rc<TxMempoolEntry>>) -> TxMempoolEntry {
        Self {
            tx,
            fee,
            parents,
            children: BTreeSet::default(),
        }
    }

    fn is_replaceable(&self) -> bool {
        self.tx.is_replaceable()
            || self.unconfirmed_ancestors().iter().any(|ancestor| ancestor.tx.is_replaceable())
    }

    fn unconfirmed_ancestors(&self) -> BTreeSet<Rc<TxMempoolEntry>> {
        let mut visited = BTreeSet::new();
        self.unconfirmed_ancestors_inner(&mut visited);
        visited
    }

    fn unconfirmed_ancestors_inner(&self, visited: &mut BTreeSet<Rc<TxMempoolEntry>>) {
        for parent in self.parents.iter() {
            if visited.contains(parent) {
                continue;
            } else {
                visited.insert(Rc::clone(parent));
                parent.unconfirmed_ancestors_inner(visited);
            }
        }
    }
}

impl PartialOrd for TxMempoolEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(other.tx.get_id().get().cmp(&self.tx.get_id().get()))
    }
}

impl Ord for TxMempoolEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        other.tx.get_id().get().cmp(&self.tx.get_id().get())
    }
}

#[derive(Debug)]
pub struct MempoolImpl<C: ChainState> {
    store: MempoolStore,
    chain_state: C,
}

#[derive(Debug)]
struct MempoolStore {
    txs_by_id: HashMap<H256, Rc<TxMempoolEntry>>,
    txs_by_fee: BTreeMap<Amount, BTreeSet<Rc<TxMempoolEntry>>>,
    spender_txs: BTreeMap<OutPoint, Rc<TxMempoolEntry>>,
}

impl MempoolStore {
    fn new() -> Self {
        Self {
            txs_by_fee: BTreeMap::new(),
            txs_by_id: HashMap::new(),
            spender_txs: BTreeMap::new(),
        }
    }

    // Checks whether the outpoint is to be created by an unconfirmed tx
    fn contains_outpoint(&self, outpoint: &OutPoint) -> bool {
        matches!(self.txs_by_id.get(&outpoint.get_tx_id().get()),
            Some(entry) if entry.tx.get_outputs().len() > outpoint.get_output_index() as usize)
    }

    fn get_unconfirmed_outpoint_value(
        &self,
        outpoint: &OutPoint,
    ) -> Result<Amount, TxValidationError> {
        let tx_id = outpoint.get_tx_id();
        let err = || TxValidationError::OutPointNotFound {
            outpoint: outpoint.to_owned(),
            tx_id: tx_id.to_owned(),
        };
        self.txs_by_id
            .get(&tx_id.get())
            .ok_or_else(err)
            .and_then(|entry| {
                entry.tx.get_outputs().get(outpoint.get_output_index() as usize).ok_or_else(err)
            })
            .map(|output| output.get_value())
    }

    fn add_tx(&mut self, entry: TxMempoolEntry) -> Result<(), MempoolError> {
        let id = entry.tx.get_id().get();
        let entry = Rc::new(entry);
        self.txs_by_id.insert(id, Rc::clone(&entry));
        self.txs_by_fee.entry(entry.fee).or_default().insert(Rc::clone(&entry));

        for outpoint in entry.tx.get_inputs().iter().map(|input| input.get_outpoint()) {
            self.spender_txs.insert(outpoint.to_owned(), Rc::clone(&entry));
        }

        for mut parent in entry.parents.clone() {
            assert!(Rc::get_mut(&mut parent)
                .expect("exclusive access to parent")
                .children
                .insert(Rc::clone(&entry)))
        }

        Ok(())
    }

    fn drop_tx(&mut self, tx_id: &Id<Transaction>) {
        if let Some(entry) = self.txs_by_id.remove(&tx_id.get()) {
            self.txs_by_fee.entry(entry.fee).and_modify(|entries| {
                entries.remove(&entry).then(|| ()).expect("Inconsistent mempool store")
            });
            self.spender_txs.retain(|_, entry| entry.tx.get_id() != *tx_id)
        } else {
            assert!(!self.txs_by_fee.values().flatten().any(|entry| entry.tx.get_id() == *tx_id));
            assert!(!self.spender_txs.iter().any(|(_, entry)| entry.tx.get_id() == *tx_id));
        }
    }

    fn find_conflicting_tx(&self, outpoint: &OutPoint) -> Option<Rc<TxMempoolEntry>> {
        self.spender_txs.get(outpoint).cloned()
    }
}

#[derive(Debug, Error)]
pub enum MempoolError {
    #[error("Mempool is full")]
    MempoolFull,
    #[error(transparent)]
    TxValidationError(TxValidationError),
}

#[derive(Debug, Error)]
pub enum TxValidationError {
    #[error("No Inputs")]
    NoInputs,
    #[error("No Ouputs")]
    NoOutputs,
    #[error("DuplicateInputs")]
    DuplicateInputs,
    #[error("LooseCoinbase")]
    LooseCoinbase,
    #[error("OutPointNotFound {outpoint:?}")]
    OutPointNotFound {
        outpoint: OutPoint,
        tx_id: Id<Transaction>,
    },
    #[error("ExceedsMaxBlockSize")]
    ExceedsMaxBlockSize,
    #[error("TransactionAlreadyInMempool")]
    TransactionAlreadyInMempool,
    #[error("ConflictWithIrreplaceableTransaction")]
    ConflictWithIrreplaceableTransaction,
    #[error("TransactionFeeOverflow")]
    TransactionFeeOverflow,
}

impl From<TxValidationError> for MempoolError {
    fn from(e: TxValidationError) -> Self {
        MempoolError::TxValidationError(e)
    }
}

impl<C: ChainState + Debug> MempoolImpl<C> {
    fn verify_inputs_available(&self, tx: &Transaction) -> Result<(), TxValidationError> {
        tx.get_inputs()
            .iter()
            .map(TxInput::get_outpoint)
            .find(|outpoint| !self.outpoint_available(outpoint))
            .map_or_else(
                || Ok(()),
                |outpoint| {
                    Err(TxValidationError::OutPointNotFound {
                        outpoint: outpoint.clone(),
                        tx_id: tx.get_id(),
                    })
                },
            )
    }

    fn outpoint_available(&self, outpoint: &OutPoint) -> bool {
        self.store.contains_outpoint(outpoint) || self.chain_state.contains_outpoint(outpoint)
    }

    fn create_entry(&self, tx: Transaction) -> Result<TxMempoolEntry, TxValidationError> {
        let parents = tx
            .get_inputs()
            .iter()
            .filter_map(|input| self.store.txs_by_id.get(&input.get_outpoint().get_tx_id().get()))
            .cloned()
            .collect::<BTreeSet<_>>();

        let fee = self.try_get_fee(&tx)?;
        Ok(TxMempoolEntry::new(tx, fee, parents))
    }

    fn validate_transaction(&self, tx: &Transaction) -> Result<(), TxValidationError> {
        if tx.get_inputs().is_empty() {
            return Err(TxValidationError::NoInputs);
        }

        if tx.get_outputs().is_empty() {
            return Err(TxValidationError::NoOutputs);
        }

        if tx.is_coinbase() {
            return Err(TxValidationError::LooseCoinbase);
        }

        // TODO consier a MAX_MONEY check reminiscent of bitcoin's
        // TODO consider rejecting non-standard transactions (for some definition of standard)

        let outpoints = tx.get_inputs().iter().map(|input| input.get_outpoint()).cloned();

        if has_duplicate_entry(outpoints) {
            return Err(TxValidationError::DuplicateInputs);
        }

        if tx.encoded_size() > MAX_BLOCK_SIZE_BYTES {
            return Err(TxValidationError::ExceedsMaxBlockSize);
        }

        if self.contains_transaction(&tx.get_id()) {
            return Err(TxValidationError::TransactionAlreadyInMempool);
        }

        let conflicts = tx
            .get_inputs()
            .iter()
            .filter_map(|input| self.store.find_conflicting_tx(input.get_outpoint()))
            .collect::<Vec<_>>();

        for entry in &conflicts {
            entry
                .is_replaceable()
                .then(|| ())
                .ok_or(TxValidationError::ConflictWithIrreplaceableTransaction)?;
        }

        self.verify_inputs_available(tx)?;

        Ok(())
    }
}

impl<C: ChainState + Debug> Mempool<C> for MempoolImpl<C> {
    fn create(chain_state: C) -> Self {
        Self {
            store: MempoolStore::new(),
            chain_state,
        }
    }

    fn new_tip_set(&mut self) -> Result<(), MempoolError> {
        unimplemented!()
    }
    //

    fn add_transaction(&mut self, tx: Transaction) -> Result<(), MempoolError> {
        // TODO (1). First, we need to decide on criteria for the Mempool to be considered full. Maybe number
        // of transactions is not a good enough indicator. Consider checking mempool size as well
        // TODO (2) What to do when the mempool is full. Instead of rejecting Do incoming transaction we probably want to evict a low-score transaction
        if self.store.txs_by_fee.len() >= MEMPOOL_MAX_TXS {
            return Err(MempoolError::MempoolFull);
        }
        self.validate_transaction(&tx)?;
        let entry = self.create_entry(tx)?;
        self.store.add_tx(entry)?;
        Ok(())
    }

    fn get_all(&self) -> Vec<&Transaction> {
        self.store.txs_by_fee.values().flatten().map(|entry| &entry.tx).collect()
    }

    fn contains_transaction(&self, tx_id: &Id<Transaction>) -> bool {
        self.store.txs_by_id.contains_key(&tx_id.get())
    }

    // TODO Consider returning an error
    fn drop_transaction(&mut self, tx_id: &Id<Transaction>) {
        self.store.drop_tx(tx_id);
    }
}

fn has_duplicate_entry<T>(iter: T) -> bool
where
    T: IntoIterator,
    T::Item: Ord,
{
    let mut uniq = BTreeSet::new();
    iter.into_iter().any(move |x| !uniq.insert(x))
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::address::Address;
    use common::chain::config::create_mainnet;
    use common::chain::transaction::{Destination, TxInput, TxOutput};
    use rand::Rng;

    const DUMMY_WITNESS_MSG: &[u8] = b"dummy_witness_msg";

    #[derive(PartialEq, Eq, PartialOrd, Ord, Clone)]
    struct ValuedOutPoint {
        outpoint: OutPoint,
        value: Amount,
    }

    fn valued_outpoint(
        tx_id: &Id<Transaction>,
        outpoint_index: u32,
        output: &TxOutput,
    ) -> ValuedOutPoint {
        let outpoint = OutPoint::new(tx_id.to_owned(), outpoint_index);
        let value = output.get_value();
        ValuedOutPoint { outpoint, value }
    }

    pub(crate) fn create_genesis_tx() -> Transaction {
        const TOTAL_SUPPLY: u128 = 10_000_000_000_000;
        let genesis_message = b"".to_vec();
        let config = create_mainnet();
        let genesis_mint_receiver =
            Address::new(&config, []).expect("Failed to create genesis mint address");
        let input = TxInput::new(Id::new(&H256::zero()), 0, genesis_message);
        let output = TxOutput::new(
            Amount::new(TOTAL_SUPPLY),
            Destination::Address(genesis_mint_receiver),
        );
        Transaction::new(0, vec![input], vec![output], 0)
            .expect("Failed to create genesis coinbase transaction")
    }

    impl TxMempoolEntry {
        fn outpoints_created(&self) -> BTreeSet<ValuedOutPoint> {
            let id = self.tx.get_id();
            std::iter::repeat(id)
                .zip(self.tx.get_outputs().iter().enumerate())
                .map(|(id, (index, output))| valued_outpoint(&id, index as u32, output))
                .collect()
        }
    }

    impl MempoolStore {
        fn unconfirmed_outpoints(&self) -> BTreeSet<ValuedOutPoint> {
            self.txs_by_id
                .values()
                .cloned()
                .flat_map(|entry| entry.outpoints_created())
                .collect()
        }
    }

    impl MempoolImpl<ChainStateMock> {
        fn available_outpoints(&self) -> BTreeSet<ValuedOutPoint> {
            self.store
                .unconfirmed_outpoints()
                .into_iter()
                .chain(self.chain_state.confirmed_outpoints())
                .collect()
        }

        fn get_input_value(&self, input: &TxInput) -> anyhow::Result<Amount> {
            self.available_outpoints()
                .iter()
                .find_map(|valued_outpoint| {
                    (valued_outpoint.outpoint == *input.get_outpoint())
                        .then(|| valued_outpoint.value)
                })
                .ok_or(anyhow::anyhow!("No such unconfirmed output"))
        }
    }

    #[derive(Debug, Clone)]
    pub(crate) struct ChainStateMock {
        txs: HashMap<H256, Transaction>,
        outpoints: BTreeSet<OutPoint>,
    }

    impl ChainStateMock {
        pub(crate) fn new() -> Self {
            let genesis_tx = create_genesis_tx();
            let outpoints = genesis_tx
                .get_outputs()
                .iter()
                .enumerate()
                .map(|(index, _)| OutPoint::new(genesis_tx.get_id(), index as u32))
                .collect();
            Self {
                txs: std::iter::once((genesis_tx.get_id().get(), genesis_tx)).collect(),
                outpoints,
            }
        }

        fn unspent_outpoints(&self) -> BTreeSet<ValuedOutPoint> {
            self.outpoints
                .iter()
                .map(|outpoint| {
                    let value =
                        self.get_outpoint_value(outpoint).expect("Inconsistent Chain State");
                    ValuedOutPoint {
                        outpoint: outpoint.to_owned(),
                        value,
                    }
                })
                .collect()
        }

        fn confirmed_txs(&self) -> &HashMap<H256, Transaction> {
            &self.txs
        }

        fn confirmed_outpoints(&self) -> BTreeSet<ValuedOutPoint> {
            self.txs
                .values()
                .flat_map(|tx| {
                    std::iter::repeat(tx.get_id())
                        .zip(tx.get_outputs().iter().enumerate())
                        .map(move |(tx_id, (i, output))| valued_outpoint(&tx_id, i as u32, output))
                })
                .collect()
        }
    }

    impl ChainState for ChainStateMock {
        fn contains_outpoint(&self, outpoint: &OutPoint) -> bool {
            self.outpoints.iter().any(|value| *value == *outpoint)
        }

        fn get_outpoint_value(&self, outpoint: &OutPoint) -> Result<Amount, anyhow::Error> {
            self.txs
                .get(&outpoint.get_tx_id().get())
                .ok_or(anyhow::anyhow!(
                    "tx for outpoint sought in chain state, not found"
                ))
                .and_then(|tx| {
                    tx.get_outputs()
                        .get(outpoint.get_output_index() as usize)
                        .ok_or(anyhow::anyhow!("outpoint index out of bounds"))
                        .map(|output| output.get_value())
                })
        }
    }

    struct TxGenerator {
        coin_pool: BTreeSet<ValuedOutPoint>,
        num_inputs: usize,
        num_outputs: usize,
        tx_fee: Option<Amount>,
    }

    impl TxGenerator {
        fn new(
            mempool: &MempoolImpl<ChainStateMock>,
            num_inputs: usize,
            num_outputs: usize,
        ) -> Self {
            let unconfirmed_outputs = BTreeSet::new();
            Self::create_tx_generator(
                &mempool.chain_state,
                &unconfirmed_outputs,
                num_inputs,
                num_outputs,
            )
        }

        fn with_fee(mut self, fee: Amount) -> Self {
            self.tx_fee = Some(fee);
            self
        }

        fn new_with_unconfirmed(
            mempool: &MempoolImpl<ChainStateMock>,
            num_inputs: usize,
            num_outputs: usize,
        ) -> Self {
            let unconfirmed_outputs = mempool.available_outpoints();
            Self::create_tx_generator(
                &mempool.chain_state,
                &unconfirmed_outputs,
                num_inputs,
                num_outputs,
            )
        }

        fn create_tx_generator(
            chain_state: &ChainStateMock,
            unconfirmed_outputs: &BTreeSet<ValuedOutPoint>,
            num_inputs: usize,
            num_outputs: usize,
        ) -> Self {
            let coin_pool = chain_state
                .unspent_outpoints()
                .iter()
                .chain(unconfirmed_outputs)
                .cloned()
                .collect();

            Self {
                coin_pool,
                num_inputs,
                num_outputs,
                tx_fee: None,
            }
        }

        fn generate_tx(&mut self) -> anyhow::Result<Transaction> {
            let valued_inputs = self.generate_tx_inputs();
            let outputs = self.generate_tx_outputs(&valued_inputs, self.tx_fee)?;
            let locktime = 0;
            let flags = 0;
            let (inputs, _): (Vec<TxInput>, Vec<Amount>) = valued_inputs.into_iter().unzip();
            let spent_outpoints =
                inputs.iter().map(|input| input.get_outpoint()).collect::<BTreeSet<_>>();
            self.coin_pool.retain(|outpoint| {
                !spent_outpoints.iter().any(|spent| **spent == outpoint.outpoint)
            });
            let tx = Transaction::new(flags, inputs, outputs.clone(), locktime)
                .map_err(anyhow::Error::from)?;
            self.coin_pool.extend(
                std::iter::repeat(tx.get_id())
                    .zip(outputs.iter().enumerate())
                    .map(|(id, (i, output))| valued_outpoint(&id, i as u32, output)),
            );

            Ok(tx)
        }

        fn generate_replaceable_tx(mut self) -> anyhow::Result<Transaction> {
            let valued_inputs = self.generate_tx_inputs();
            let outputs = self.generate_tx_outputs(&valued_inputs, self.tx_fee)?;
            let locktime = 0;
            let flags = 1;
            let (inputs, _values): (Vec<TxInput>, Vec<Amount>) = valued_inputs.into_iter().unzip();
            let tx = Transaction::new(flags, inputs, outputs, locktime)?;
            assert!(tx.is_replaceable());
            Ok(tx)
        }

        fn generate_tx_inputs(&mut self) -> Vec<(TxInput, Amount)> {
            std::iter::repeat(())
                .take(self.num_inputs)
                .filter_map(|_| self.generate_input().ok())
                .collect()
        }

        fn generate_tx_outputs(
            &self,
            inputs: &[(TxInput, Amount)],
            tx_fee: Option<Amount>,
        ) -> anyhow::Result<Vec<TxOutput>> {
            if self.num_outputs == 0 {
                return Ok(vec![]);
            }

            let inputs: Vec<_> = inputs.to_owned();
            let (inputs, values): (Vec<TxInput>, Vec<Amount>) = inputs.into_iter().unzip();
            if inputs.is_empty() {
                return Ok(vec![]);
            }
            let sum_of_inputs =
                values.into_iter().sum::<Option<_>>().expect("Overflow in sum of input values");

            let total_to_spend = if let Some(fee) = tx_fee {
                (sum_of_inputs - fee).expect("underflow")
            } else {
                sum_of_inputs
            };

            let mut left_to_spend = total_to_spend;
            let mut outputs = Vec::new();

            let max_output_value = Amount::from(1_000);
            for _ in 0..self.num_outputs - 1 {
                let max_output_value = std::cmp::min(
                    (left_to_spend / (2.into())).expect("division failed"),
                    max_output_value,
                );
                if max_output_value == 0.into() {
                    return Err(anyhow::Error::msg("No more funds to spend"));
                }
                let value = Amount::random(1.into()..=max_output_value);
                outputs.push(TxOutput::new(value, Destination::PublicKey));
                left_to_spend = (left_to_spend - value).expect("subtraction failed");
            }

            outputs.push(TxOutput::new(left_to_spend, Destination::PublicKey));
            Ok(outputs)
        }

        fn generate_input(&self) -> anyhow::Result<(TxInput, Amount)> {
            let ValuedOutPoint { outpoint, value } = self.random_unspent_outpoint()?;
            Ok((
                TxInput::new(
                    outpoint.get_tx_id(),
                    outpoint.get_output_index(),
                    DUMMY_WITNESS_MSG.to_vec(),
                ),
                value,
            ))
        }

        fn random_unspent_outpoint(&self) -> anyhow::Result<ValuedOutPoint> {
            let num_outpoints = self.coin_pool.len();
            (num_outpoints > 0)
                .then(|| {
                    let index = rand::thread_rng().gen_range(0..num_outpoints);
                    self.coin_pool
                        .iter()
                        .cloned()
                        .nth(index)
                        .expect("Outpoint set should not be empty")
                })
                .ok_or(anyhow::anyhow!("no outpoints left"))
        }
    }

    #[test]
    fn add_single_tx() -> anyhow::Result<()> {
        let mut mempool = MempoolImpl::create(ChainStateMock::new());

        let genesis_tx = mempool
            .chain_state
            .confirmed_txs()
            .values()
            .next()
            .expect("genesis tx not found");

        let flags = 0;
        let locktime = 0;
        let input = TxInput::new(genesis_tx.get_id(), 0, DUMMY_WITNESS_MSG.to_vec());
        let tx = tx_spend_input(&mempool, input, None, flags, locktime)?;

        let tx_clone = tx.clone();
        let tx_id = tx.get_id();
        mempool.add_transaction(tx)?;
        assert!(mempool.contains_transaction(&tx_id));
        let all_txs = mempool.get_all();
        assert_eq!(all_txs, vec![&tx_clone]);
        mempool.drop_transaction(&tx_id);
        assert!(!mempool.contains_transaction(&tx_id));
        let all_txs = mempool.get_all();
        assert_eq!(all_txs, Vec::<&Transaction>::new());
        Ok(())
    }

    // The "fees" now a are calculated as sum of the outputs
    // This test creates transactions with a single input and a single output to check that the
    // mempool sorts txs by fee
    #[test]
    fn txs_sorted() -> anyhow::Result<()> {
        let chain_state = ChainStateMock::new();
        let num_inputs = 1;
        let num_outputs = 1;
        let mut mempool = MempoolImpl::create(chain_state);
        let mut tx_generator = TxGenerator::new(&mempool, num_inputs, num_outputs);
        let target_txs = 100;

        for _ in 0..target_txs {
            match tx_generator.generate_tx() {
                Ok(tx) => {
                    mempool.add_transaction(tx.clone())?;
                }
                _ => break,
            }
        }

        let fees = mempool
            .get_all()
            .iter()
            .map(|tx| {
                tx.get_outputs().first().expect("TX should have exactly one output").get_value()
            })
            .collect::<Vec<_>>();
        let mut fees_sorted = fees.clone();
        fees_sorted.sort_by(|a, b| b.cmp(a));
        assert_eq!(fees, fees_sorted);
        Ok(())
    }

    #[test]
    fn tx_no_inputs() -> anyhow::Result<()> {
        let mut mempool = setup();
        let num_inputs = 0;
        let num_outputs = 1;
        let tx = TxGenerator::new(&mempool, num_inputs, num_outputs)
            .generate_tx()
            .expect("generate_tx failed");
        assert!(matches!(
            mempool.add_transaction(tx),
            Err(MempoolError::TxValidationError(TxValidationError::NoInputs))
        ));
        Ok(())
    }

    fn setup() -> MempoolImpl<ChainStateMock> {
        MempoolImpl::create(ChainStateMock::new())
    }

    #[test]
    fn tx_no_outputs() -> anyhow::Result<()> {
        let mut mempool = setup();
        let num_inputs = 1;
        let num_outputs = 0;
        let tx = TxGenerator::new(&mempool, num_inputs, num_outputs)
            .generate_tx()
            .expect("generate_tx failed");
        assert!(matches!(
            mempool.add_transaction(tx),
            Err(MempoolError::TxValidationError(
                TxValidationError::NoOutputs
            ))
        ));
        Ok(())
    }

    #[test]
    fn tx_duplicate_inputs() -> anyhow::Result<()> {
        let mut mempool = MempoolImpl::create(ChainStateMock::new());

        let genesis_tx = mempool
            .chain_state
            .confirmed_txs()
            .values()
            .next()
            .expect("genesis tx not found");

        let input = TxInput::new(genesis_tx.get_id(), 0, DUMMY_WITNESS_MSG.to_vec());
        let witness = b"attempted_double_spend".to_vec();
        let duplicate_input = TxInput::new(genesis_tx.get_id(), 0, witness);
        let flags = 0;
        let locktime = 0;
        let outputs = tx_spend_input(&mempool, input.clone(), None, flags, locktime)?
            .get_outputs()
            .clone();
        let inputs = vec![input, duplicate_input];
        let tx = Transaction::new(flags, inputs, outputs, locktime)?;

        assert!(matches!(
            mempool.add_transaction(tx),
            Err(MempoolError::TxValidationError(
                TxValidationError::DuplicateInputs
            ))
        ));
        Ok(())
    }

    #[test]
    fn tx_already_in_mempool() -> anyhow::Result<()> {
        let mut mempool = MempoolImpl::create(ChainStateMock::new());

        let genesis_tx = mempool
            .chain_state
            .confirmed_txs()
            .values()
            .next()
            .expect("genesis tx not found");

        let input = TxInput::new(genesis_tx.get_id(), 0, DUMMY_WITNESS_MSG.to_vec());
        let flags = 0;
        let locktime = 0;
        let tx = tx_spend_input(&mempool, input, None, flags, locktime)?;

        mempool.add_transaction(tx.clone())?;
        assert!(matches!(
            mempool.add_transaction(tx),
            Err(MempoolError::TxValidationError(
                TxValidationError::TransactionAlreadyInMempool
            ))
        ));
        Ok(())
    }

    pub fn coinbase_input() -> TxInput {
        TxInput::new(
            Id::new(&H256::zero()),
            OutPoint::COINBASE_OUTPOINT_INDEX,
            DUMMY_WITNESS_MSG.to_vec(),
        )
    }

    pub fn coinbase_output() -> TxOutput {
        const BLOCK_REWARD: u32 = 50;
        TxOutput::new(Amount::new(BLOCK_REWARD.into()), Destination::PublicKey)
    }

    pub fn coinbase_tx() -> anyhow::Result<Transaction> {
        const COINBASE_LOCKTIME: u32 = 100;

        let flags = 0;
        let inputs = vec![coinbase_input()];
        let outputs = vec![coinbase_output()];
        let locktime = COINBASE_LOCKTIME;
        Transaction::new(flags, inputs, outputs, locktime).map_err(anyhow::Error::from)
    }

    #[test]
    fn loose_coinbase() -> anyhow::Result<()> {
        let mut mempool = MempoolImpl::create(ChainStateMock::new());
        let coinbase_tx = coinbase_tx()?;

        assert!(matches!(
            mempool.add_transaction(coinbase_tx),
            Err(MempoolError::TxValidationError(
                TxValidationError::LooseCoinbase
            ))
        ));
        Ok(())
    }

    #[test]
    fn outpoint_not_found() -> anyhow::Result<()> {
        let mut mempool = MempoolImpl::create(ChainStateMock::new());

        let genesis_tx = mempool
            .chain_state
            .confirmed_txs()
            .values()
            .next()
            .expect("genesis tx not found");

        let good_input = TxInput::new(genesis_tx.get_id(), 0, DUMMY_WITNESS_MSG.to_vec());
        let flags = 0;
        let locktime = 0;
        let outputs = tx_spend_input(&mempool, good_input, None, flags, locktime)?
            .get_outputs()
            .clone();

        let bad_outpoint_index = 1;
        let bad_input = TxInput::new(
            genesis_tx.get_id(),
            bad_outpoint_index,
            DUMMY_WITNESS_MSG.to_vec(),
        );

        let inputs = vec![bad_input];
        let tx = Transaction::new(flags, inputs, outputs, locktime)?;

        assert!(matches!(
            mempool.add_transaction(tx),
            Err(MempoolError::TxValidationError(
                TxValidationError::OutPointNotFound { .. }
            ))
        ));

        Ok(())
    }

    #[test]
    fn tx_too_big() -> anyhow::Result<()> {
        let mut mempool = setup();
        let num_inputs = 1;
        let num_outputs = 400_000;
        let tx = TxGenerator::new(&mempool, num_inputs, num_outputs)
            .generate_tx()
            .expect("generate_tx failed");
        assert!(matches!(
            mempool.add_transaction(tx),
            Err(MempoolError::TxValidationError(
                TxValidationError::ExceedsMaxBlockSize
            ))
        ));
        Ok(())
    }

    #[test]
    fn tx_replace() -> anyhow::Result<()> {
        let mut mempool = setup();
        let num_inputs = 1;
        let num_outputs = 1;
        let original_fee = Amount::from(10);
        let tx = TxGenerator::new(&mempool, num_inputs, num_outputs)
            .with_fee(original_fee)
            .generate_replaceable_tx()
            .expect("generate_replaceable_tx");
        mempool.add_transaction(tx)?;

        let fee_delta = Amount::from(5);
        let replacement_fee = (original_fee + fee_delta).expect("overflow");
        let tx = TxGenerator::new(&mempool, num_inputs, num_outputs)
            .with_fee(replacement_fee)
            .generate_tx()
            .expect("generate_tx_failed");

        mempool.add_transaction(tx)?;
        Ok(())
    }

    #[test]
    fn tx_replace_child() -> anyhow::Result<()> {
        let mut mempool = setup();
        let num_inputs = 1;
        let num_outputs = 1;
        let tx = TxGenerator::new_with_unconfirmed(&mempool, num_inputs, num_outputs)
            .generate_replaceable_tx()
            .expect("generate_replaceable_tx");
        mempool.add_transaction(tx.clone())?;

        let child_tx_input = TxInput::new(tx.get_id(), 0, DUMMY_WITNESS_MSG.to_vec());
        // We want to test that even though it doesn't signal replaceability directly, the child tx is replaceable because it's parent signalled replaceability
        // replaced
        let flags = 0;
        let locktime = 0;
        let child_tx = tx_spend_input(&mempool, child_tx_input, None, flags, locktime)?;
        mempool.add_transaction(child_tx)?;
        Ok(())
    }

    fn tx_spend_input(
        mempool: &MempoolImpl<ChainStateMock>,
        input: TxInput,
        fee: Option<Amount>,
        flags: u32,
        locktime: u32,
    ) -> anyhow::Result<Transaction> {
        tx_spend_several_inputs(mempool, &[input], fee, flags, locktime)
    }

    fn tx_spend_several_inputs(
        mempool: &MempoolImpl<ChainStateMock>,
        inputs: &[TxInput],
        fee: impl Into<Option<Amount>>,
        flags: u32,
        locktime: u32,
    ) -> anyhow::Result<Transaction> {
        let input_value = inputs
            .iter()
            .map(|input| mempool.get_input_value(input))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .sum::<Option<_>>()
            .expect("tx_spend_input: overflow");

        let output_value = if let Some(fee) = fee.into() {
            (fee <= input_value)
                .then(|| (input_value - fee).expect("tx_spend_input: subtraction error"))
                .ok_or(anyhow::anyhow!("Not enough funds"))?
        } else {
            (input_value / 2.into()).expect("tx_spend_input: division error")
        };

        Transaction::new(
            flags,
            inputs.to_owned(),
            vec![
                TxOutput::new(output_value, Destination::PublicKey),
                TxOutput::new(
                    (input_value - output_value).expect("underflow"),
                    Destination::PublicKey,
                ),
            ],
            locktime,
        )
        .map_err(Into::into)
    }

    #[test]
    fn one_ancestor_signal_is_enough() -> anyhow::Result<()> {
        let mut mempool = setup();
        let num_inputs = 1;
        let num_outputs = 2;
        let tx = TxGenerator::new_with_unconfirmed(&mempool, num_inputs, num_outputs)
            .generate_tx()
            .expect("generate_replaceable_tx");

        mempool.add_transaction(tx.clone())?;

        let flags_replaceable = 1;
        let flags_irreplaceable = 0;
        let locktime = 0;

        let ancestor_with_signal = tx_spend_input(
            &mempool,
            TxInput::new(tx.get_id(), 0, DUMMY_WITNESS_MSG.to_vec()),
            None,
            flags_replaceable,
            locktime,
        )?;

        let ancestor_without_signal = tx_spend_input(
            &mempool,
            TxInput::new(tx.get_id(), 1, DUMMY_WITNESS_MSG.to_vec()),
            None,
            flags_irreplaceable,
            locktime,
        )?;

        mempool.add_transaction(ancestor_with_signal.clone())?;
        mempool.add_transaction(ancestor_without_signal.clone())?;

        let input_with_replaceable_parent =
            TxInput::new(ancestor_with_signal.get_id(), 0, DUMMY_WITNESS_MSG.to_vec());

        let input_with_irreplaceable_parent = TxInput::new(
            ancestor_without_signal.get_id(),
            0,
            DUMMY_WITNESS_MSG.to_vec(),
        );

        let original_fee = Amount::from(10);
        let dummy_output = TxOutput::new(original_fee, Destination::PublicKey);
        let replaced_tx = tx_spend_several_inputs(
            &mempool,
            &[input_with_irreplaceable_parent.clone(), input_with_replaceable_parent],
            original_fee,
            flags_irreplaceable,
            locktime,
        )?;

        mempool.add_transaction(replaced_tx)?;

        let replacing_tx = Transaction::new(
            flags_irreplaceable,
            vec![input_with_irreplaceable_parent],
            vec![dummy_output],
            locktime,
        )?;

        mempool.add_transaction(replacing_tx)?;

        Ok(())
    }

    #[test]
    fn tx_mempool_entry_num_ancestors() -> anyhow::Result<()> {
        // Input different flag values just to make the hashes of these dummy transactions
        // different
        let tx1 = Transaction::new(1, vec![], vec![], 0).map_err(anyhow::Error::from)?;
        let tx2 = Transaction::new(2, vec![], vec![], 0).map_err(anyhow::Error::from)?;
        let tx3 = Transaction::new(3, vec![], vec![], 0).map_err(anyhow::Error::from)?;
        let tx4 = Transaction::new(4, vec![], vec![], 0).map_err(anyhow::Error::from)?;
        let tx5 = Transaction::new(5, vec![], vec![], 0).map_err(anyhow::Error::from)?;

        let tx6 = Transaction::new(6, vec![], vec![], 0).map_err(anyhow::Error::from)?;
        let fee = Amount::from(0);

        // Generation 1
        let tx1_parents = BTreeSet::default();
        let entry1 = Rc::new(TxMempoolEntry::new(tx1, fee, tx1_parents));
        let tx2_parents = BTreeSet::default();
        let entry2 = Rc::new(TxMempoolEntry::new(tx2, fee, tx2_parents));

        // Generation 2
        let tx3_parents = vec![Rc::clone(&entry1), Rc::clone(&entry2)].into_iter().collect();
        let entry3 = Rc::new(TxMempoolEntry::new(tx3, fee, tx3_parents));

        // Generation 3
        let tx4_parents = vec![Rc::clone(&entry3)].into_iter().collect();
        let tx5_parents = vec![Rc::clone(&entry3)].into_iter().collect();
        let entry4 = Rc::new(TxMempoolEntry::new(tx4, fee, tx4_parents));
        let entry5 = Rc::new(TxMempoolEntry::new(tx5, fee, tx5_parents));

        // Generation 4
        let tx6_parents = vec![Rc::clone(&entry3), Rc::clone(&entry4), Rc::clone(&entry5)]
            .into_iter()
            .collect();
        let entry6 = Rc::new(TxMempoolEntry::new(tx6, fee, tx6_parents));

        assert_eq!(entry1.unconfirmed_ancestors().len(), 0);
        assert_eq!(entry2.unconfirmed_ancestors().len(), 0);
        assert_eq!(entry3.unconfirmed_ancestors().len(), 2);
        assert_eq!(entry4.unconfirmed_ancestors().len(), 3);
        assert_eq!(entry5.unconfirmed_ancestors().len(), 3);
        assert_eq!(entry6.unconfirmed_ancestors().len(), 5);
        Ok(())
    }
}

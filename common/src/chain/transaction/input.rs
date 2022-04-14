use crate::chain::{block::Block, transaction::Transaction};
use crate::primitives::{Id, H256};
use parity_scale_codec::{Decode, Encode};

#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub enum OutPointSourceId {
    #[codec(index = 0)]
    Transaction(Id<Transaction>),
    #[codec(index = 1)]
    BlockReward(Id<Block>),
}

impl From<Id<Transaction>> for OutPointSourceId {
    fn from(id: Id<Transaction>) -> OutPointSourceId {
        OutPointSourceId::Transaction(id)
    }
}

impl From<Id<Block>> for OutPointSourceId {
    fn from(id: Id<Block>) -> OutPointSourceId {
        OutPointSourceId::BlockReward(id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct OutPoint {
    id: OutPointSourceId,
    index: u32,
}

fn outpoint_source_id_as_monolithic_tuple(v: &OutPointSourceId) -> (u8, H256) {
    let tx_out_index = 0;
    let blk_reward_index = 1;
    match v {
        OutPointSourceId::Transaction(h) => (tx_out_index, h.get()),
        OutPointSourceId::BlockReward(h) => (blk_reward_index, h.get()),
    }
}

impl PartialOrd for OutPointSourceId {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        let id = outpoint_source_id_as_monolithic_tuple(&self);
        let other_id = outpoint_source_id_as_monolithic_tuple(&other);
        println!("Comparing {:?} to {:?}", id, other_id);
        Some(id.cmp(&other_id))
    }
}

impl Ord for OutPointSourceId {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.partial_cmp(&other).expect("Comparison should never fail")
    }
}

impl PartialOrd for OutPoint {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        let id = outpoint_source_id_as_monolithic_tuple(&self.id);
        let other_id = outpoint_source_id_as_monolithic_tuple(&other.id);

        (id, self.index).partial_cmp(&(other_id, other.index))
    }
}

impl Ord for OutPoint {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self).partial_cmp(&other).expect("Comparison should never fail")
    }
}

impl OutPoint {
    pub fn new(outpoint_source_id: OutPointSourceId, output_index: u32) -> Self {
        OutPoint {
            id: outpoint_source_id,
            index: output_index,
        }
    }

    pub fn get_tx_id(&self) -> OutPointSourceId {
        self.id.clone()
    }

    pub fn get_output_index(&self) -> u32 {
        self.index
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct TxInput {
    outpoint: OutPoint,
    witness: Vec<u8>,
}

impl TxInput {
    pub fn new(outpoint_source_id: OutPointSourceId, output_index: u32, witness: Vec<u8>) -> Self {
        TxInput {
            outpoint: OutPoint::new(outpoint_source_id, output_index),
            witness,
        }
    }

    pub fn get_outpoint(&self) -> &OutPoint {
        &self.outpoint
    }

    pub fn get_witness(&self) -> &Vec<u8> {
        &self.witness
    }
}

#[cfg(test)]
mod test {
    use super::*;

    // The hash value doesn't matter because we first compare the enum arm
    fn compare_test(block_reward_hash: &H256, tx_hash: &H256) {
        let br = OutPointSourceId::BlockReward(Id::new(block_reward_hash));
        let bro0 = OutPoint::new(br.clone(), 0);
        let bro1 = OutPoint::new(br.clone(), 1);
        let bro2 = OutPoint::new(br, 2);

        let tx = OutPointSourceId::Transaction(Id::new(tx_hash));
        let txo0 = OutPoint::new(tx.clone(), 0);
        let txo1 = OutPoint::new(tx.clone(), 1);
        let txo2 = OutPoint::new(tx, 2);

        assert_eq!(bro0.cmp(&bro1), std::cmp::Ordering::Less);
        assert_eq!(bro0.cmp(&bro2), std::cmp::Ordering::Less);
        assert_eq!(bro1.cmp(&bro2), std::cmp::Ordering::Less);
        assert_eq!(bro0.cmp(&bro0), std::cmp::Ordering::Equal);
        assert_eq!(bro1.cmp(&bro1), std::cmp::Ordering::Equal);
        assert_eq!(bro2.cmp(&bro2), std::cmp::Ordering::Equal);
        assert_eq!(bro1.cmp(&bro0), std::cmp::Ordering::Greater);
        assert_eq!(bro2.cmp(&bro1), std::cmp::Ordering::Greater);
        assert_eq!(bro2.cmp(&bro0), std::cmp::Ordering::Greater);

        assert_eq!(txo0.cmp(&txo1), std::cmp::Ordering::Less);
        assert_eq!(txo0.cmp(&txo2), std::cmp::Ordering::Less);
        assert_eq!(txo1.cmp(&txo2), std::cmp::Ordering::Less);
        assert_eq!(txo0.cmp(&txo0), std::cmp::Ordering::Equal);
        assert_eq!(txo1.cmp(&txo1), std::cmp::Ordering::Equal);
        assert_eq!(txo2.cmp(&txo2), std::cmp::Ordering::Equal);
        assert_eq!(txo1.cmp(&txo0), std::cmp::Ordering::Greater);
        assert_eq!(txo2.cmp(&txo1), std::cmp::Ordering::Greater);
        assert_eq!(txo2.cmp(&txo0), std::cmp::Ordering::Greater);

        assert_eq!(bro0.cmp(&txo0), std::cmp::Ordering::Greater);
        assert_eq!(bro0.cmp(&txo1), std::cmp::Ordering::Greater);
        assert_eq!(bro0.cmp(&txo2), std::cmp::Ordering::Greater);

        assert_eq!(txo0.cmp(&bro0), std::cmp::Ordering::Less);
        assert_eq!(txo1.cmp(&bro0), std::cmp::Ordering::Less);
        assert_eq!(txo2.cmp(&bro0), std::cmp::Ordering::Less);

        assert_eq!(txo0.cmp(&bro1), std::cmp::Ordering::Less);
        assert_eq!(txo1.cmp(&bro1), std::cmp::Ordering::Less);
        assert_eq!(txo2.cmp(&bro1), std::cmp::Ordering::Less);

        assert_eq!(txo0.cmp(&bro2), std::cmp::Ordering::Less);
        assert_eq!(txo1.cmp(&bro2), std::cmp::Ordering::Less);
        assert_eq!(txo2.cmp(&bro2), std::cmp::Ordering::Less);

        assert_eq!(bro1.cmp(&txo1), std::cmp::Ordering::Greater);
        assert_eq!(txo1.cmp(&bro1), std::cmp::Ordering::Less);

        assert_eq!(bro2.cmp(&txo2), std::cmp::Ordering::Greater);
        assert_eq!(txo2.cmp(&bro2), std::cmp::Ordering::Less);
    }

    #[test]
    fn ord_and_equality_less() {
        let hash_br = H256::from_low_u64_le(10);
        let hash_tx = H256::from_low_u64_le(20);

        compare_test(&hash_br, &hash_tx);
    }

    #[test]
    fn ord_and_equality_greater() {
        let hash_br = H256::from_low_u64_le(20);
        let hash_tx = H256::from_low_u64_le(10);

        compare_test(&hash_br, &hash_tx);
    }

    #[test]
    fn ord_and_equality_random() {
        let hash_br = H256::random();
        let hash_tx = H256::random();

        compare_test(&hash_br, &hash_tx);
    }
}

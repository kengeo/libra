// Copyright (c) The Libra Core Contributors
// SPDX-License-Identifier: Apache-2.0

use super::{balance_ap, encode_mint_transaction, encode_transfer_transaction, seqnum_ap, MockVM};
use config::config::VMConfig;
use failure::Result;
use libra_types::{
    access_path::AccessPath,
    account_address::{AccountAddress, ADDRESS_LENGTH},
    transaction::Transaction,
    write_set::WriteOp,
};
use state_view::StateView;
use vm_runtime::VMExecutor;

fn gen_address(index: u8) -> AccountAddress {
    AccountAddress::new([index; ADDRESS_LENGTH])
}

struct MockStateView;

impl StateView for MockStateView {
    fn get(&self, _access_path: &AccessPath) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }

    fn multi_get(&self, _access_paths: &[AccessPath]) -> Result<Vec<Option<Vec<u8>>>> {
        unimplemented!();
    }

    fn is_genesis(&self) -> bool {
        false
    }
}

#[test]
fn test_mock_vm_different_senders() {
    let amount = 100;
    let mut txns = vec![];
    for i in 0..10 {
        txns.push(Transaction::UserTransaction(encode_mint_transaction(
            gen_address(i),
            amount,
        )));
    }

    let outputs = MockVM::execute_block(
        txns.clone(),
        &VMConfig::empty_whitelist_FOR_TESTING(),
        &MockStateView,
    );

    for (output, txn) in itertools::zip_eq(outputs.iter(), txns.iter()) {
        let sender = txn.as_signed_user_txn().unwrap().sender();
        assert_eq!(
            output.write_set().iter().cloned().collect::<Vec<_>>(),
            vec![
                (
                    balance_ap(sender),
                    WriteOp::Value(amount.to_le_bytes().to_vec())
                ),
                (
                    seqnum_ap(sender),
                    WriteOp::Value(1u64.to_le_bytes().to_vec())
                ),
            ]
        );
    }
}

#[test]
fn test_mock_vm_same_sender() {
    let amount = 100;
    let sender = gen_address(1);
    let mut txns = vec![];
    for _i in 0..10 {
        txns.push(Transaction::UserTransaction(encode_mint_transaction(
            sender, amount,
        )));
    }

    let outputs = MockVM::execute_block(
        txns,
        &VMConfig::empty_whitelist_FOR_TESTING(),
        &MockStateView,
    );

    for (i, output) in outputs.iter().enumerate() {
        assert_eq!(
            output.write_set().iter().cloned().collect::<Vec<_>>(),
            vec![
                (
                    balance_ap(sender),
                    WriteOp::Value((amount * (i as u64 + 1)).to_le_bytes().to_vec())
                ),
                (
                    seqnum_ap(sender),
                    WriteOp::Value((i as u64 + 1).to_le_bytes().to_vec())
                ),
            ]
        );
    }
}

#[test]
fn test_mock_vm_payment() {
    let mut txns = vec![];
    txns.push(encode_mint_transaction(gen_address(0), 100));
    txns.push(encode_mint_transaction(gen_address(1), 100));
    txns.push(encode_transfer_transaction(
        gen_address(0),
        gen_address(1),
        50,
    ));

    let output = MockVM::execute_block(
        txns.into_iter().map(Transaction::UserTransaction).collect(),
        &VMConfig::empty_whitelist_FOR_TESTING(),
        &MockStateView,
    );

    let mut output_iter = output.iter();
    output_iter.next();
    output_iter.next();
    assert_eq!(
        output_iter
            .next()
            .unwrap()
            .write_set()
            .iter()
            .cloned()
            .collect::<Vec<_>>(),
        vec![
            (
                balance_ap(gen_address(0)),
                WriteOp::Value(50u64.to_le_bytes().to_vec())
            ),
            (
                seqnum_ap(gen_address(0)),
                WriteOp::Value(2u64.to_le_bytes().to_vec())
            ),
            (
                balance_ap(gen_address(1)),
                WriteOp::Value(150u64.to_le_bytes().to_vec())
            ),
        ]
    );
}

use shell_core::{AccessListItem, Transaction};
use shell_primitives::{Address, Bytes, ShellHash, U256};

#[test]
fn sdk_hash_transaction_golden_vector_matches_chain() {
    let tx = Transaction {
        chain_id: 1337,
        nonce: 7,
        to: Some(Address::from([0x11; 20])),
        value: U256::from(0x1234u64),
        data: Bytes::from(vec![0xde, 0xad, 0xbe, 0xef]),
        gas_limit: 50_000,
        max_fee_per_gas: 1_000_000_000,
        max_priority_fee_per_gas: 250_000_000,
        access_list: Some(vec![AccessListItem {
            address: Address::from([0x22; 20]),
            storage_keys: vec![ShellHash::from([0x33; 32]), ShellHash::from([0x44; 32])],
        }]),
        tx_type: 3,
        max_fee_per_blob_gas: Some(0),
        blob_versioned_hashes: Some(vec![ShellHash::from([0x55; 32])]),
    };

    // This vector commits to the access list under the V2 signing domain.
    // shell-sdk hashTransaction() must use the same domain and field order.
    let expected = ShellHash::from([
        0x1c, 0x0e, 0x0e, 0x9b, 0xd5, 0x59, 0xaa, 0xe3, 0x20, 0x4e, 0xb2, 0xfd, 0x11, 0xa7, 0x3a,
        0xec, 0xdd, 0xa3, 0x4b, 0x92, 0x21, 0xcb, 0xc2, 0x68, 0x98, 0x68, 0x7f, 0x65, 0xde, 0xbc,
        0xe5, 0x46,
    ]);

    assert_eq!(tx.hash(), expected);
}

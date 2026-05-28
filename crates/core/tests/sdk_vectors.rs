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

    // Updated golden after adding PQTX_SIGNING_V1\0 domain prefix (WP §1503-1509).
    // shell-sdk hashTransaction() must be updated to prepend the same 16-byte domain.
    // Previous (no-domain): 0xf5a14a12f556ff79fff941e944519f1c965b80e53c91503a676ff0a891ef0836
    let expected = ShellHash::from([
        0x68, 0xee, 0xa4, 0x69, 0x4a, 0xb0, 0xfb, 0xa5, 0x49, 0xe5, 0xb5, 0x2b, 0xe4, 0x72, 0x98,
        0x4c, 0x61, 0x21, 0xf0, 0x95, 0xd8, 0x3d, 0xb5, 0x51, 0x5a, 0x59, 0xcc, 0x34, 0x5c, 0xcc,
        0x47, 0x61,
    ]);

    assert_eq!(tx.hash(), expected);
}

//! Native system contract: ValidatorRegistry at address 0x0000…0001.
//!
//! Instead of deploying Solidity bytecode, this contract is intercepted by the
//! EVM executor and executed as native Rust code. This avoids the need for a
//! Solidity compiler and ensures deterministic, efficient validator management.
//!
//! # Supported Functions
//!
//! | Selector | Signature                  | Access     |
//! |----------|----------------------------|------------|
//! | `execute_system_contract` dispatches: |            |
//! | 0x4d238c8e | `addValidator(address)`   | validators |
//! | 0x40a141ff | `removeValidator(address)` | validators |
//! | 0xb7ab4db5 | `getValidators()`          | anyone     |
//! | 0xfacd743b | `isValidator(address)`     | anyone     |

use shell_primitives::{keccak256, Address};
use shell_storage::{KvStore, WorldState};

// ── Contract address ───────────────────────────────────────────────

/// System contract address for ValidatorRegistry: 0x0000…0001.
pub const VALIDATOR_REGISTRY_ADDR: [u8; 20] = {
    let mut addr = [0u8; 20];
    addr[19] = 1;
    addr
};

/// Return the system contract address as a shell `Address`.
pub fn registry_address() -> Address {
    Address::from(VALIDATOR_REGISTRY_ADDR)
}

// ── Function selectors (keccak256 of signature, first 4 bytes) ────

/// keccak256("addValidator(address)")[..4]
pub const ADD_VALIDATOR_SELECTOR: [u8; 4] = compute_selector(b"addValidator(address)");
/// keccak256("removeValidator(address)")[..4]
pub const REMOVE_VALIDATOR_SELECTOR: [u8; 4] = compute_selector(b"removeValidator(address)");
/// keccak256("getValidators()")[..4]
pub const GET_VALIDATORS_SELECTOR: [u8; 4] = compute_selector(b"getValidators()");
/// keccak256("isValidator(address)")[..4]
pub const IS_VALIDATOR_SELECTOR: [u8; 4] = compute_selector(b"isValidator(address)");

/// Compute a 4-byte function selector at compile time.
const fn compute_selector(sig: &[u8]) -> [u8; 4] {
    let hash = const_keccak256(sig);
    [hash[0], hash[1], hash[2], hash[3]]
}

// ── Event topic signatures ─────────────────────────────────────────

/// keccak256("ValidatorAdded(address)")
pub fn validator_added_topic() -> [u8; 32] {
    *keccak256(b"ValidatorAdded(address)").as_bytes()
}

/// keccak256("ValidatorRemoved(address)")
pub fn validator_removed_topic() -> [u8; 32] {
    *keccak256(b"ValidatorRemoved(address)").as_bytes()
}

// ── Gas constants ──────────────────────────────────────────────────

/// Base gas cost for a system contract call (same as a normal tx).
pub const SYSTEM_CALL_BASE_GAS: u64 = 21_000;
/// Additional gas per state-mutating operation.
pub const SYSTEM_CALL_OP_GAS: u64 = 5_000;

// ── Errors ─────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum SystemContractError {
    #[error("input too short: need at least 4 bytes for selector")]
    InputTooShort,
    #[error("unknown function selector: 0x{}", hex::encode(.0))]
    UnknownSelector([u8; 4]),
    #[error("unauthorized: caller is not a validator")]
    Unauthorized,
    #[error("validator already exists: {0}")]
    AlreadyExists(Address),
    #[error("validator not found: {0}")]
    NotFound(Address),
    #[error("cannot remove last validator")]
    LastValidator,
    #[error("invalid ABI parameter: {0}")]
    AbiDecode(String),
    #[error("storage error: {0}")]
    Storage(String),
}

// ── Main dispatch ──────────────────────────────────────────────────

/// Execute the ValidatorRegistry system contract.
///
/// Returns `(output_bytes, gas_used)` on success.
pub fn execute_system_contract<S: KvStore + 'static>(
    caller: &Address,
    input: &[u8],
    world_state: &mut WorldState<S>,
) -> Result<(Vec<u8>, u64), SystemContractError> {
    if input.len() < 4 {
        return Err(SystemContractError::InputTooShort);
    }

    let selector: [u8; 4] = input[..4].try_into().unwrap();
    let params = &input[4..];

    match selector {
        s if s == ADD_VALIDATOR_SELECTOR => {
            let addr = decode_address(params)?;
            add_validator(caller, &addr, world_state)?;
            let gas = SYSTEM_CALL_BASE_GAS + SYSTEM_CALL_OP_GAS;
            Ok((encode_bool(true), gas))
        }
        s if s == REMOVE_VALIDATOR_SELECTOR => {
            let addr = decode_address(params)?;
            remove_validator(caller, &addr, world_state)?;
            let gas = SYSTEM_CALL_BASE_GAS + SYSTEM_CALL_OP_GAS;
            Ok((encode_bool(true), gas))
        }
        s if s == GET_VALIDATORS_SELECTOR => {
            let validators = world_state
                .get_validators()
                .map_err(|e| SystemContractError::Storage(e.to_string()))?;
            Ok((encode_address_array(&validators), SYSTEM_CALL_BASE_GAS))
        }
        s if s == IS_VALIDATOR_SELECTOR => {
            let addr = decode_address(params)?;
            let validators = world_state
                .get_validators()
                .map_err(|e| SystemContractError::Storage(e.to_string()))?;
            let is_val = validators.contains(&addr);
            Ok((encode_bool(is_val), SYSTEM_CALL_BASE_GAS))
        }
        _ => Err(SystemContractError::UnknownSelector(selector)),
    }
}

// ── Mutating operations ────────────────────────────────────────────

fn add_validator<S: KvStore + 'static>(
    caller: &Address,
    target: &Address,
    world_state: &mut WorldState<S>,
) -> Result<(), SystemContractError> {
    let mut validators = world_state
        .get_validators()
        .map_err(|e| SystemContractError::Storage(e.to_string()))?;

    // Authorization: caller must be an existing validator
    if !validators.contains(caller) {
        return Err(SystemContractError::Unauthorized);
    }

    // Duplicate check
    if validators.contains(target) {
        return Err(SystemContractError::AlreadyExists(*target));
    }

    validators.push(*target);
    world_state
        .set_validators(&validators)
        .map_err(|e| SystemContractError::Storage(e.to_string()))?;

    Ok(())
}

fn remove_validator<S: KvStore + 'static>(
    caller: &Address,
    target: &Address,
    world_state: &mut WorldState<S>,
) -> Result<(), SystemContractError> {
    let mut validators = world_state
        .get_validators()
        .map_err(|e| SystemContractError::Storage(e.to_string()))?;

    // Authorization: caller must be an existing validator
    if !validators.contains(caller) {
        return Err(SystemContractError::Unauthorized);
    }

    // Cannot remove the last validator
    if validators.len() <= 1 {
        return Err(SystemContractError::LastValidator);
    }

    let pos = validators
        .iter()
        .position(|v| v == target)
        .ok_or(SystemContractError::NotFound(*target))?;

    validators.remove(pos);
    world_state
        .set_validators(&validators)
        .map_err(|e| SystemContractError::Storage(e.to_string()))?;

    Ok(())
}

// ── ABI helpers ────────────────────────────────────────────────────

/// Decode a single ABI-encoded `address` parameter (32 bytes, left-padded with zeros).
pub fn decode_address(input: &[u8]) -> Result<Address, SystemContractError> {
    if input.len() < 32 {
        return Err(SystemContractError::AbiDecode(format!(
            "expected 32 bytes for address, got {}",
            input.len()
        )));
    }
    // ABI: address is right-aligned in 32-byte word (bytes 12..32)
    Address::try_from_slice(&input[12..32])
        .map_err(|e| SystemContractError::AbiDecode(e.to_string()))
}

/// ABI-encode a `bool` as a 32-byte word.
pub fn encode_bool(val: bool) -> Vec<u8> {
    let mut out = vec![0u8; 32];
    if val {
        out[31] = 1;
    }
    out
}

/// ABI-encode a dynamic array of addresses.
///
/// Layout:
/// - word 0: offset to data (= 0x20)
/// - word 1: array length
/// - word 2..N+2: each address left-padded to 32 bytes
pub fn encode_address_array(addrs: &[Address]) -> Vec<u8> {
    let mut out = Vec::with_capacity(64 + addrs.len() * 32);

    // Offset to dynamic data
    let mut offset = [0u8; 32];
    offset[31] = 0x20;
    out.extend_from_slice(&offset);

    // Length
    let mut len_word = [0u8; 32];
    let len_bytes = (addrs.len() as u64).to_be_bytes();
    len_word[24..32].copy_from_slice(&len_bytes);
    out.extend_from_slice(&len_word);

    // Elements
    for addr in addrs {
        let mut word = [0u8; 32];
        word[12..32].copy_from_slice(addr.as_bytes());
        out.extend_from_slice(&word);
    }

    out
}

/// Encode calldata for `addValidator(address)`.
pub fn encode_add_validator_calldata(address: &Address) -> Vec<u8> {
    let mut data = Vec::with_capacity(4 + 32);
    data.extend_from_slice(&ADD_VALIDATOR_SELECTOR);
    let mut word = [0u8; 32];
    word[12..32].copy_from_slice(address.as_bytes());
    data.extend_from_slice(&word);
    data
}

/// Encode calldata for `removeValidator(address)`.
pub fn encode_remove_validator_calldata(address: &Address) -> Vec<u8> {
    let mut data = Vec::with_capacity(4 + 32);
    data.extend_from_slice(&REMOVE_VALIDATOR_SELECTOR);
    let mut word = [0u8; 32];
    word[12..32].copy_from_slice(address.as_bytes());
    data.extend_from_slice(&word);
    data
}

// ── Const Keccak-256 (compile-time) ────────────────────────────────

/// Minimal const-compatible Keccak-256 used solely for selector computation.
/// Produces the same output as `sha3::Keccak256`.
const fn const_keccak256(data: &[u8]) -> [u8; 32] {
    // Keccak-256 parameters: rate=136, capacity=64, delimited suffix=0x01
    const RATE: usize = 136;
    let mut state = [0u64; 25];

    // Absorb: pad input with Keccak padding (0x01 … 0x80)
    let mut block = [0u8; RATE];
    let mut offset = 0;
    let mut i = 0;
    while i < data.len() {
        block[offset] = data[i];
        offset += 1;
        if offset == RATE {
            state = xor_block(state, &block);
            state = keccak_f1600(state);
            block = [0u8; RATE];
            offset = 0;
        }
        i += 1;
    }
    block[offset] ^= 0x01; // Keccak domain separator
    block[RATE - 1] ^= 0x80; // padding end
    state = xor_block(state, &block);
    state = keccak_f1600(state);

    // Squeeze: first 32 bytes
    let mut out = [0u8; 32];
    let mut j = 0;
    while j < 32 {
        let lane = j / 8;
        let byte_in_lane = j % 8;
        out[j] = (state[lane] >> (8 * byte_in_lane)) as u8;
        j += 1;
    }
    out
}

const fn xor_block(mut state: [u64; 25], block: &[u8; 136]) -> [u64; 25] {
    let mut i = 0;
    while i < 136 / 8 {
        let b = i * 8;
        let lane = (block[b] as u64)
            | (block[b + 1] as u64) << 8
            | (block[b + 2] as u64) << 16
            | (block[b + 3] as u64) << 24
            | (block[b + 4] as u64) << 32
            | (block[b + 5] as u64) << 40
            | (block[b + 6] as u64) << 48
            | (block[b + 7] as u64) << 56;
        state[i] ^= lane;
        i += 1;
    }
    state
}

const fn keccak_f1600(mut state: [u64; 25]) -> [u64; 25] {
    const RC: [u64; 24] = [
        0x0000000000000001, 0x0000000000008082, 0x800000000000808A,
        0x8000000080008000, 0x000000000000808B, 0x0000000080000001,
        0x8000000080008081, 0x8000000000008009, 0x000000000000008A,
        0x0000000000000088, 0x0000000080008009, 0x000000008000000A,
        0x000000008000808B, 0x800000000000008B, 0x8000000000008089,
        0x8000000000008003, 0x8000000000008002, 0x8000000000000080,
        0x000000000000800A, 0x800000008000000A, 0x8000000080008081,
        0x8000000000008080, 0x0000000080000001, 0x8000000080008008,
    ];
    const ROT: [u32; 24] = [
        1, 3, 6, 10, 15, 21, 28, 36, 45, 55, 2, 14,
        27, 41, 56, 8, 25, 43, 62, 18, 39, 61, 20, 44,
    ];
    const PI: [usize; 24] = [
        10, 7, 11, 17, 18, 3, 5, 16, 8, 21, 24, 4,
        15, 23, 19, 13, 12, 2, 20, 14, 22, 9, 6, 1,
    ];

    let mut round = 0;
    while round < 24 {
        // θ
        let mut c = [0u64; 5];
        let mut x = 0;
        while x < 5 {
            c[x] = state[x] ^ state[x + 5] ^ state[x + 10] ^ state[x + 15] ^ state[x + 20];
            x += 1;
        }
        let mut d = [0u64; 5];
        x = 0;
        while x < 5 {
            d[x] = c[(x + 4) % 5] ^ c[(x + 1) % 5].rotate_left(1);
            x += 1;
        }
        x = 0;
        while x < 25 {
            state[x] ^= d[x % 5];
            x += 1;
        }

        // ρ and π
        let mut current = state[1];
        let mut t = 0;
        while t < 24 {
            let j = PI[t];
            let temp = state[j];
            state[j] = current.rotate_left(ROT[t]);
            current = temp;
            t += 1;
        }

        // χ
        let mut y = 0;
        while y < 5 {
            let base = y * 5;
            let t0 = state[base];
            let t1 = state[base + 1];
            let t2 = state[base + 2];
            let t3 = state[base + 3];
            let t4 = state[base + 4];
            state[base] = t0 ^ (!t1 & t2);
            state[base + 1] = t1 ^ (!t2 & t3);
            state[base + 2] = t2 ^ (!t3 & t4);
            state[base + 3] = t3 ^ (!t4 & t0);
            state[base + 4] = t4 ^ (!t0 & t1);
            y += 1;
        }

        // ι
        state[0] ^= RC[round];
        round += 1;
    }
    state
}

// ── Placeholder code hash for the system contract ──────────────────

/// A deterministic code hash for the ValidatorRegistry system contract.
/// Used so `eth_getCode` returns a non-empty marker for this address.
///
/// Value: keccak256("ValidatorRegistry")
pub fn system_contract_code_hash() -> shell_primitives::ShellHash {
    keccak256(b"ValidatorRegistry")
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use shell_storage::MemoryDb;
    use std::sync::Arc;

    fn setup_with_validators(validators: &[Address]) -> WorldState<MemoryDb> {
        let store = Arc::new(MemoryDb::new());
        let mut ws = WorldState::new(store);
        if !validators.is_empty() {
            ws.set_validators(validators).unwrap();
        }
        ws
    }

    // ── Selector computation ───────────────────────────────────

    #[test]
    fn selector_add_validator() {
        let hash = keccak256(b"addValidator(address)");
        let expected = &hash.as_bytes()[..4];
        assert_eq!(&ADD_VALIDATOR_SELECTOR, expected);
    }

    #[test]
    fn selector_remove_validator() {
        let hash = keccak256(b"removeValidator(address)");
        let expected = &hash.as_bytes()[..4];
        assert_eq!(&REMOVE_VALIDATOR_SELECTOR, expected);
    }

    #[test]
    fn selector_get_validators() {
        let hash = keccak256(b"getValidators()");
        let expected = &hash.as_bytes()[..4];
        assert_eq!(&GET_VALIDATORS_SELECTOR, expected);
    }

    #[test]
    fn selector_is_validator() {
        let hash = keccak256(b"isValidator(address)");
        let expected = &hash.as_bytes()[..4];
        assert_eq!(&IS_VALIDATOR_SELECTOR, expected);
    }

    // ── addValidator ───────────────────────────────────────────

    #[test]
    fn add_validator_authorized_success() {
        let v1 = Address::from([0x01; 20]);
        let new_val = Address::from([0x02; 20]);
        let mut ws = setup_with_validators(&[v1]);

        let calldata = encode_add_validator_calldata(&new_val);
        let (output, gas) = execute_system_contract(&v1, &calldata, &mut ws).unwrap();

        assert_eq!(output, encode_bool(true));
        assert_eq!(gas, SYSTEM_CALL_BASE_GAS + SYSTEM_CALL_OP_GAS);

        let validators = ws.get_validators().unwrap();
        assert_eq!(validators.len(), 2);
        assert!(validators.contains(&new_val));
    }

    #[test]
    fn add_validator_unauthorized_fails() {
        let v1 = Address::from([0x01; 20]);
        let outsider = Address::from([0x99; 20]);
        let new_val = Address::from([0x02; 20]);
        let mut ws = setup_with_validators(&[v1]);

        let calldata = encode_add_validator_calldata(&new_val);
        let err = execute_system_contract(&outsider, &calldata, &mut ws).unwrap_err();
        assert!(matches!(err, SystemContractError::Unauthorized));
    }

    #[test]
    fn add_validator_duplicate_fails() {
        let v1 = Address::from([0x01; 20]);
        let mut ws = setup_with_validators(&[v1]);

        let calldata = encode_add_validator_calldata(&v1);
        let err = execute_system_contract(&v1, &calldata, &mut ws).unwrap_err();
        assert!(matches!(err, SystemContractError::AlreadyExists(_)));
    }

    // ── removeValidator ────────────────────────────────────────

    #[test]
    fn remove_validator_success() {
        let v1 = Address::from([0x01; 20]);
        let v2 = Address::from([0x02; 20]);
        let mut ws = setup_with_validators(&[v1, v2]);

        let calldata = encode_remove_validator_calldata(&v2);
        let (output, gas) = execute_system_contract(&v1, &calldata, &mut ws).unwrap();

        assert_eq!(output, encode_bool(true));
        assert_eq!(gas, SYSTEM_CALL_BASE_GAS + SYSTEM_CALL_OP_GAS);

        let validators = ws.get_validators().unwrap();
        assert_eq!(validators, vec![v1]);
    }

    #[test]
    fn remove_validator_last_fails() {
        let v1 = Address::from([0x01; 20]);
        let mut ws = setup_with_validators(&[v1]);

        let calldata = encode_remove_validator_calldata(&v1);
        let err = execute_system_contract(&v1, &calldata, &mut ws).unwrap_err();
        assert!(matches!(err, SystemContractError::LastValidator));
    }

    #[test]
    fn remove_validator_not_found_fails() {
        let v1 = Address::from([0x01; 20]);
        let v2 = Address::from([0x02; 20]);
        let unknown = Address::from([0xFF; 20]);
        let mut ws = setup_with_validators(&[v1, v2]);

        let calldata = encode_remove_validator_calldata(&unknown);
        let err = execute_system_contract(&v1, &calldata, &mut ws).unwrap_err();
        assert!(matches!(err, SystemContractError::NotFound(_)));
    }

    #[test]
    fn remove_validator_unauthorized_fails() {
        let v1 = Address::from([0x01; 20]);
        let v2 = Address::from([0x02; 20]);
        let outsider = Address::from([0x99; 20]);
        let mut ws = setup_with_validators(&[v1, v2]);

        let calldata = encode_remove_validator_calldata(&v2);
        let err = execute_system_contract(&outsider, &calldata, &mut ws).unwrap_err();
        assert!(matches!(err, SystemContractError::Unauthorized));
    }

    // ── getValidators ──────────────────────────────────────────

    #[test]
    fn get_validators_returns_list() {
        let v1 = Address::from([0x01; 20]);
        let v2 = Address::from([0x02; 20]);
        let v3 = Address::from([0x03; 20]);
        let mut ws = setup_with_validators(&[v1, v2, v3]);

        let calldata = GET_VALIDATORS_SELECTOR.to_vec();
        let (output, gas) = execute_system_contract(&Address::ZERO, &calldata, &mut ws).unwrap();

        assert_eq!(gas, SYSTEM_CALL_BASE_GAS);

        // Decode the output: offset(32) + len(32) + 3 * address(32) = 5 * 32
        assert_eq!(output.len(), 5 * 32);

        // Check length word
        let len = u64::from_be_bytes(output[56..64].try_into().unwrap());
        assert_eq!(len, 3);

        // Check addresses
        let a1 = Address::try_from_slice(&output[76..96]).unwrap();
        let a2 = Address::try_from_slice(&output[108..128]).unwrap();
        let a3 = Address::try_from_slice(&output[140..160]).unwrap();
        assert_eq!(a1, v1);
        assert_eq!(a2, v2);
        assert_eq!(a3, v3);
    }

    #[test]
    fn get_validators_empty() {
        let mut ws = setup_with_validators(&[]);

        let calldata = GET_VALIDATORS_SELECTOR.to_vec();
        let (output, _) = execute_system_contract(&Address::ZERO, &calldata, &mut ws).unwrap();

        // offset + len(0)
        assert_eq!(output.len(), 64);
        let len = u64::from_be_bytes(output[56..64].try_into().unwrap());
        assert_eq!(len, 0);
    }

    // ── isValidator ────────────────────────────────────────────

    #[test]
    fn is_validator_true() {
        let v1 = Address::from([0x01; 20]);
        let mut ws = setup_with_validators(&[v1]);

        let mut calldata = IS_VALIDATOR_SELECTOR.to_vec();
        let mut word = [0u8; 32];
        word[12..32].copy_from_slice(v1.as_bytes());
        calldata.extend_from_slice(&word);

        let (output, _) = execute_system_contract(&Address::ZERO, &calldata, &mut ws).unwrap();
        assert_eq!(output, encode_bool(true));
    }

    #[test]
    fn is_validator_false() {
        let v1 = Address::from([0x01; 20]);
        let outsider = Address::from([0xFF; 20]);
        let mut ws = setup_with_validators(&[v1]);

        let mut calldata = IS_VALIDATOR_SELECTOR.to_vec();
        let mut word = [0u8; 32];
        word[12..32].copy_from_slice(outsider.as_bytes());
        calldata.extend_from_slice(&word);

        let (output, _) = execute_system_contract(&Address::ZERO, &calldata, &mut ws).unwrap();
        assert_eq!(output, encode_bool(false));
    }

    // ── ABI encoding/decoding ──────────────────────────────────

    #[test]
    fn decode_address_valid() {
        let addr = Address::from([0xAB; 20]);
        let mut word = [0u8; 32];
        word[12..32].copy_from_slice(addr.as_bytes());

        let decoded = decode_address(&word).unwrap();
        assert_eq!(decoded, addr);
    }

    #[test]
    fn decode_address_too_short() {
        let short = [0u8; 16];
        let err = decode_address(&short).unwrap_err();
        assert!(matches!(err, SystemContractError::AbiDecode(_)));
    }

    #[test]
    fn encode_bool_true() {
        let encoded = encode_bool(true);
        assert_eq!(encoded.len(), 32);
        assert_eq!(encoded[31], 1);
        assert!(encoded[..31].iter().all(|&b| b == 0));
    }

    #[test]
    fn encode_bool_false() {
        let encoded = encode_bool(false);
        assert_eq!(encoded.len(), 32);
        assert!(encoded.iter().all(|&b| b == 0));
    }

    #[test]
    fn encode_address_array_roundtrip() {
        let addrs = vec![
            Address::from([0x11; 20]),
            Address::from([0x22; 20]),
        ];
        let encoded = encode_address_array(&addrs);

        // offset(32) + len(32) + 2 * elem(32) = 128 bytes
        assert_eq!(encoded.len(), 128);

        // offset = 0x20
        assert_eq!(encoded[31], 0x20);

        // length = 2
        let len = u64::from_be_bytes(encoded[56..64].try_into().unwrap());
        assert_eq!(len, 2);

        // First address
        let a1 = Address::try_from_slice(&encoded[76..96]).unwrap();
        assert_eq!(a1, addrs[0]);

        // Second address
        let a2 = Address::try_from_slice(&encoded[108..128]).unwrap();
        assert_eq!(a2, addrs[1]);
    }

    #[test]
    fn encode_calldata_add_validator() {
        let addr = Address::from([0xDE; 20]);
        let calldata = encode_add_validator_calldata(&addr);

        assert_eq!(calldata.len(), 36);
        assert_eq!(&calldata[..4], &ADD_VALIDATOR_SELECTOR);
        let decoded = decode_address(&calldata[4..]).unwrap();
        assert_eq!(decoded, addr);
    }

    #[test]
    fn encode_calldata_remove_validator() {
        let addr = Address::from([0xBE; 20]);
        let calldata = encode_remove_validator_calldata(&addr);

        assert_eq!(calldata.len(), 36);
        assert_eq!(&calldata[..4], &REMOVE_VALIDATOR_SELECTOR);
        let decoded = decode_address(&calldata[4..]).unwrap();
        assert_eq!(decoded, addr);
    }

    // ── Edge cases ─────────────────────────────────────────────

    #[test]
    fn input_too_short() {
        let mut ws = setup_with_validators(&[]);
        let err = execute_system_contract(&Address::ZERO, &[0x00, 0x01], &mut ws).unwrap_err();
        assert!(matches!(err, SystemContractError::InputTooShort));
    }

    #[test]
    fn unknown_selector() {
        let mut ws = setup_with_validators(&[]);
        let input = [0xDE, 0xAD, 0xBE, 0xEF];
        let err = execute_system_contract(&Address::ZERO, &input, &mut ws).unwrap_err();
        assert!(matches!(err, SystemContractError::UnknownSelector(_)));
    }

    #[test]
    fn const_keccak256_matches_runtime() {
        // Verify the const keccak matches the runtime one for our signatures
        let runtime = keccak256(b"addValidator(address)");
        let compile_time = const_keccak256(b"addValidator(address)");
        assert_eq!(runtime.as_bytes(), &compile_time);

        let runtime = keccak256(b"removeValidator(address)");
        let compile_time = const_keccak256(b"removeValidator(address)");
        assert_eq!(runtime.as_bytes(), &compile_time);

        let runtime = keccak256(b"getValidators()");
        let compile_time = const_keccak256(b"getValidators()");
        assert_eq!(runtime.as_bytes(), &compile_time);

        let runtime = keccak256(b"isValidator(address)");
        let compile_time = const_keccak256(b"isValidator(address)");
        assert_eq!(runtime.as_bytes(), &compile_time);
    }

    // ── Multiple sequential operations ─────────────────────────

    #[test]
    fn sequential_add_then_remove_multiple() {
        let v1 = Address::from([0x01; 20]);
        let v2 = Address::from([0x02; 20]);
        let v3 = Address::from([0x03; 20]);
        let v4 = Address::from([0x04; 20]);
        let mut ws = setup_with_validators(&[v1]);

        // v1 adds v2
        let calldata = encode_add_validator_calldata(&v2);
        execute_system_contract(&v1, &calldata, &mut ws).unwrap();
        assert_eq!(ws.get_validators().unwrap().len(), 2);

        // v2 adds v3
        let calldata = encode_add_validator_calldata(&v3);
        execute_system_contract(&v2, &calldata, &mut ws).unwrap();
        assert_eq!(ws.get_validators().unwrap().len(), 3);

        // v3 adds v4
        let calldata = encode_add_validator_calldata(&v4);
        execute_system_contract(&v3, &calldata, &mut ws).unwrap();
        assert_eq!(ws.get_validators().unwrap().len(), 4);

        // v1 removes v2
        let calldata = encode_remove_validator_calldata(&v2);
        execute_system_contract(&v1, &calldata, &mut ws).unwrap();
        let validators = ws.get_validators().unwrap();
        assert_eq!(validators.len(), 3);
        assert!(!validators.contains(&v2));

        // v3 removes v4
        let calldata = encode_remove_validator_calldata(&v4);
        execute_system_contract(&v3, &calldata, &mut ws).unwrap();
        let validators = ws.get_validators().unwrap();
        assert_eq!(validators.len(), 2);
        assert!(validators.contains(&v1));
        assert!(validators.contains(&v3));
    }

    #[test]
    fn add_remove_then_re_add_same_validator() {
        let v1 = Address::from([0x01; 20]);
        let v2 = Address::from([0x02; 20]);
        let mut ws = setup_with_validators(&[v1, v2]);

        // Remove v2
        let calldata = encode_remove_validator_calldata(&v2);
        execute_system_contract(&v1, &calldata, &mut ws).unwrap();
        assert!(!ws.get_validators().unwrap().contains(&v2));

        // Re-add v2
        let calldata = encode_add_validator_calldata(&v2);
        execute_system_contract(&v1, &calldata, &mut ws).unwrap();
        assert!(ws.get_validators().unwrap().contains(&v2));
        assert_eq!(ws.get_validators().unwrap().len(), 2);
    }

    // ── Event encoding correctness ─────────────────────────────

    #[test]
    fn validator_added_topic_matches_keccak() {
        let expected = keccak256(b"ValidatorAdded(address)");
        let topic = validator_added_topic();
        assert_eq!(topic, *expected.as_bytes());
    }

    #[test]
    fn validator_removed_topic_matches_keccak() {
        let expected = keccak256(b"ValidatorRemoved(address)");
        let topic = validator_removed_topic();
        assert_eq!(topic, *expected.as_bytes());
    }

    #[test]
    fn event_topics_are_distinct() {
        let added = validator_added_topic();
        let removed = validator_removed_topic();
        assert_ne!(added, removed);
    }

    // ── Additional ABI encoding edge cases ─────────────────────

    #[test]
    fn encode_address_array_single_element() {
        let addr = Address::from([0xAA; 20]);
        let encoded = encode_address_array(&[addr]);

        // offset(32) + len(32) + 1 * elem(32) = 96 bytes
        assert_eq!(encoded.len(), 96);

        // length = 1
        let len = u64::from_be_bytes(encoded[56..64].try_into().unwrap());
        assert_eq!(len, 1);

        // Address
        let decoded = Address::try_from_slice(&encoded[76..96]).unwrap();
        assert_eq!(decoded, addr);
    }

    #[test]
    fn encode_address_array_empty_is_just_header() {
        let encoded = encode_address_array(&[]);

        // offset(32) + len(32) = 64 bytes
        assert_eq!(encoded.len(), 64);

        // offset = 0x20
        assert_eq!(encoded[31], 0x20);

        // length = 0
        let len = u64::from_be_bytes(encoded[56..64].try_into().unwrap());
        assert_eq!(len, 0);
    }

    #[test]
    fn decode_address_ignores_extra_bytes() {
        let addr = Address::from([0xCC; 20]);
        let mut input = vec![0u8; 64]; // 64 bytes, only first 32 matter
        input[12..32].copy_from_slice(addr.as_bytes());

        let decoded = decode_address(&input).unwrap();
        assert_eq!(decoded, addr);
    }

    #[test]
    fn decode_address_all_zeros() {
        let input = [0u8; 32];
        let decoded = decode_address(&input).unwrap();
        assert_eq!(decoded, Address::ZERO);
    }

    #[test]
    fn system_contract_code_hash_is_deterministic() {
        let h1 = system_contract_code_hash();
        let h2 = system_contract_code_hash();
        assert_eq!(h1, h2);
        // Must not be the zero hash
        assert_ne!(h1, shell_primitives::ShellHash::ZERO);
    }

    #[test]
    fn registry_address_matches_constant() {
        let addr = registry_address();
        assert_eq!(addr.as_bytes(), &VALIDATOR_REGISTRY_ADDR);
    }

    // ── Gas accounting ─────────────────────────────────────────

    #[test]
    fn get_validators_charges_base_gas_only() {
        let v1 = Address::from([0x01; 20]);
        let mut ws = setup_with_validators(&[v1]);
        let calldata = GET_VALIDATORS_SELECTOR.to_vec();
        let (_, gas) = execute_system_contract(&Address::ZERO, &calldata, &mut ws).unwrap();
        assert_eq!(gas, SYSTEM_CALL_BASE_GAS);
    }

    #[test]
    fn is_validator_charges_base_gas_only() {
        let v1 = Address::from([0x01; 20]);
        let mut ws = setup_with_validators(&[v1]);
        let mut calldata = IS_VALIDATOR_SELECTOR.to_vec();
        let mut word = [0u8; 32];
        word[12..32].copy_from_slice(v1.as_bytes());
        calldata.extend_from_slice(&word);
        let (_, gas) = execute_system_contract(&Address::ZERO, &calldata, &mut ws).unwrap();
        assert_eq!(gas, SYSTEM_CALL_BASE_GAS);
    }

    #[test]
    fn mutating_ops_charge_base_plus_op_gas() {
        let v1 = Address::from([0x01; 20]);
        let v2 = Address::from([0x02; 20]);
        let mut ws = setup_with_validators(&[v1]);
        let expected = SYSTEM_CALL_BASE_GAS + SYSTEM_CALL_OP_GAS;

        let calldata = encode_add_validator_calldata(&v2);
        let (_, gas) = execute_system_contract(&v1, &calldata, &mut ws).unwrap();
        assert_eq!(gas, expected);

        let calldata = encode_remove_validator_calldata(&v2);
        let (_, gas) = execute_system_contract(&v1, &calldata, &mut ws).unwrap();
        assert_eq!(gas, expected);
    }
}

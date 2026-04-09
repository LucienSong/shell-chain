// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

interface IAccountValidator {
    function validateTransaction(
        bytes32 txHash,
        bytes calldata sig,
        bytes calldata pubkey
    ) external view returns (bytes1);
}

/// @title DefaultPQValidator
/// @notice Reference AA validator that mirrors Shell-Chain's built-in
/// Dilithium verification in contract form.
/// @dev Calls the Shell-Chain Dilithium precompile at 0x0100 using the native
/// length-prefixed wire format:
/// [4-byte pubkey length][pubkey][4-byte message length][32-byte tx hash][sig]
contract DefaultPQValidator is IAccountValidator {
    address internal constant PQ_DILITHIUM_VERIFY = address(0x0100);

    function validateTransaction(
        bytes32 txHash,
        bytes calldata sig,
        bytes calldata pubkey
    ) external view override returns (bytes1) {
        bytes memory input = abi.encodePacked(
            uint32(pubkey.length),
            pubkey,
            uint32(32),
            txHash,
            sig
        );

        (bool ok, bytes memory output) = PQ_DILITHIUM_VERIFY.staticcall(input);
        if (!ok || output.length < 32) {
            return 0x00;
        }

        return output[31] == 0x01 ? bytes1(0x01) : bytes1(0x00);
    }
}

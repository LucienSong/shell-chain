# Testnet Test Account Template

This template shows the expected structure of each test account keystore file.
Actual keystore files (`account-*.json`) are gitignored.

## Keystore Format (v1, Dilithium3)

```json
{
  "version": 1,
  "address": "0x<64-lowercase-hex>",
  "key_type": "dilithium3",
  "kdf": "argon2id",
  "kdf_params": {
    "m_cost": 65536,
    "t_cost": 3,
    "p_cost": 4,
    "salt": "<32-byte-hex>"
  },
  "cipher": "xchacha20-poly1305",
  "cipher_params": {
    "nonce": "<24-byte-hex>"
  },
  "ciphertext": "<hex>",
  "public_key": "<hex>"
}
```

> The `address` field uses the canonical Shell Chain format: `0x` + 64 lowercase hex chars.

See `docs/keystore-format.md` for the full specification.

## Genesis Alloc Entry

Each account is allocated in genesis as:

```json
{
  "alloc": {
    "0x<account-address-64-hex>": {
      "balance": "0xde0b6b3a7640000",
      "nonce": 0
    }
  }
}
```

All alloc map keys must be canonical `0x` + 64 lowercase hex addresses.

## Address Format

Addresses are stored in keystores and genesis files as `0x` + 64 lowercase hex.

To view the address from a keystore:
```bash
shell-node key inspect account-1.json
# Address: 0x...
```

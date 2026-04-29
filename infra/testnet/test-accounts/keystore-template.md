# Testnet Test Account Template

This template shows the expected structure of each test account keystore file.
Actual keystore files (`account-*.json`) are gitignored.

## Keystore Format (v1, Dilithium3)

```json
{
  "version": 1,
  "address": "0x<20-byte-hex>",
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

See `docs/keystore-format.md` for the full specification.

## Genesis Alloc Entry

Each account is allocated in genesis as:

```json
{
  "alloc": {
    "<20-byte-hex-address>": {
      "balance": "1000000000000000000"
    }
  }
}
```

## Address Format

Addresses are stored in keystores as `0x`-prefixed hex (lowercase).
The CLI displays them in bech32 (`pq1...`) form.

To convert:
```bash
shell-node key inspect account-1.json
# Address: pq1...
```

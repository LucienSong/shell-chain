# Testnet Archive Manifest

> Last updated: 2026-04-29

## Testnet Reset History

### Reset 1 — 2026-04-28 (Pre-ML-DSA-65 → ML-DSA-65 migration)

**Reason**: F-TESTNET-FIXES — unified keystore format (sk-only), ML-DSA-65 as
independent FIPS 204 algorithm (not Dilithium3 alias), SIG_IDS bugfix.

**Archive location**: `/opt/shell/data/archive/` on server `i-bp10tmk5vjoo9nam52zo`

| Item | Path on server | Notes |
|------|---------------|-------|
| Old chain DB backup | `/opt/shell/data/archive/validator-backup-20260428-113122/` | RocksDB checkpoint, ~size unknown |
| Old validator keystore | `/opt/shell/keystore/archive/validator-pre-reset-20260428.json` | Dilithium3, keep for reference |

**State at reset:**
- Last block: ~height N/A (db wiped)
- Chain ID: 10
- Consensus: PoA single validator
- Validator algo: Dilithium3 → **migrated to ML-DSA-65**

**New chain state (post-reset):**
- Genesis timestamp: 2026-04-28
- Validator: `pq1q92dxh9a243vlgampz4cxscrg7750rmzautc48ja` (ML-DSA-65)
- 10 test accounts (Dilithium3, 1 SHELL each)
- Chain ID: 10

## Archived Files (Committed)

The keystore files and DB snapshots are too large to commit to git. They are archived
on the server and referenced here.

To restore an old DB:
```bash
shell-node backup restore /opt/shell/data/archive/validator-backup-20260428-113122
```

## RPC Archive

Historical RPC data is not archived. If needed, use `shell-node export-state` before
a future reset to create a snapshot file.

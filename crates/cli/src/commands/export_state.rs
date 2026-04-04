//! `shell-node export-state` — export chain state to a snapshot file.

use std::path::PathBuf;

/// Export chain state at a given block to a snapshot file.
pub fn export_state(
    datadir: PathBuf,
    output: PathBuf,
    block: Option<u64>,
) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(feature = "rocksdb")]
    {
        use std::sync::Arc;
        use shell_storage::{ChainStore, RocksDbStore, SnapshotMetadata};

        let db_path = datadir.join("db");
        if !db_path.exists() {
            return Err(format!(
                "Database not found at {}. Run `shell-node init` first.",
                db_path.display()
            )
            .into());
        }

        let stores = RocksDbStore::open_all(&db_path, None)?;
        let store = Arc::new(stores.state);
        let chain_store = ChainStore::new(store);

        // Resolve block number: use provided value or latest head block.
        let target_block = match block {
            Some(n) => {
                let blk = chain_store
                    .get_block_by_number(n)?
                    .ok_or_else(|| format!("Block #{n} not found in chain store"))?;
                blk
            }
            None => chain_store
                .get_head_block()?
                .ok_or("No head block found. Is the chain initialized?")?,
        };

        let metadata = SnapshotMetadata::new(
            chain_store
                .get_chain_config()?
                .map(|c| c.chain_id)
                .unwrap_or(0),
            target_block.number(),
            target_block.hash(),
            target_block.header.state_root,
            chain_store
                .get_chain_config()?
                .map(|c| c.genesis_hash)
                .unwrap_or_default(),
        );

        let file = std::fs::File::create(&output)?;
        let writer = std::io::BufWriter::new(file);
        let final_meta = chain_store.export_snapshot(metadata, writer)?;

        let file_size = std::fs::metadata(&output)?.len();
        eprintln!("✓ State exported successfully");
        eprintln!("  Block:   #{}", final_meta.block_number);
        eprintln!("  Entries: {}", final_meta.entry_count);
        eprintln!("  File:    {} ({} bytes)", output.display(), file_size);

        Ok(())
    }
    #[cfg(not(feature = "rocksdb"))]
    {
        let _ = (datadir, output, block);
        Err("RocksDB support not compiled. Rebuild with: cargo build -p shell-cli --features rocksdb".into())
    }
}

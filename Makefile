.PHONY: bench bench-quick test

# Run full criterion benchmarks for all workspace crates
bench:
	cargo bench --workspace

# Quick compile-check for benchmarks (no actual measurement)
bench-quick:
	cargo bench --workspace -- --test

# Run all workspace tests
test:
	cargo test --workspace --tests

/// Default initial base fee (1 gwei) used for the first block after genesis.
pub const INITIAL_BASE_FEE: u64 = 1_000_000_000;

/// EIP-1559 base fee elasticity denominator (max ±12.5% change per block).
const BASE_FEE_CHANGE_DENOMINATOR: u64 = 8;

/// Calculate the effective gas price paid by the transaction.
/// EIP-1559: effective_price = min(max_fee, base_fee + max_priority_fee)
pub fn effective_gas_price(max_fee: u64, max_priority_fee: u64, base_fee: u64) -> u64 {
    max_fee.min(base_fee.saturating_add(max_priority_fee))
}

/// Calculate the miner tip (priority fee actually paid).
/// tip = effective_price - base_fee
pub fn miner_tip(max_fee: u64, max_priority_fee: u64, base_fee: u64) -> u64 {
    effective_gas_price(max_fee, max_priority_fee, base_fee).saturating_sub(base_fee)
}

/// Calculate EIP-1559 base fee for the next block.
///
/// - If parent used more gas than target (50% of limit), base fee increases
/// - If parent used less gas than target, base fee decreases
/// - Minimum base fee is 1 (never 0 after genesis)
/// - Maximum change per block: ±12.5% (1/8)
///
/// Special case: if `parent_base_fee` is 0 (genesis block), returns
/// [`INITIAL_BASE_FEE`].
pub fn calculate_base_fee(
    parent_gas_used: u64,
    parent_gas_limit: u64,
    parent_base_fee: u64,
) -> u64 {
    // Genesis parent → bootstrap with initial base fee.
    if parent_base_fee == 0 {
        return INITIAL_BASE_FEE;
    }

    let gas_target = parent_gas_limit / 2;
    if gas_target == 0 {
        return parent_base_fee;
    }

    if parent_gas_used == gas_target {
        parent_base_fee
    } else if parent_gas_used > gas_target {
        let delta = parent_base_fee
            .saturating_mul(parent_gas_used - gas_target)
            / gas_target
            / BASE_FEE_CHANGE_DENOMINATOR;
        parent_base_fee.saturating_add(delta.max(1))
    } else {
        let delta = parent_base_fee
            .saturating_mul(gas_target - parent_gas_used)
            / gas_target
            / BASE_FEE_CHANGE_DENOMINATOR;
        (parent_base_fee.saturating_sub(delta)).max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GAS_LIMIT: u64 = 30_000_000;
    const GAS_TARGET: u64 = GAS_LIMIT / 2; // 15_000_000

    #[test]
    fn genesis_returns_initial_base_fee() {
        assert_eq!(
            calculate_base_fee(0, GAS_LIMIT, 0),
            INITIAL_BASE_FEE,
        );
    }

    #[test]
    fn exact_target_unchanged() {
        let base = 1_000_000_000u64;
        assert_eq!(calculate_base_fee(GAS_TARGET, GAS_LIMIT, base), base);
    }

    #[test]
    fn full_block_increases_fee() {
        let base = 1_000_000_000u64;
        let new = calculate_base_fee(GAS_LIMIT, GAS_LIMIT, base);
        assert!(new > base, "base fee should increase when block is full");
        // At 100% full (used == limit), delta = base * target / target / 8 = base / 8
        assert_eq!(new, base + base / 8);
    }

    #[test]
    fn empty_block_decreases_fee() {
        let base = 1_000_000_000u64;
        let new = calculate_base_fee(0, GAS_LIMIT, base);
        assert!(new < base, "base fee should decrease when block is empty");
        // At 0% usage, delta = base * target / target / 8 = base / 8
        assert_eq!(new, base - base / 8);
    }

    #[test]
    fn minimum_base_fee_is_one() {
        // Even with zero usage and a very low fee, should never go below 1
        let new = calculate_base_fee(0, GAS_LIMIT, 1);
        assert_eq!(new, 1, "base fee must never drop below 1");
    }

    #[test]
    fn increase_at_least_one() {
        // With a very small base fee, ensure delta is at least 1
        let base = 8u64; // delta would be 8 * 1 / 8 = 1 without max(delta,1)
        let new = calculate_base_fee(GAS_TARGET + 1, GAS_LIMIT, base);
        assert!(new > base, "fee should increase even with tiny base");
    }

    #[test]
    fn maximum_increase_is_12_5_percent() {
        let base = 1_000_000_000u64;
        // Block 100% full → maximum increase
        let new = calculate_base_fee(GAS_LIMIT, GAS_LIMIT, base);
        let max_increase = base / 8;
        assert_eq!(new - base, max_increase);
    }

    #[test]
    fn maximum_decrease_is_12_5_percent() {
        let base = 1_000_000_000u64;
        // Block completely empty → maximum decrease
        let new = calculate_base_fee(0, GAS_LIMIT, base);
        let max_decrease = base / 8;
        assert_eq!(base - new, max_decrease);
    }

    #[test]
    fn consecutive_full_blocks_keep_increasing() {
        let mut base = INITIAL_BASE_FEE;
        for _ in 0..10 {
            let next = calculate_base_fee(GAS_LIMIT, GAS_LIMIT, base);
            assert!(next > base);
            base = next;
        }
    }

    #[test]
    fn consecutive_empty_blocks_keep_decreasing() {
        let mut base = INITIAL_BASE_FEE;
        for _ in 0..200 {
            let next = calculate_base_fee(0, GAS_LIMIT, base);
            assert!(next <= base);
            base = next;
        }
        // Should converge to a small value (≥ 1)
        assert!(base >= 1 && base <= 8, "converged to {base}");
    }

    #[test]
    fn half_full_block_unchanged() {
        let base = 500_000_000u64;
        assert_eq!(calculate_base_fee(GAS_TARGET, GAS_LIMIT, base), base);
    }

    #[test]
    fn slightly_over_target_increases() {
        let base = 1_000_000_000u64;
        let over = GAS_TARGET + 1_000_000;
        let new = calculate_base_fee(over, GAS_LIMIT, base);
        assert!(new > base);
    }

    #[test]
    fn slightly_under_target_decreases() {
        let base = 1_000_000_000u64;
        let under = GAS_TARGET - 1_000_000;
        let new = calculate_base_fee(under, GAS_LIMIT, base);
        assert!(new < base);
    }

    #[test]
    fn saturating_add_prevents_overflow() {
        // With a very high base fee, increase must not overflow.
        let base = u64::MAX - 1_000_000_000;
        let new = calculate_base_fee(GAS_LIMIT, GAS_LIMIT, base);
        assert!(new >= base, "fee should not wrap around");
        assert!(new <= u64::MAX, "fee should cap at u64::MAX");
    }

    // ── effective_gas_price tests ──────────────────────────────

    #[test]
    fn effective_price_capped_by_max_fee() {
        // max_fee < base_fee + priority → effective = max_fee
        assert_eq!(effective_gas_price(10, 5, 8), 10);
    }

    #[test]
    fn effective_price_capped_by_sum() {
        // base_fee + priority < max_fee → effective = base_fee + priority
        assert_eq!(effective_gas_price(20, 3, 10), 13);
    }

    #[test]
    fn effective_price_exact_match() {
        // max_fee == base_fee + priority
        assert_eq!(effective_gas_price(15, 5, 10), 15);
    }

    #[test]
    fn effective_price_zero_priority() {
        assert_eq!(effective_gas_price(10, 0, 8), 8);
    }

    #[test]
    fn effective_price_zero_base_fee() {
        assert_eq!(effective_gas_price(10, 3, 0), 3);
    }

    #[test]
    fn effective_price_saturates_on_overflow() {
        // base_fee + priority overflows u64 → min(max_fee, u64::MAX)
        assert_eq!(effective_gas_price(100, u64::MAX, u64::MAX), 100);
    }

    // ── miner_tip tests ───────────────────────────────────────

    #[test]
    fn tip_from_priority_fee() {
        // effective = min(20, 10+3) = 13; tip = 13 - 10 = 3
        assert_eq!(miner_tip(20, 3, 10), 3);
    }

    #[test]
    fn tip_capped_by_max_fee() {
        // effective = min(10, 8+5) = 10; tip = 10 - 8 = 2
        assert_eq!(miner_tip(10, 5, 8), 2);
    }

    #[test]
    fn tip_zero_when_no_priority() {
        assert_eq!(miner_tip(10, 0, 8), 0);
    }

    #[test]
    fn tip_zero_when_max_fee_equals_base() {
        assert_eq!(miner_tip(10, 5, 10), 0);
    }
}

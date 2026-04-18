//! K3: ProverHealth monitor and graceful degradation.
//!
//! `ProverHealth` tracks the operational health of the local prover service:
//! - Backlog queue depth (how far behind the prover is).
//! - Recent proof failure rate.
//! - Time since last successful proof.
//!
//! Based on these metrics, it reports a `HealthStatus`:
//! - `Healthy`: Prover is keeping up; backlog ≤ `warn_backlog_depth`.
//! - `Degraded`: Backlog growing or failure rate elevated; emit warnings.
//! - `Overloaded`: Backlog exceeds `overload_backlog_depth`; shed new tasks.
//! - `Failing`: Recent failure rate exceeds `max_failure_rate`; alert and stop accepting.
//!
//! The node can use `HealthStatus` to decide whether to claim new proof windows
//! (I4) or temporarily yield to other provers.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Configuration for prover health monitoring.
#[derive(Debug, Clone)]
pub struct ProverHealthConfig {
    /// Backlog depth at which status becomes `Degraded`. Default: 10.
    pub warn_backlog_depth: usize,
    /// Backlog depth at which status becomes `Overloaded`. Default: 50.
    pub overload_backlog_depth: usize,
    /// Rolling window for failure rate calculation. Default: 20 proofs.
    pub failure_window: usize,
    /// Fraction of recent proofs that may fail before status is `Failing`. Default: 0.5.
    pub max_failure_rate: f64,
    /// Duration since last successful proof after which status is `Degraded`. Default: 60s.
    pub stale_after: Duration,
}

impl Default for ProverHealthConfig {
    fn default() -> Self {
        Self {
            warn_backlog_depth: 10,
            overload_backlog_depth: 50,
            failure_window: 20,
            max_failure_rate: 0.5,
            stale_after: Duration::from_secs(60),
        }
    }
}

/// Operational health of the local prover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthStatus {
    /// Prover is keeping up normally.
    Healthy,
    /// Backlog growing or failure rate elevated; continue but emit warnings.
    Degraded,
    /// Backlog very deep; shed new proof tasks until it drains.
    Overloaded,
    /// Failure rate too high; stop accepting new work and alert.
    Failing,
}

impl std::fmt::Display for HealthStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Healthy => write!(f, "healthy"),
            Self::Degraded => write!(f, "degraded"),
            Self::Overloaded => write!(f, "overloaded"),
            Self::Failing => write!(f, "failing"),
        }
    }
}

/// K3: Prover health monitor with graceful degradation support.
#[derive(Debug)]
pub struct ProverHealth {
    config: ProverHealthConfig,
    /// Rolling window: `true` = success, `false` = failure.
    recent_results: VecDeque<bool>,
    last_success: Option<Instant>,
    total_proofs: u64,
    total_failures: u64,
}

impl ProverHealth {
    pub fn new(config: ProverHealthConfig) -> Self {
        Self {
            config,
            recent_results: VecDeque::new(),
            last_success: None,
            total_proofs: 0,
            total_failures: 0,
        }
    }

    /// Record a successful proof.
    pub fn record_success(&mut self) {
        self.total_proofs += 1;
        self.last_success = Some(Instant::now());
        self.push_result(true);
    }

    /// Record a proof failure.
    pub fn record_failure(&mut self) {
        self.total_proofs += 1;
        self.total_failures += 1;
        self.push_result(false);
    }

    fn push_result(&mut self, success: bool) {
        if self.recent_results.len() >= self.config.failure_window {
            self.recent_results.pop_front();
        }
        self.recent_results.push_back(success);
    }

    /// Compute current health status given `backlog_depth`.
    pub fn status(&self, backlog_depth: usize) -> HealthStatus {
        // Check failure rate first (highest severity).
        if self.recent_results.len() >= self.config.failure_window / 2 {
            let failures = self.recent_results.iter().filter(|&&s| !s).count();
            let rate = failures as f64 / self.recent_results.len() as f64;
            if rate >= self.config.max_failure_rate {
                return HealthStatus::Failing;
            }
        }

        // Check backlog depth.
        if backlog_depth >= self.config.overload_backlog_depth {
            return HealthStatus::Overloaded;
        }
        if backlog_depth >= self.config.warn_backlog_depth {
            return HealthStatus::Degraded;
        }

        // Check staleness.
        if let Some(last) = self.last_success {
            if last.elapsed() > self.config.stale_after && self.total_proofs > 0 {
                return HealthStatus::Degraded;
            }
        } else if self.total_proofs > 0 {
            // Had proofs but no success recorded → degraded.
            return HealthStatus::Degraded;
        }

        HealthStatus::Healthy
    }

    /// Whether the prover should accept new tasks given current health.
    pub fn should_accept_work(&self, backlog_depth: usize) -> bool {
        !matches!(
            self.status(backlog_depth),
            HealthStatus::Overloaded | HealthStatus::Failing
        )
    }

    /// Total successful proofs.
    pub fn total_proofs(&self) -> u64 {
        self.total_proofs
    }

    /// Total failed proofs.
    pub fn total_failures(&self) -> u64 {
        self.total_failures
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn health() -> ProverHealth {
        ProverHealth::new(ProverHealthConfig {
            warn_backlog_depth: 5,
            overload_backlog_depth: 20,
            failure_window: 10,
            max_failure_rate: 0.5,
            stale_after: Duration::from_secs(3600),
        })
    }

    #[test]
    fn healthy_with_no_proofs_and_empty_backlog() {
        let h = health();
        assert_eq!(h.status(0), HealthStatus::Healthy);
    }

    #[test]
    fn degraded_when_backlog_above_warn() {
        let h = health();
        assert_eq!(h.status(5), HealthStatus::Degraded);
    }

    #[test]
    fn overloaded_when_backlog_above_overload() {
        let h = health();
        assert_eq!(h.status(20), HealthStatus::Overloaded);
    }

    #[test]
    fn healthy_after_successes() {
        let mut h = health();
        for _ in 0..5 {
            h.record_success();
        }
        assert_eq!(h.status(0), HealthStatus::Healthy);
    }

    #[test]
    fn failing_when_failure_rate_exceeded() {
        let mut h = health();
        // 5 failures out of 10 = 50% rate = exactly at threshold → Failing.
        for _ in 0..5 {
            h.record_success();
        }
        for _ in 0..5 {
            h.record_failure();
        }
        assert_eq!(h.status(0), HealthStatus::Failing);
    }

    #[test]
    fn should_accept_work_when_healthy() {
        let h = health();
        assert!(h.should_accept_work(0));
    }

    #[test]
    fn should_not_accept_work_when_overloaded() {
        let h = health();
        assert!(!h.should_accept_work(20));
    }

    #[test]
    fn total_counts_tracked() {
        let mut h = health();
        h.record_success();
        h.record_success();
        h.record_failure();
        assert_eq!(h.total_proofs(), 3);
        assert_eq!(h.total_failures(), 1);
    }
}

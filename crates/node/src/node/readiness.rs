use std::time::{Duration, Instant};

use tracing::{debug, warn};

/// Coarse block-production readiness used to keep restarting validators from
/// proposing on a stale local head while sync is still in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProductionReadinessState {
    Starting,
    CatchingUp,
    Ready,
    Degraded,
}

#[derive(Debug, Clone)]
pub(crate) struct ProductionReadiness {
    state: ProductionReadinessState,
    allow_isolated_production: bool,
    deadline: Option<Instant>,
    last_head: u64,
    last_reason: &'static str,
}

impl ProductionReadiness {
    pub(crate) fn new(
        allow_isolated_production: bool,
        peer_count: usize,
        head: u64,
        now: Instant,
        startup_grace: Duration,
    ) -> Self {
        let (state, deadline, reason) = if peer_count == 0 {
            if allow_isolated_production {
                (ProductionReadinessState::Ready, None, "isolated-dev")
            } else {
                (ProductionReadinessState::Degraded, None, "no-peers")
            }
        } else {
            (
                ProductionReadinessState::Starting,
                Some(now + startup_grace),
                "startup-sync",
            )
        };

        Self {
            state,
            allow_isolated_production,
            deadline,
            last_head: head,
            last_reason: reason,
        }
    }

    pub(crate) fn state(&self) -> ProductionReadinessState {
        self.state
    }

    pub(crate) fn reason(&self) -> &'static str {
        self.last_reason
    }

    pub(crate) fn note_sync_requested(
        &mut self,
        head: u64,
        now: Instant,
        timeout: Duration,
        reason: &'static str,
    ) {
        self.last_head = head;
        self.state = ProductionReadinessState::CatchingUp;
        self.deadline = Some(now + timeout);
        self.last_reason = reason;
        debug!(head, reason, "block production paused for sync");
    }

    pub(crate) fn note_head_probe(
        &mut self,
        head: u64,
        now: Instant,
        grace: Duration,
        reason: &'static str,
    ) {
        if self.state == ProductionReadinessState::CatchingUp {
            debug!(
                head,
                reason, "head probe ignored while active catch-up is in progress"
            );
            return;
        }

        self.last_head = head;
        self.state = ProductionReadinessState::Starting;
        self.deadline = Some(now + grace);
        self.last_reason = reason;
        debug!(
            head,
            reason, "block production briefly paused for head probe"
        );
    }

    pub(crate) fn note_import_progress(&mut self, head: u64) {
        self.last_head = head;
        if self.state == ProductionReadinessState::CatchingUp {
            self.last_reason = "catch-up-progress";
            return;
        }
        self.state = ProductionReadinessState::Ready;
        self.deadline = None;
        self.last_reason = "import-progress";
    }

    pub(crate) fn note_sync_idle(&mut self) {
        self.state = ProductionReadinessState::Ready;
        self.deadline = None;
        self.last_reason = "sync-idle";
    }

    pub(crate) fn refresh(
        &mut self,
        peer_count: usize,
        sync_requested: bool,
        head: u64,
        now: Instant,
    ) {
        self.last_head = head;

        if peer_count == 0 {
            if self.allow_isolated_production {
                self.state = ProductionReadinessState::Ready;
                self.deadline = None;
                self.last_reason = "isolated-dev";
            } else {
                self.state = ProductionReadinessState::Degraded;
                self.deadline = None;
                self.last_reason = "no-peers";
            }
            return;
        }

        match self.state {
            ProductionReadinessState::Starting => {
                if !sync_requested {
                    self.note_sync_idle();
                } else if self.deadline.is_some_and(|deadline| now >= deadline) {
                    warn!(
                        head,
                        reason = self.last_reason,
                        "startup sync grace elapsed while sync is still pending; production remains disabled"
                    );
                    self.state = ProductionReadinessState::Degraded;
                    self.deadline = None;
                    self.last_reason = "startup-sync-timeout";
                }
            }
            ProductionReadinessState::CatchingUp => {
                if !sync_requested {
                    self.note_sync_idle();
                } else if self.deadline.is_some_and(|deadline| now >= deadline) {
                    warn!(
                        head,
                        reason = self.last_reason,
                        "sync did not complete before timeout; production remains disabled"
                    );
                    self.state = ProductionReadinessState::Degraded;
                    self.deadline = None;
                    self.last_reason = "sync-timeout";
                }
            }
            ProductionReadinessState::Degraded => {
                if !sync_requested {
                    self.note_sync_idle();
                }
            }
            ProductionReadinessState::Ready => {}
        }
    }

    pub(crate) fn can_produce(&self) -> bool {
        self.state == ProductionReadinessState::Ready
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_isolated_node_can_produce() {
        let now = Instant::now();
        let readiness = ProductionReadiness::new(true, 0, 0, now, Duration::from_secs(5));

        assert_eq!(readiness.state(), ProductionReadinessState::Ready);
        assert!(readiness.can_produce());
        assert_eq!(readiness.reason(), "isolated-dev");
    }

    #[test]
    fn testnet_without_peers_is_degraded() {
        let now = Instant::now();
        let readiness = ProductionReadiness::new(false, 0, 10, now, Duration::from_secs(5));

        assert_eq!(readiness.state(), ProductionReadinessState::Degraded);
        assert!(!readiness.can_produce());
        assert_eq!(readiness.reason(), "no-peers");
    }

    #[test]
    fn startup_sync_blocks_production_until_sync_finishes() {
        let now = Instant::now();
        let mut readiness = ProductionReadiness::new(false, 2, 7, now, Duration::from_secs(5));

        readiness.refresh(2, true, 7, now + Duration::from_secs(1));
        assert_eq!(readiness.state(), ProductionReadinessState::Starting);
        assert!(!readiness.can_produce());

        readiness.refresh(2, true, 7, now + Duration::from_secs(6));
        assert_eq!(readiness.state(), ProductionReadinessState::Degraded);
        assert!(!readiness.can_produce());
        assert_eq!(readiness.reason(), "startup-sync-timeout");

        readiness.refresh(2, false, 7, now + Duration::from_secs(7));
        assert_eq!(readiness.state(), ProductionReadinessState::Ready);
        assert!(readiness.can_produce());
        assert_eq!(readiness.reason(), "sync-idle");
    }

    #[test]
    fn gap_sync_timeout_moves_to_degraded() {
        let now = Instant::now();
        let mut readiness = ProductionReadiness::new(false, 2, 10, now, Duration::from_secs(5));
        readiness.note_import_progress(10);
        readiness.note_sync_requested(10, now, Duration::from_secs(3), "gap-detected");

        readiness.refresh(2, true, 10, now + Duration::from_secs(4));
        assert_eq!(readiness.state(), ProductionReadinessState::Degraded);
        assert!(!readiness.can_produce());
        assert_eq!(readiness.reason(), "sync-timeout");
    }

    #[test]
    fn degraded_sync_recovers_only_after_sync_request_clears() {
        let now = Instant::now();
        let mut readiness = ProductionReadiness::new(false, 2, 10, now, Duration::from_secs(5));
        readiness.note_import_progress(10);
        readiness.note_sync_requested(10, now, Duration::from_secs(3), "gap-detected");

        readiness.refresh(2, true, 10, now + Duration::from_secs(4));
        assert_eq!(readiness.state(), ProductionReadinessState::Degraded);
        assert!(!readiness.can_produce());

        readiness.refresh(2, true, 10, now + Duration::from_secs(35));
        assert_eq!(readiness.state(), ProductionReadinessState::Degraded);
        assert!(!readiness.can_produce());

        readiness.refresh(2, false, 10, now + Duration::from_secs(36));
        assert_eq!(readiness.state(), ProductionReadinessState::Ready);
        assert!(readiness.can_produce());
        assert_eq!(readiness.reason(), "sync-idle");
    }

    #[test]
    fn import_progress_restores_readiness() {
        let now = Instant::now();
        let mut readiness = ProductionReadiness::new(false, 1, 3, now, Duration::from_secs(5));
        readiness.note_head_probe(3, now, Duration::from_secs(30), "peer-connected");

        readiness.note_import_progress(4);
        assert_eq!(readiness.state(), ProductionReadinessState::Ready);
        assert!(readiness.can_produce());
        assert_eq!(readiness.reason(), "import-progress");
    }

    #[test]
    fn import_progress_preserves_active_catchup() {
        let now = Instant::now();
        let mut readiness = ProductionReadiness::new(false, 1, 11, now, Duration::from_secs(5));
        readiness.note_sync_requested(11, now, Duration::from_secs(30), "gap-detected");

        readiness.note_import_progress(12);
        assert_eq!(readiness.state(), ProductionReadinessState::CatchingUp);
        assert!(!readiness.can_produce());
        assert_eq!(readiness.reason(), "catch-up-progress");
    }

    #[test]
    fn head_probe_does_not_downgrade_active_catchup() {
        let now = Instant::now();
        let mut readiness = ProductionReadiness::new(false, 2, 9, now, Duration::from_secs(5));

        readiness.note_sync_requested(9, now, Duration::from_secs(30), "gap-detected");
        readiness.note_head_probe(
            10,
            now + Duration::from_secs(1),
            Duration::from_secs(2),
            "block-response-next-batch",
        );
        readiness.refresh(2, true, 10, now + Duration::from_secs(4));

        assert_eq!(readiness.state(), ProductionReadinessState::CatchingUp);
        assert!(!readiness.can_produce());
        assert_eq!(readiness.reason(), "gap-detected");
    }

    #[test]
    fn head_probe_keeps_production_paused_until_peer_response() {
        let now = Instant::now();
        let mut readiness = ProductionReadiness::new(false, 1, 5, now, Duration::from_secs(5));
        readiness.note_import_progress(5);
        readiness.note_head_probe(5, now, Duration::from_secs(2), "next-block-probe");

        readiness.refresh(1, true, 5, now + Duration::from_secs(3));
        assert_eq!(readiness.state(), ProductionReadinessState::Degraded);
        assert!(!readiness.can_produce());

        readiness.refresh(1, false, 5, now + Duration::from_secs(4));
        assert_eq!(readiness.state(), ProductionReadinessState::Ready);
        assert!(readiness.can_produce());
    }
}

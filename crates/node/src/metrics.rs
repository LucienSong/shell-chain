//! Prometheus metrics collection and HTTP endpoint for shell-chain.
//!
//! Exposes `/metrics` (Prometheus text format) and `/health` (JSON)
//! via a lightweight hyper HTTP server.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use prometheus::{
    Encoder, Histogram, HistogramOpts, IntCounter, IntGauge, Opts, Registry, TextEncoder,
};

/// Prometheus metrics for a shell-chain node.
pub struct Metrics {
    /// Current block height.
    pub block_height: IntGauge,
    /// Number of connected peers.
    pub peer_count: IntGauge,
    /// Number of pending transactions in the mempool.
    pub tx_pool_size: IntGauge,
    /// Block production latency in seconds.
    pub block_production_ms: Histogram,
    /// Total number of blocks imported.
    pub blocks_imported: IntCounter,
    /// Total number of transactions received.
    pub txs_received: IntCounter,
    /// Timestamp when the node started, used for uptime calculation.
    pub uptime_start: Instant,
    registry: Registry,
}

impl Metrics {
    /// Create a new `Metrics` instance with all gauges, counters and histograms
    /// registered against a fresh [`Registry`].
    ///
    /// Returns an error if metric registration fails (e.g. duplicate names).
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let registry = Registry::new();

        let block_height =
            IntGauge::with_opts(Opts::new("shell_block_height", "Current block height"))?;
        let peer_count =
            IntGauge::with_opts(Opts::new("shell_peer_count", "Number of connected peers"))?;
        let tx_pool_size = IntGauge::with_opts(Opts::new(
            "shell_tx_pool_size",
            "Number of pending transactions",
        ))?;
        let block_production_ms = Histogram::with_opts(
            HistogramOpts::new(
                "shell_block_production_duration_seconds",
                "Block production latency",
            )
            .buckets(vec![0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0]),
        )?;
        let blocks_imported = IntCounter::with_opts(Opts::new(
            "shell_blocks_imported_total",
            "Total blocks imported",
        ))?;
        let txs_received = IntCounter::with_opts(Opts::new(
            "shell_txs_received_total",
            "Total transactions received",
        ))?;

        registry.register(Box::new(block_height.clone()))?;
        registry.register(Box::new(peer_count.clone()))?;
        registry.register(Box::new(tx_pool_size.clone()))?;
        registry.register(Box::new(block_production_ms.clone()))?;
        registry.register(Box::new(blocks_imported.clone()))?;
        registry.register(Box::new(txs_received.clone()))?;

        Ok(Self {
            block_height,
            peer_count,
            tx_pool_size,
            block_production_ms,
            blocks_imported,
            txs_received,
            uptime_start: Instant::now(),
            registry,
        })
    }

    /// Encode all collected metrics into Prometheus text exposition format.
    pub fn gather(&self) -> String {
        let encoder = TextEncoder::new();
        let metric_families = self.registry.gather();
        let mut buffer = Vec::new();
        if let Err(e) = encoder.encode(&metric_families, &mut buffer) {
            tracing::error!(error = %e, "failed to encode Prometheus metrics");
            return String::new();
        }
        String::from_utf8(buffer).unwrap_or_default()
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new().expect("failed to register Prometheus metrics")
    }
}

/// Determine health status based on current metrics.
fn health_status(metrics: &Metrics) -> &'static str {
    if metrics.peer_count.get() == 0 {
        "degraded"
    } else {
        "healthy"
    }
}

/// Handle a single HTTP request, routing to `/metrics`, `/health`, or `/readiness`.
fn handle_request(
    req: Request<Incoming>,
    metrics: &Arc<Metrics>,
) -> Response<http_body_util::Full<hyper::body::Bytes>> {
    match (req.method(), req.uri().path()) {
        (&Method::GET, "/metrics") => {
            let body = metrics.gather();
            Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "text/plain; version=0.0.4; charset=utf-8")
                .body(http_body_util::Full::new(hyper::body::Bytes::from(body)))
                .unwrap()
        }
        (&Method::GET, "/health") => {
            let status = health_status(metrics);
            let body = serde_json::json!({
                "status": status,
                "block_height": metrics.block_height.get(),
                "peer_count": metrics.peer_count.get(),
                "tx_pool_size": metrics.tx_pool_size.get(),
                "blocks_imported": metrics.blocks_imported.get(),
                "uptime_seconds": metrics.uptime_start.elapsed().as_secs(),
            });
            Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "application/json")
                .body(http_body_util::Full::new(hyper::body::Bytes::from(
                    body.to_string(),
                )))
                .unwrap()
        }
        (&Method::GET, "/readiness") => {
            let ready = metrics.peer_count.get() > 0 && metrics.block_height.get() > 0;
            let (status_code, status_text) = if ready {
                (StatusCode::OK, "ready")
            } else {
                (StatusCode::SERVICE_UNAVAILABLE, "not ready")
            };
            let body = serde_json::json!({ "status": status_text });
            Response::builder()
                .status(status_code)
                .header("Content-Type", "application/json")
                .body(http_body_util::Full::new(hyper::body::Bytes::from(
                    body.to_string(),
                )))
                .unwrap()
        }
        _ => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(http_body_util::Full::new(hyper::body::Bytes::from(
                "Not Found",
            )))
            .unwrap(),
    }
}

/// Start an HTTP server that exposes Prometheus metrics and a health endpoint.
///
/// The server runs until the process exits or the task is cancelled.
pub async fn serve_metrics(metrics: Arc<Metrics>, addr: SocketAddr) {
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(%addr, error = %e, "failed to bind metrics server");
            return;
        }
    };
    tracing::info!(%addr, "metrics server listening");

    loop {
        let (stream, _) = match listener.accept().await {
            Ok(conn) => conn,
            Err(e) => {
                tracing::warn!(error = %e, "metrics server accept error");
                continue;
            }
        };

        let metrics = Arc::clone(&metrics);
        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            let service = hyper::service::service_fn(move |req| {
                let metrics = Arc::clone(&metrics);
                async move { Ok::<_, std::convert::Infallible>(handle_request(req, &metrics)) }
            });
            if let Err(e) = hyper::server::conn::http1::Builder::new()
                .serve_connection(io, service)
                .await
            {
                tracing::debug!(error = %e, "metrics connection error");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build a health JSON body from the given metrics.
    fn build_health_body(metrics: &Metrics) -> serde_json::Value {
        let status = health_status(metrics);
        serde_json::json!({
            "status": status,
            "block_height": metrics.block_height.get(),
            "peer_count": metrics.peer_count.get(),
            "tx_pool_size": metrics.tx_pool_size.get(),
            "blocks_imported": metrics.blocks_imported.get(),
            "uptime_seconds": metrics.uptime_start.elapsed().as_secs(),
        })
    }

    #[test]
    fn metrics_new_creates_valid_instance() {
        let m = Metrics::new().expect("metrics init");
        assert_eq!(m.block_height.get(), 0);
        assert_eq!(m.peer_count.get(), 0);
        assert_eq!(m.tx_pool_size.get(), 0);
        assert_eq!(m.blocks_imported.get(), 0);
        assert_eq!(m.txs_received.get(), 0);
    }

    #[test]
    fn gather_returns_prometheus_text_format() {
        let m = Metrics::new().expect("metrics init");
        m.block_height.set(42);
        m.blocks_imported.inc();

        let output = m.gather();
        assert!(
            output.contains("shell_block_height 42"),
            "should contain block_height metric"
        );
        assert!(
            output.contains("shell_blocks_imported_total 1"),
            "should contain blocks_imported metric"
        );
        assert!(
            output.contains("shell_peer_count"),
            "should contain peer_count metric"
        );
        assert!(
            output.contains("shell_tx_pool_size"),
            "should contain tx_pool_size metric"
        );
        assert!(
            output.contains("shell_block_production_duration_seconds"),
            "should contain block_production_duration_seconds metric"
        );
        assert!(
            output.contains("shell_txs_received_total"),
            "should contain txs_received metric"
        );
    }

    #[test]
    fn health_body_includes_all_fields() {
        let m = Metrics::new().expect("metrics init");
        m.block_height.set(12345);
        m.peer_count.set(3);
        m.tx_pool_size.set(42);
        for _ in 0..500 {
            m.blocks_imported.inc();
        }

        let body = build_health_body(&m);
        assert_eq!(body["status"], "healthy");
        assert_eq!(body["block_height"], 12345);
        assert_eq!(body["peer_count"], 3);
        assert_eq!(body["tx_pool_size"], 42);
        assert_eq!(body["blocks_imported"], 500);
        assert!(
            body["uptime_seconds"].is_u64(),
            "uptime_seconds should be a number"
        );
    }

    #[test]
    fn health_status_healthy_when_peers_connected() {
        let m = Metrics::new().expect("metrics init");
        m.peer_count.set(5);
        assert_eq!(health_status(&m), "healthy");
    }

    #[test]
    fn health_status_degraded_when_no_peers() {
        let m = Metrics::new().expect("metrics init");
        assert_eq!(m.peer_count.get(), 0);
        assert_eq!(health_status(&m), "degraded");
    }

    #[test]
    fn health_status_transitions() {
        let m = Metrics::new().expect("metrics init");
        assert_eq!(health_status(&m), "degraded");
        m.peer_count.set(1);
        assert_eq!(health_status(&m), "healthy");
        m.peer_count.set(0);
        assert_eq!(health_status(&m), "degraded");
    }

    #[test]
    fn uptime_is_non_negative() {
        let m = Metrics::new().expect("metrics init");
        let elapsed = m.uptime_start.elapsed().as_secs();
        // Uptime should be 0 or a small positive number (test runs fast).
        assert!(elapsed < 5, "uptime should be near zero in a test");
    }

    #[test]
    fn readiness_not_ready_when_no_peers_or_height() {
        let m = Metrics::new().expect("metrics init");
        let ready = m.peer_count.get() > 0 && m.block_height.get() > 0;
        assert!(!ready, "should not be ready with zero peers and height");
    }

    #[test]
    fn readiness_not_ready_when_no_peers() {
        let m = Metrics::new().expect("metrics init");
        m.block_height.set(10);
        let ready = m.peer_count.get() > 0 && m.block_height.get() > 0;
        assert!(!ready, "should not be ready with zero peers");
    }

    #[test]
    fn readiness_not_ready_when_no_height() {
        let m = Metrics::new().expect("metrics init");
        m.peer_count.set(2);
        let ready = m.peer_count.get() > 0 && m.block_height.get() > 0;
        assert!(!ready, "should not be ready with zero block height");
    }

    #[test]
    fn readiness_ready_when_peers_and_height() {
        let m = Metrics::new().expect("metrics init");
        m.peer_count.set(2);
        m.block_height.set(10);
        let ready = m.peer_count.get() > 0 && m.block_height.get() > 0;
        assert!(ready, "should be ready with peers and block height");
    }

    #[test]
    fn block_height_gauge_updates() {
        let m = Metrics::new().expect("metrics init");
        assert_eq!(m.block_height.get(), 0);
        m.block_height.set(100);
        assert_eq!(m.block_height.get(), 100);
        m.block_height.inc();
        assert_eq!(m.block_height.get(), 101);
    }

    #[test]
    fn histogram_records_values() {
        let m = Metrics::new().expect("metrics init");
        m.block_production_ms.observe(0.05);
        m.block_production_ms.observe(0.25);
        m.block_production_ms.observe(1.5);

        let output = m.gather();
        assert!(
            output.contains("shell_block_production_duration_seconds_count 3"),
            "histogram should record 3 observations"
        );
    }
}

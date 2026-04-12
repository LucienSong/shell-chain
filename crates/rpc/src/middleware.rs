//! Custom tower middleware layers for the JSON-RPC server.
//!
//! Both `RateLimitLayer` and `ApiKeyLayer` are `Clone` by design so they
//! can be composed with jsonrpsee's `set_http_middleware`.

use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use http::{Request, Response, StatusCode};
use parking_lot::Mutex;
use tower::{Layer, Service};

// ---------------------------------------------------------------------------
// RateLimitLayer — server-wide fixed-window request rate limiter
// ---------------------------------------------------------------------------

/// Shared state for the server-wide fixed-window rate limiter.
/// All connections share a single counter; this prevents a single burst of
/// global traffic from overloading the server.
///
/// Note: this is a **server-wide** (not per-IP) limiter. All clients share
/// the same request budget. A per-IP limiter would require extracting the
/// remote address from the connection-level context (e.g. via a
/// `ConnectInfo` extension), which is not available at the HTTP middleware
/// layer. Operators who need per-IP limiting should use a reverse proxy
/// (e.g. nginx/HAProxy) in front of the RPC server.
struct RateLimiterState {
    max_per_sec: u32,
    window_start: Instant,
    count: u32,
}

impl RateLimiterState {
    fn new(max_per_sec: u32) -> Self {
        Self {
            max_per_sec,
            window_start: Instant::now(),
            count: 0,
        }
    }

    /// Returns `true` if the request is allowed (within the current window).
    fn check_and_record(&mut self) -> bool {
        let now = Instant::now();
        if now.duration_since(self.window_start) >= Duration::from_secs(1) {
            self.window_start = now;
            self.count = 0;
        }
        if self.count >= self.max_per_sec {
            return false;
        }
        self.count += 1;
        true
    }
}

/// Tower layer that enforces a global request rate limit (req/sec).
/// Clone-compatible: all clones share the same `Arc<Mutex<RateLimiterState>>`.
#[derive(Clone)]
pub struct RateLimitLayer {
    state: Arc<Mutex<RateLimiterState>>,
}

impl RateLimitLayer {
    pub fn new(max_per_sec: u32) -> Self {
        Self {
            state: Arc::new(Mutex::new(RateLimiterState::new(max_per_sec))),
        }
    }

    /// Create from an optional config value. When `None`, the limit is set to
    /// `u32::MAX` (effectively disabled) so the layer type stays uniform.
    pub fn from_config(max_per_sec: Option<u32>) -> Self {
        Self::new(max_per_sec.unwrap_or(u32::MAX))
    }
}

impl<S> Layer<S> for RateLimitLayer {
    type Service = RateLimitService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RateLimitService {
            inner,
            state: Arc::clone(&self.state),
        }
    }
}

/// Tower service produced by `RateLimitLayer`.
#[derive(Clone)]
pub struct RateLimitService<S> {
    inner: S,
    state: Arc<Mutex<RateLimiterState>>,
}

impl<S, ReqBody, ResBody> Service<Request<ReqBody>> for RateLimitService<S>
where
    S: Service<Request<ReqBody>, Response = Response<ResBody>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    ReqBody: Send + 'static,
    ResBody: Default + Send + 'static,
{
    type Response = Response<ResBody>;
    type Error = S::Error;
    type Future = futures_util::future::Either<
        S::Future,
        std::future::Ready<Result<Response<ResBody>, S::Error>>,
    >;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<ReqBody>) -> Self::Future {
        if self.state.lock().check_and_record() {
            futures_util::future::Either::Left(self.inner.call(req))
        } else {
            let mut resp = Response::new(ResBody::default());
            *resp.status_mut() = StatusCode::TOO_MANY_REQUESTS;
            futures_util::future::Either::Right(std::future::ready(Ok(resp)))
        }
    }
}

// ---------------------------------------------------------------------------
// ApiKeyLayer — Bearer token authentication
// ---------------------------------------------------------------------------

/// Tower layer that enforces `Authorization: Bearer <key>` on **all** requests.
/// When `api_key` is `None`, the layer is a no-op pass-through.
/// Clone-compatible: holds the key in an `Arc<str>`.
///
/// Note: this layer authenticates every HTTP request regardless of the
/// JSON-RPC method name. All methods (reads and writes) require the Bearer
/// token when an API key is configured.
#[derive(Clone)]
pub struct ApiKeyLayer {
    api_key: Option<Arc<str>>,
}

impl ApiKeyLayer {
    pub fn new(api_key: Option<String>) -> Self {
        Self {
            api_key: api_key.map(|k| Arc::from(k.as_str())),
        }
    }
}

impl<S> Layer<S> for ApiKeyLayer {
    type Service = ApiKeyService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ApiKeyService {
            inner,
            api_key: self.api_key.clone(),
        }
    }
}

/// Tower service produced by `ApiKeyLayer`.
#[derive(Clone)]
pub struct ApiKeyService<S> {
    inner: S,
    api_key: Option<Arc<str>>,
}

impl<S, ReqBody, ResBody> Service<Request<ReqBody>> for ApiKeyService<S>
where
    S: Service<Request<ReqBody>, Response = Response<ResBody>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    ReqBody: Send + 'static,
    ResBody: Default + Send + 'static,
{
    type Response = Response<ResBody>;
    type Error = S::Error;
    type Future = futures_util::future::Either<
        S::Future,
        std::future::Ready<Result<Response<ResBody>, S::Error>>,
    >;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<ReqBody>) -> Self::Future {
        if let Some(ref key) = self.api_key {
            let expected = format!("Bearer {key}");
            let auth = req
                .headers()
                .get(http::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");

            if auth != expected {
                let mut resp = Response::new(ResBody::default());
                *resp.status_mut() = StatusCode::UNAUTHORIZED;
                return futures_util::future::Either::Right(std::future::ready(Ok(resp)));
            }
        }
        futures_util::future::Either::Left(self.inner.call(req))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::{Request, Response, StatusCode};
    use std::convert::Infallible;
    use tower::{Layer, Service, ServiceExt};

    // Minimal echo service for testing.
    #[derive(Clone)]
    struct OkService;
    impl Service<Request<()>> for OkService {
        type Response = Response<()>;
        type Error = Infallible;
        type Future = std::future::Ready<Result<Self::Response, Self::Error>>;

        fn poll_ready(
            &mut self,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn call(&mut self, _: Request<()>) -> Self::Future {
            std::future::ready(Ok(Response::new(())))
        }
    }

    #[tokio::test]
    async fn rate_limit_allows_within_window() {
        let layer = RateLimitLayer::new(100);
        let mut svc = layer.layer(OkService);
        let req = Request::new(());
        let resp = svc.ready().await.unwrap().call(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn rate_limit_rejects_over_quota() {
        let layer = RateLimitLayer::new(1);
        let mut svc = layer.layer(OkService);
        // First request: allowed.
        let _ = svc
            .ready()
            .await
            .unwrap()
            .call(Request::new(()))
            .await
            .unwrap();
        // Second request in same second: rejected.
        let resp = svc
            .ready()
            .await
            .unwrap()
            .call(Request::new(()))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn api_key_passes_with_correct_token() {
        let layer = ApiKeyLayer::new(Some("secret".into()));
        let mut svc = layer.layer(OkService);
        let req = Request::builder()
            .header(http::header::AUTHORIZATION, "Bearer secret")
            .body(())
            .unwrap();
        let resp = svc.ready().await.unwrap().call(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn api_key_rejects_wrong_token() {
        let layer = ApiKeyLayer::new(Some("secret".into()));
        let mut svc = layer.layer(OkService);
        let req = Request::builder()
            .header(http::header::AUTHORIZATION, "Bearer wrong")
            .body(())
            .unwrap();
        let resp = svc.ready().await.unwrap().call(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn api_key_disabled_passes_all() {
        let layer = ApiKeyLayer::new(None);
        let mut svc = layer.layer(OkService);
        let req = Request::new(());
        let resp = svc.ready().await.unwrap().call(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}

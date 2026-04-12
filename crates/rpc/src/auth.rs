//! API key authentication for the JSON-RPC server.
//!
//! The `ApiKeyLayer` in [`crate::middleware`] is the primary implementation.
//! This module re-exports the tower-http primitive for callers that prefer
//! the standard HTTP `Authorization: Bearer` validation header approach.

pub use tower_http::validate_request::ValidateRequestHeaderLayer;

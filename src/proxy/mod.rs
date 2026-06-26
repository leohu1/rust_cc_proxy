pub mod streaming;

use crate::error::AppError;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;
use std::time::Duration;

/// Shared HTTP client for forwarding requests to upstream providers.
///
/// reqwest::Client is designed to be cloned and shared across tasks —
/// it manages an internal connection pool.
#[derive(Clone)]
pub struct ProxyClient {
    client: reqwest::Client,
    default_upstream: String,
}

impl ProxyClient {
    /// Create a new proxy client with connection pooling.
    pub fn new(
        default_upstream: String,
        timeout_secs: u64,
        pool_max: usize,
    ) -> Result<Self, AppError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .pool_max_idle_per_host(pool_max)
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_keepalive(Duration::from_secs(60))
            .build()
            .map_err(|e| AppError::Internal(format!("failed to create HTTP client: {e}")))?;

        Ok(ProxyClient {
            client,
            default_upstream,
        })
    }

    /// Forward a non-streaming request to the upstream and return the full response body.
    pub async fn forward_non_streaming(
        &self,
        path: &str,
        body: &Value,
        upstream_url: Option<&str>,
        incoming_headers: &HeaderMap,
        extra_headers: HeaderMap,
    ) -> Result<reqwest::Response, AppError> {
        let base = upstream_url.unwrap_or(&self.default_upstream);
        let url = format!("{base}{path}");

        let req = self
            .client
            .post(&url)
            .json(body)
            .headers(self.build_forward_headers(incoming_headers, extra_headers))
            .version(reqwest::Version::HTTP_11);

        let response = req.send().await?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let resp_body = response.text().await.unwrap_or_default();
            return Err(AppError::UpstreamError {
                status,
                body: resp_body,
            });
        }

        Ok(response)
    }

    /// Forward a streaming request and return the byte stream.
    pub async fn forward_streaming(
        &self,
        path: &str,
        body: &Value,
        upstream_url: Option<&str>,
        incoming_headers: &HeaderMap,
        extra_headers: HeaderMap,
    ) -> Result<reqwest::Response, AppError> {
        let base = upstream_url.unwrap_or(&self.default_upstream);
        let url = format!("{base}{path}");

        let req = self
            .client
            .post(&url)
            .json(body)
            .headers(self.build_forward_headers(incoming_headers, extra_headers))
            .version(reqwest::Version::HTTP_11);

        let response = req.send().await?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let resp_body = response.text().await.unwrap_or_default();
            return Err(AppError::UpstreamError {
                status,
                body: resp_body,
            });
        }

        Ok(response)
    }

    /// Forward a GET request to an upstream endpoint.
    pub async fn forward_get(
        &self,
        path: &str,
        upstream_url: Option<&str>,
        extra_headers: HeaderMap,
    ) -> Result<reqwest::Response, AppError> {
        let base = upstream_url.unwrap_or(&self.default_upstream);
        let url = format!("{base}{path}");

        let req = self
            .client
            .get(&url)
            .headers(extra_headers)
            .version(reqwest::Version::HTTP_11);

        let response = req.send().await?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let resp_body = response.text().await.unwrap_or_default();
            return Err(AppError::UpstreamError { status, body: resp_body });
        }

        Ok(response)
    }

    /// Build the set of headers to forward to the upstream.
    fn build_forward_headers(
        &self,
        incoming: &HeaderMap,
        extra: HeaderMap,
    ) -> HeaderMap {
        let mut headers = HeaderMap::new();

        // Forward relevant headers from the client request
        let forward_keys = [
            "anthropic-beta",
            "anthropic-version",
            "x-api-key",
            "authorization",
            "content-type",
            "x-claude-code-session-id",
            "x-claude-code-agent-id",
            "x-claude-code-parent-agent-id",
        ];

        for key in &forward_keys {
            if let Some(value) = incoming.get(*key) {
                if let Ok(hdr_name) = HeaderName::from_bytes(key.as_bytes()) {
                    headers.insert(hdr_name, value.clone());
                }
            }
        }

        // Merge in extra headers (provider-specific auth, etc.)
        for (key, value) in extra.iter() {
            headers.insert(key.clone(), value.clone());
        }

        // Ensure content-type is set
        if !headers.contains_key("content-type") {
            headers.insert(
                reqwest::header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
        }

        headers
    }
}

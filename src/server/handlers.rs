use std::time::Instant;

use actix_web::{web, HttpRequest, HttpResponse};
use reqwest::header::HeaderMap;
use serde_json::Value;

use crate::error::AppError;
use crate::monitor::TokenMonitor;
use crate::protocol::models::ModelListResponse;
use crate::proxy::streaming;
use crate::server::AppState;

/// GET /health — health check, cc-switch compatible.
pub async fn health(state: web::Data<AppState>) -> HttpResponse {
    HttpResponse::Ok().json(serde_json::json!({
        "status": "healthy",
        "timestamp": chrono_now(),
        "version": env!("CARGO_PKG_VERSION"),
        "upstream": state.config.upstream.base_url,
    }))
}

/// GET /status — proxy status (cc-switch compatible).
pub async fn status_handler(state: web::Data<AppState>) -> HttpResponse {
    let usage = state.token_monitor.usage_response();
    HttpResponse::Ok().json(serde_json::json!({
        "status": "running",
        "uptime_secs": usage.uptime_secs,
        "requests_total": usage.requests_total,
        "requests_streaming": usage.requests_streaming,
        "requests_non_streaming": usage.requests_non_streaming,
        "errors_total": usage.errors_total,
        "input_tokens_total": usage.input_tokens_total,
        "output_tokens_total": usage.output_tokens_total,
    }))
}

/// GET /v1/usage — cc-switch compatible token usage endpoint.
///
/// Usage scripts in cc-switch call `GET /v1/usage` to query token
/// consumption. Returns cumulative token counts across all requests.
pub async fn usage_handler(state: web::Data<AppState>) -> HttpResponse {
    HttpResponse::Ok().json(state.token_monitor.usage_response())
}

/// GET /v1/models — model discovery for Claude Code's `/model` picker (CC Switch).
pub async fn models_handler(state: web::Data<AppState>) -> HttpResponse {
    HttpResponse::Ok().json(ModelListResponse::new(state.provider_registry.all_models()))
}

/// GET /user/balance — DeepSeek-compatible balance endpoint for cc-switch.
///
/// cc-switch's built-in "third-party balance" template auto-detects providers
/// by base URL. For users who select that template, we expose a DeepSeek-format
/// balance endpoint that reports token usage as a virtual "balance".
///
/// When DeepSeek is the upstream, this can also proxy to the real DeepSeek
/// balance API. Use `PROXY_BALANCE_MODE=passthrough` to forward to upstream,
/// or `PROXY_BALANCE_MODE=virtual` (default) to report token stats.
/// GET /user/balance — DeepSeek-compatible balance endpoint.
///
/// The DeepSeek balance API is at a FIXED URL (https://api.deepseek.com/user/balance),
/// independent of the Messages API endpoint. When DeepSeek provider is configured with
/// an API key, we proxy directly to this URL and return the real CNY balance.
/// Otherwise, we fall back to estimated token-based cost.
///
/// Ref: https://api-docs.deepseek.com/api/get-user-balance
pub async fn balance_handler(state: web::Data<AppState>) -> HttpResponse {
    // DeepSeek real balance: GET https://api.deepseek.com/user/balance (fixed URL)
    if let Some(_) = state
        .provider_registry
        .get(&crate::providers::ProviderKind::DeepSeek)
    {
        let api_key = state
            .config
            .providers
            .get("deepseek")
            .and_then(|c| c.api_key.clone());

        if let Some(key) = api_key {
            let mut headers = reqwest::header::HeaderMap::new();
            if let Ok(val) = reqwest::header::HeaderValue::from_str(&format!("Bearer {key}")) {
                headers.insert(
                    reqwest::header::HeaderName::from_static("authorization"),
                    val,
                );
            }
            headers.insert(
                reqwest::header::HeaderName::from_static("accept"),
                reqwest::header::HeaderValue::from_static("application/json"),
            );

            // Balance API uses the base domain, NOT the /anthropic endpoint
            match state
                .proxy_client
                .forward_get(
                    "/user/balance",
                    Some("https://api.deepseek.com"), // Fixed URL per official docs
                    headers,
                )
                .await
            {
                Ok(resp) => {
                    let body: serde_json::Value = match resp.json().await {
                        Ok(v) => v,
                        Err(e) => {
                            return HttpResponse::BadGateway().json(serde_json::json!({
                                "error": format!("failed to parse balance: {e}")
                            }))
                        }
                    };
                    tracing::debug!(
                        "DeepSeek balance: {}",
                        serde_json::to_string(&body).unwrap_or_default()
                    );
                    return HttpResponse::Ok().json(body);
                }
                Err(e) => {
                    tracing::warn!(
                        "DeepSeek balance proxy failed: {e}, falling back to token stats"
                    );
                }
            }
        }
    }

    // Fallback: estimate cost from cumulative token usage (1 CNY ≈ 1M tokens)
    let usage = state.token_monitor.usage_response();
    let total_tokens = usage.input_tokens_total + usage.output_tokens_total;
    let estimated_cny = total_tokens as f64 / 1_000_000.0;
    HttpResponse::Ok().json(serde_json::json!({
        "is_available": true,
        "balance_infos": [
            {
                "currency": "CNY",
                "total_balance": format!("{estimated_cny:.2}"),
                "granted_balance": "0.00",
                "topped_up_balance": format!("{estimated_cny:.2}"),
            }
        ]
    }))
}

/// GET /metrics — dev-mode monitoring endpoint.
pub async fn metrics_handler(state: web::Data<AppState>) -> HttpResponse {
    HttpResponse::Ok().json(state.token_monitor.metrics_response())
}

/// POST /v1/retrieve — CCR content retrieval.
///
/// Called by the LLM (via `headroom_retrieve` tool) to get back original
/// content that was compressed with a `<<ccr:HASH>>` marker.
pub async fn retrieve_handler(
    state: web::Data<AppState>,
    body: web::Json<Value>,
) -> Result<HttpResponse, AppError> {
    let hash = body
        .get("hash")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::InvalidRequest("missing 'hash' field".into()))?;

    match &state.compressor {
        Some(compressor) => match compressor.retrieve(hash) {
            Some(content) => Ok(HttpResponse::Ok().json(serde_json::json!({
                "hash": hash,
                "content": content,
            }))),
            None => Err(AppError::InvalidRequest(format!("hash not found: {hash}"))),
        },
        None => Err(AppError::InvalidRequest(
            "compression is not enabled".into(),
        )),
    }
}

/// GET /v1/compression/stats — CCR store statistics.
pub async fn compression_stats_handler(state: web::Data<AppState>) -> HttpResponse {
    match &state.compressor {
        Some(c) => HttpResponse::Ok().json(c.stats()),
        None => HttpResponse::Ok().json(serde_json::json!({
            "enabled": false,
            "message": "compression is not enabled"
        })),
    }
}

/// POST /v1/messages — main chat completion endpoint.
/// POST /v1/messages — main chat completion endpoint.
pub async fn messages_handler(
    req: HttpRequest,
    state: web::Data<AppState>,
    body: web::Json<Value>,
) -> Result<HttpResponse, AppError> {
    let start = Instant::now();

    // Rate limiting
    let provider_name = {
        let model = body
            .get("model")
            .and_then(|m| m.as_str())
            .unwrap_or("default");
        if model.starts_with("deepseek") {
            "deepseek"
        } else {
            "anthropic"
        }
    };
    if !state.rate_limiter.allow(provider_name) {
        return Err(AppError::InvalidRequest(
            "rate limit exceeded, try again later".into(),
        ));
    }

    let mut body = body.into_inner();
    let incoming_headers = convert_headers(req.headers());

    // ── Request metadata ──────────────────────────────────────────
    let requested_model = body
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("claude-sonnet-4-20250514")
        .to_string();
    let is_stream = body
        .get("stream")
        .and_then(|s| s.as_bool())
        .unwrap_or(false);
    let estimated_input = TokenMonitor::estimate_input_tokens(&body);

    if state.config.dev_mode {
        tracing::info!(
            "→ REQ  model={requested_model}  stream={is_stream}  est_input_tokens={estimated_input}",
        );
    }

    // Step 1: Run the pipeline
    let pipeline_start = Instant::now();
    state.pipeline.run(&mut body)?;
    if state.config.dev_mode {
        tracing::debug!(
            "  pipeline: {} stages in {:.0}ms",
            state.pipeline.len(),
            pipeline_start.elapsed().as_millis()
        );
    }

    // Step 2: Resolve provider
    let (provider, upstream_model) = state.provider_registry.resolve(&requested_model);
    if state.config.dev_mode {
        tracing::info!(
            "  provider={:?}  upstream_model={upstream_model}  upstream={}",
            provider.kind(),
            provider.upstream_url()
        );
    }

    // Step 3: Update model field
    if let Some(obj) = body.as_object_mut() {
        obj.insert("model".to_string(), Value::String(upstream_model.clone()));
    }

    // Step 4: Provider-specific request transformation
    let mut transformed = body.clone();
    provider.transform_request(&mut transformed)?;

    // Step 5: Provider-specific headers
    let extra_headers = provider.prepare_headers(&incoming_headers);

    // Step 6: Forward to upstream
    let upstream_start = Instant::now();

    if is_stream {
        let response = state
            .proxy_client
            .forward_streaming(
                "/v1/messages",
                &transformed,
                Some(provider.upstream_url()),
                &incoming_headers,
                extra_headers,
            )
            .await
            .map_err(|e| {
                state.token_monitor.record_error();
                if state.config.dev_mode {
                    tracing::error!("← ERR  {e}  ({:.0}ms)", start.elapsed().as_millis());
                }
                e
            })?;

        if state.config.dev_mode {
            tracing::info!(
                "→ SSE  streaming  upstream_connect={:.0}ms",
                upstream_start.elapsed().as_millis()
            );
        }

        // Intercept SSE to extract token usage from message_start/message_delta
        let sse_stream =
            streaming::into_sse_stream_with_monitor(response, state.token_monitor.clone());

        Ok(HttpResponse::Ok()
            .content_type("text/event-stream")
            .insert_header(("Cache-Control", "no-cache"))
            .insert_header(("Connection", "keep-alive"))
            .insert_header(("X-Accel-Buffering", "no"))
            .streaming(sse_stream))
    } else {
        let response = state
            .proxy_client
            .forward_non_streaming(
                "/v1/messages",
                &transformed,
                Some(provider.upstream_url()),
                &incoming_headers,
                extra_headers,
            )
            .await
            .map_err(|e| {
                state.token_monitor.record_error();
                if state.config.dev_mode {
                    tracing::error!("← ERR  {e}  ({:.0}ms)", start.elapsed().as_millis());
                }
                e
            })?;

        let upstream_latency = upstream_start.elapsed();

        let response_body: Value = response.json().await.map_err(|e| {
            state.token_monitor.record_error();
            AppError::UpstreamError {
                status: 502,
                body: format!("failed to parse upstream response: {e}"),
            }
        })?;

        // Track token usage
        let (input_tokens, output_tokens) =
            TokenMonitor::parse_usage(&response_body).unwrap_or((estimated_input, 0));
        let (cache_read, cache_creation) = TokenMonitor::parse_cache_tokens(&response_body);

        state
            .token_monitor
            .record_non_streaming(input_tokens, output_tokens);
        state
            .token_monitor
            .record_cache_tokens(cache_read, cache_creation);
        state
            .token_monitor
            .record_latency(start.elapsed().as_millis() as u64);

        if state.config.dev_mode {
            tracing::info!(
                "← OK   model={upstream_model}  latency={:.0}ms  upstream={:.0}ms  \
                 tokens  in={input_tokens}  out={output_tokens}",
                start.elapsed().as_millis(),
                upstream_latency.as_millis(),
            );
        }

        Ok(HttpResponse::Ok().json(response_body))
    }
}

/// POST /v1/messages/count_tokens — token counting endpoint.
pub async fn count_tokens_handler(
    req: HttpRequest,
    state: web::Data<AppState>,
    body: web::Json<Value>,
) -> Result<HttpResponse, AppError> {
    let body = body.into_inner();
    let incoming_headers = convert_headers(req.headers());

    let requested_model = body
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("claude-sonnet-4-20250514");

    let (provider, _upstream_model) = state.provider_registry.resolve(requested_model);
    let extra_headers = provider.prepare_headers(&incoming_headers);

    let response = state
        .proxy_client
        .forward_non_streaming(
            "/v1/messages/count_tokens",
            &body,
            Some(provider.upstream_url()),
            &incoming_headers,
            extra_headers,
        )
        .await?;

    let response_body: Value = response.json().await.map_err(|e| AppError::UpstreamError {
        status: 502,
        body: format!("failed to parse upstream response: {e}"),
    })?;

    if state.config.dev_mode {
        let input_tokens = response_body
            .get("input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        tracing::debug!("  count_tokens: input_tokens={input_tokens}");
    }

    Ok(HttpResponse::Ok().json(response_body))
}

/// Convert actix-web HeaderMap to reqwest HeaderMap.
fn convert_headers(headers: &actix_web::http::header::HeaderMap) -> HeaderMap {
    let mut result = HeaderMap::new();
    for (key, value) in headers.iter() {
        if let Ok(hdr_name) = reqwest::header::HeaderName::from_bytes(key.as_str().as_bytes()) {
            if let Ok(hdr_value) = reqwest::header::HeaderValue::from_bytes(value.as_bytes()) {
                result.insert(hdr_name, hdr_value);
            }
        }
    }
    result
}

fn chrono_now() -> String {
    // Simple RFC 3339 timestamp without pulling in chrono crate.
    use std::time::SystemTime;
    let dur = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs();
    // Format: 2026-06-26T12:00:00Z (approx)
    let days_since_epoch = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    // Compute year/month/day from days since Unix epoch
    let mut y = 1970i64;
    let mut remaining = days_since_epoch as i64;
    loop {
        let days_in_year = if is_leap(y) { 366 } else { 365 };
        if remaining < days_in_year {
            break;
        }
        remaining -= days_in_year;
        y += 1;
    }
    let month_days = if is_leap(y) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut m = 0;
    for (i, &md) in month_days.iter().enumerate() {
        if remaining < md {
            m = i + 1;
            break;
        }
        remaining -= md;
    }
    let d = remaining + 1;

    format!("{y:04}-{m:02}-{d:02}T{hours:02}:{minutes:02}:{seconds:02}Z")
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0)
}

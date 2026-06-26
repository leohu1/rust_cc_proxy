use actix_web::{test, web, HttpResponse};
use serde_json::Value;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use rust_cc_proxy::config::{Config, ProviderConfig, ServerConfig, UpstreamConfig};
use rust_cc_proxy::pipeline::Pipeline;
use rust_cc_proxy::providers::{self, ProviderKind, ProviderRegistry};
use rust_cc_proxy::proxy::ProxyClient;
use rust_cc_proxy::server::AppState;

/// Macro to build a test App. Avoids opaque return type issues with `impl` in
/// `build_test_app` by expanding inline at each call site.
macro_rules! make_test_app {
    ($upstream:expr) => {{
        let upstream: String = $upstream;
        let config = Config {
            server: ServerConfig {
                bind_addr: "127.0.0.1:0".parse().unwrap(),
                log_level: "debug".into(),
            },
            upstream: UpstreamConfig {
                base_url: upstream.clone(),
                api_key: None,
                timeout_secs: 30,
                pool_max_connections: 5,
            },
            providers: Default::default(),
            dump_dir: None,
            compression_enabled: false,
            dev_mode: false,
        };

        let proxy_client = ProxyClient::new(
            config.upstream.base_url.clone(),
            config.upstream.timeout_secs,
            config.upstream.pool_max_connections,
        )
        .unwrap();

        let mut registry = ProviderRegistry::new(ProviderKind::Anthropic);
        let anthropic_config = ProviderConfig {
            upstream_url: upstream,
            api_key: None,
            default_model: "claude-sonnet-4-20250514".into(),
            model_map: Default::default(),
        };
        registry.register(providers::create_provider(
            ProviderKind::Anthropic,
            &anthropic_config,
        ));

        let mut pipeline = Pipeline::new();
        pipeline.push(Arc::new(
            rust_cc_proxy::pipeline::system_normalizer::SystemRoleNormalizer,
        ));

        let state = web::Data::new(AppState {
            config,
            proxy_client,
            pipeline,
            provider_registry: registry,
            token_monitor: std::sync::Arc::new(rust_cc_proxy::monitor::TokenMonitor::new()),
            compressor: None,
            rate_limiter: rust_cc_proxy::server::rate_limiter::RateLimiter::default(),
        });

        actix_web::App::new()
            .app_data(state)
            .route("/health", web::get().to(rust_cc_proxy::server::handlers::health))
            .route("/v1/models", web::get().to(rust_cc_proxy::server::handlers::models_handler))
            .route("/v1/messages", web::post().to(rust_cc_proxy::server::handlers::messages_handler))
    }};
}

// ── Health endpoint ──────────────────────────────────────────────────

#[actix_web::test]
async fn test_health_returns_ok() {
    let app = test::init_service(make_test_app!("http://127.0.0.1:19999".to_string())).await;

    let req = test::TestRequest::get().uri("/health").to_request();
    let resp = test::call_service(&app, req).await;

    assert!(resp.status().is_success());
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["status"], "healthy");
}

// ── Models endpoint ──────────────────────────────────────────────────

#[actix_web::test]
async fn test_models_returns_list() {
    let app = test::init_service(make_test_app!("http://127.0.0.1:19999".to_string())).await;

    let req = test::TestRequest::get().uri("/v1/models").to_request();
    let resp = test::call_service(&app, req).await;

    assert!(resp.status().is_success());
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["object"], "list");
    assert!(body["data"].as_array().unwrap().len() > 0);
}

// ── Mock upstream helper ─────────────────────────────────────────────

async fn spawn_mock_upstream(
    expected_response: Value,
) -> (u16, Arc<AtomicUsize>, Arc<std::sync::Mutex<Option<Value>>>) {
    let request_count = Arc::new(AtomicUsize::new(0));
    let last_body: Arc<std::sync::Mutex<Option<Value>>> =
        Arc::new(std::sync::Mutex::new(None));

    let req_count = request_count.clone();
    let last_req = last_body.clone();

    let server = actix_web::HttpServer::new(move || {
        let resp = expected_response.clone();
        let req_count = req_count.clone();
        let last_req = last_req.clone();
        actix_web::App::new().route(
            "/v1/messages",
            web::post().to(move |body: web::Json<Value>| {
                req_count.fetch_add(1, Ordering::SeqCst);
                *last_req.lock().unwrap() = Some(body.into_inner());
                let resp = resp.clone();
                async move { HttpResponse::Ok().json(resp) }
            }),
        )
    })
    .bind("127.0.0.1:0")
    .unwrap();

    let port = server.addrs()[0].port();
    tokio::spawn(server.run());
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;

    (port, request_count, last_body)
}

// ── Messages endpoint ────────────────────────────────────────────────

#[actix_web::test]
async fn test_messages_non_streaming_forwards_to_upstream() {
    let mock_response = serde_json::json!({
        "id": "msg_001",
        "type": "message",
        "role": "assistant",
        "content": [{"type": "text", "text": "Hello from mock!"}],
        "model": "claude-sonnet-4-20250514",
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": {"input_tokens": 10, "output_tokens": 5}
    });

    let (port, request_count, _) = spawn_mock_upstream(mock_response).await;
    let upstream_url = format!("http://127.0.0.1:{port}");
    let app = test::init_service(make_test_app!(upstream_url)).await;

    let req = test::TestRequest::post()
        .uri("/v1/messages")
        .set_json(&serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "max_tokens": 100,
            "messages": [{"role": "user", "content": "Hello from test"}]
        }))
        .to_request();

    let resp = test::call_service(&app, req).await;
    assert!(resp.status().is_success());
    assert_eq!(request_count.load(Ordering::SeqCst), 1);

    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["id"], "msg_001");
}

#[actix_web::test]
async fn test_messages_passes_system_through_pipeline() {
    let mock_response = serde_json::json!({
        "id": "msg_002",
        "type": "message",
        "role": "assistant",
        "content": [{"type": "text", "text": "Got it."}],
        "model": "claude-sonnet-4-20250514",
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": {"input_tokens": 15, "output_tokens": 3}
    });

    let (port, _, last_body) = spawn_mock_upstream(mock_response).await;
    let upstream_url = format!("http://127.0.0.1:{port}");
    let app = test::init_service(make_test_app!(upstream_url)).await;

    let req = test::TestRequest::post()
        .uri("/v1/messages")
        .set_json(&serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "max_tokens": 100,
            "messages": [
                {"role": "system", "content": "You are a test assistant."},
                {"role": "user", "content": "Hello"}
            ]
        }))
        .to_request();

    let resp = test::call_service(&app, req).await;
    assert!(resp.status().is_success());

    let upstream = last_body.lock().unwrap();
    let upstream_body = upstream.as_ref().unwrap();
    let messages = upstream_body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1);
    let system = upstream_body["system"].as_str().unwrap();
    assert!(system.contains("You are a test assistant."));
}

#[actix_web::test]
async fn test_messages_returns_error_on_upstream_failure() {
    let server = actix_web::HttpServer::new(|| {
        actix_web::App::new().route(
            "/v1/messages",
            web::post().to(|| async {
                HttpResponse::InternalServerError()
                    .json(serde_json::json!({"error": "boom"}))
            }),
        )
    })
    .bind("127.0.0.1:0")
    .unwrap();

    let port = server.addrs()[0].port();
    tokio::spawn(server.run());
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;

    let upstream_url = format!("http://127.0.0.1:{port}");
    let app = test::init_service(make_test_app!(upstream_url)).await;

    let req = test::TestRequest::post()
        .uri("/v1/messages")
        .set_json(&serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "max_tokens": 100,
            "messages": [{"role": "user", "content": "Hello"}]
        }))
        .to_request();

    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status().as_u16(), 500);
}

#[actix_web::test]
async fn test_messages_with_deepseek_model_normalizes_request() {
    let mock_response = serde_json::json!({
        "id": "msg_003",
        "type": "message",
        "role": "assistant",
        "content": [{"type": "text", "text": "DeepSeek response"}],
        "model": "deepseek-v4-pro",
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": {"input_tokens": 10, "output_tokens": 5}
    });

    let (port, _, last_body) = spawn_mock_upstream(mock_response).await;
    let upstream_url = format!("http://127.0.0.1:{port}");
    let app = test::init_service(make_test_app!(upstream_url)).await;

    let req = test::TestRequest::post()
        .uri("/v1/messages")
        .set_json(&serde_json::json!({
            "model": "deepseek-v4-pro",
            "max_tokens": 100,
            "messages": [
                {"role": "system", "content": "System via messages."},
                {"role": "user", "content": "Hello"}
            ],
            "stream": false
        }))
        .to_request();

    let resp = test::call_service(&app, req).await;
    assert!(resp.status().is_success());

    let upstream = last_body.lock().unwrap();
    let upstream_body = upstream.as_ref().unwrap();
    assert!(upstream_body.get("system").is_some());
}

#[actix_web::test]
async fn test_messages_invalid_json_returns_error() {
    let app = test::init_service(
        make_test_app!("http://127.0.0.1:19999".to_string()),
    ).await;

    let req = test::TestRequest::post()
        .uri("/v1/messages")
        .insert_header(("content-type", "application/json"))
        .set_payload("not valid json")
        .to_request();

    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status().as_u16(), 400);
}

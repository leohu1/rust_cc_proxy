pub mod handlers;
pub mod rate_limiter;
pub mod shutdown;

use crate::compress::Compressor;
use crate::config::Config;
use crate::monitor::TokenMonitor;
use crate::pipeline::Pipeline;
use crate::providers::{self, ProviderKind, ProviderRegistry};
use crate::proxy::ProxyClient;
use std::sync::Arc;

/// Shared application state accessible from all handlers.
pub struct AppState {
    pub config: Config,
    pub proxy_client: ProxyClient,
    pub pipeline: Pipeline,
    pub provider_registry: ProviderRegistry,
    pub token_monitor: Arc<TokenMonitor>,
    pub compressor: Option<Arc<Compressor>>,
    pub rate_limiter: rate_limiter::RateLimiter,
}

/// Build and start the actix-web HTTP server.
pub async fn run(config: Config) -> std::io::Result<()> {
    use actix_web::{web, App, HttpServer};

    let bind_addr = config.server.bind_addr;
    let dev_mode = config.dev_mode;

    // Build proxy client
    let proxy_client = ProxyClient::new(
        config.upstream.base_url.clone(),
        config.upstream.timeout_secs,
        config.upstream.pool_max_connections,
    )
    .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;

    // Build provider registry — auto-select default: DeepSeek if configured, else Anthropic
    let default_kind = if config.providers.contains_key("deepseek") {
        ProviderKind::DeepSeek
    } else {
        ProviderKind::Anthropic
    };
    let mut registry = ProviderRegistry::new(default_kind);
    for (name, provider_config) in &config.providers {
        let kind = match name.as_str() {
            "deepseek" | "deepseek-v4" => ProviderKind::DeepSeek,
            "anthropic" => ProviderKind::Anthropic,
            _ => {
                tracing::warn!("Unknown provider: {name}, skipping");
                continue;
            }
        };
        let provider = providers::create_provider(kind, provider_config);
        registry.register(provider);
    }

    // Always register a default Anthropic passthrough if none configured
    if registry.get(&ProviderKind::Anthropic).is_none() {
        let anthropic_config = crate::config::ProviderConfig {
            upstream_url: config.upstream.base_url.clone(),
            api_key: config.upstream.api_key.clone(),
            default_model: "claude-sonnet-4-20250514".to_string(),
            model_map: Default::default(),
        };
        registry.register(providers::create_provider(
            ProviderKind::Anthropic,
            &anthropic_config,
        ));
    }

    // Build pipeline
    let mut pipeline = Pipeline::new();
    pipeline.push(Arc::new(
        crate::pipeline::system_normalizer::SystemRoleNormalizer,
    ));

    // Add compression stage if enabled
    let compressor: Option<Arc<Compressor>> = if config.compression_enabled {
        let c = Arc::new(Compressor::new(512, 10));
        pipeline.push(Arc::new(
            crate::compress::pipeline_stage::CompressionStage::new(c.clone()),
        ));
        tracing::info!("Token compression ENABLED (min=512B, max_array_items=10)");
        Some(c)
    } else {
        None
    };

    let state = web::Data::new(AppState {
        config: config.clone(),
        proxy_client,
        pipeline,
        provider_registry: registry,
        token_monitor: Arc::new(TokenMonitor::new()),
        compressor,
        rate_limiter: rate_limiter::RateLimiter::default(),
    });

    tracing::info!("Starting HTTP server on {bind_addr}");

    let server = HttpServer::new(move || {
        let app = App::new()
            .app_data(state.clone())
            // Always-on endpoints
            .route("/health", web::get().to(handlers::health))
            .route("/status", web::get().to(handlers::status_handler))
            .route("/user/balance", web::get().to(handlers::balance_handler))
            .route("/v1/usage", web::get().to(handlers::usage_handler))
            .route("/v1/retrieve", web::post().to(handlers::retrieve_handler))
            .route(
                "/v1/compression/stats",
                web::get().to(handlers::compression_stats_handler),
            )
            .route("/v1/models", web::get().to(handlers::models_handler))
            .route("/v1/messages", web::post().to(handlers::messages_handler))
            .route(
                "/v1/messages/count_tokens",
                web::post().to(handlers::count_tokens_handler),
            )
            // Production hardening
            .app_data(web::JsonConfig::default().limit(20 * 1024 * 1024)); // 20MB body limit

        // Dev-mode only endpoints
        if dev_mode {
            app.route("/metrics", web::get().to(handlers::metrics_handler))
        } else {
            app
        }
    })
    .bind(bind_addr)?
    .shutdown_timeout(30) // Graceful shutdown: 30s for in-flight requests
    .run();

    // Graceful shutdown on SIGTERM / Ctrl+C
    let server_handle = server.handle();
    tokio::spawn(async move {
        shutdown::graceful_shutdown().await;
        server_handle.stop(true).await;
    });

    server.await
}

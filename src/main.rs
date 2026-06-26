use clap::Parser;
use rust_cc_proxy::config;
use rust_cc_proxy::error;
use rust_cc_proxy::server;
use tracing_subscriber::EnvFilter;

/// Claude Code proxy server — routes requests to configurable LLM backends.
#[derive(Parser, Debug)]
#[command(name = "rust_cc_proxy", version = env!("CARGO_PKG_VERSION"))]
struct Cli {
    /// Bind address (overrides PROXY_HOST)
    #[arg(long)]
    host: Option<String>,

    /// Bind port (overrides PROXY_PORT)
    #[arg(long)]
    port: Option<u16>,

    /// Log level (overrides PROXY_LOG_LEVEL)
    #[arg(long)]
    log_level: Option<String>,

    /// Default upstream base URL (overrides PROXY_UPSTREAM)
    #[arg(long)]
    upstream: Option<String>,

    /// Enable development mode: verbose request logging + /metrics endpoint
    #[arg(long)]
    dev: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    // Initialize tracing — use debug level in dev mode
    let log_level = cli
        .log_level
        .clone()
        .or_else(|| std::env::var("PROXY_LOG_LEVEL").ok())
        .unwrap_or_else(|| {
            if cli.dev {
                "debug".to_string()
            } else {
                "info".to_string()
            }
        });

    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&log_level));

    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(false)
        .with_file(cli.dev)
        .with_line_number(cli.dev)
        .init();

    // Load configuration
    let mut config = config::Config::from_env()?;

    // CLI overrides
    if let Some(host) = cli.host {
        config.server.bind_addr.set_ip(
            host.parse()
                .map_err(|e| error::AppError::ConfigError(format!("invalid host: {e}")))?,
        );
    }
    if let Some(port) = cli.port {
        config.server.bind_addr.set_port(port);
    }
    if let Some(upstream) = cli.upstream {
        config.upstream.base_url = upstream;
    }
    if cli.dev {
        config.dev_mode = true;
    }

    tracing::info!(
        "rust_cc_proxy v{} starting on {}",
        env!("CARGO_PKG_VERSION"),
        config.server.bind_addr
    );
    tracing::info!("Default upstream: {}", config.upstream.base_url);
    if config.dev_mode {
        tracing::info!("Dev mode ENABLED — verbose logging + /metrics endpoint");
    }
    if !config.providers.is_empty() {
        tracing::info!(
            "Configured providers: {:?}",
            config.providers.keys().collect::<Vec<_>>()
        );
    }

    // Build and start the HTTP server
    server::run(config).await?;

    Ok(())
}

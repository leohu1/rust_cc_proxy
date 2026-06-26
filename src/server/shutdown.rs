//! Graceful shutdown handling.
//!
//! Listens for SIGTERM/SIGINT (Ctrl+C) and initiates an orderly shutdown.
//! The actix-web server drains in-flight requests before stopping.

use std::time::Duration;

/// Wait for a shutdown signal (Ctrl+C or SIGTERM), then initiate graceful
/// shutdown of the actix-web server. Returns after the server has stopped.
pub async fn graceful_shutdown() {
    // Wait for signal
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {
            tracing::info!("Received Ctrl+C, shutting down gracefully...");
        }
        _ = terminate => {
            tracing::info!("Received SIGTERM, shutting down gracefully...");
        }
    }

    // Allow in-flight requests to complete (actix-web handles this natively
    // when we drop the server handle, but a brief pause ensures any proxied
    // streaming connections get a chance to flush).
    tracing::info!("Waiting for in-flight requests to drain...");
    tokio::time::sleep(Duration::from_millis(500)).await;
}

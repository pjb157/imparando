mod api;
mod config;
mod profiles;
mod vm;

use anyhow::Result;
use clap::Parser;
use std::net::SocketAddr;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use config::{Cli, Config};
use vm::SessionManager;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "imparando=info,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cli = Cli::parse();
    let config = Config::load(&cli)?;

    tokio::fs::create_dir_all(&config.run_dir).await?;
    tokio::fs::create_dir_all(&config.data_dir).await?;

    if !config.kernel_path.exists() {
        tracing::warn!(
            path = ?config.kernel_path,
            "Kernel not found — run scripts/download-kernel.sh"
        );
    }
    if !config.base_rootfs_path.exists() {
        tracing::warn!(
            path = ?config.base_rootfs_path,
            "Base rootfs not found — run scripts/build-rootfs.sh"
        );
    }

    let manager = SessionManager::new(config.clone());
    let shutdown_manager = std::sync::Arc::clone(&manager);

    // Start the HTTP CONNECT proxy for VM outbound traffic.
    // VMs set HTTP(S)_PROXY=http://172.16.x.1:3128 so their traffic flows
    // through the proxy over plain TCP, avoiding NAT/iptables TLS issues.
    let proxy_addr = SocketAddr::from(([0, 0, 0, 0], 3128));
    tokio::spawn(async move {
        if let Err(e) = vm::proxy::run_connect_proxy(proxy_addr).await {
            tracing::error!(error = %e, "CONNECT proxy failed");
        }
    });

    let app = api::router(manager, config.user.clone(), config.pass.clone());
    let addr = SocketAddr::from(([0, 0, 0, 0], config.port));
    let listener = tokio::net::TcpListener::bind(addr).await?;

    tracing::info!("imparando listening on http://{addr}");

    // Run the server until a shutdown signal is received.
    tokio::select! {
        result = axum::serve(listener, app) => {
            if let Err(e) = result {
                tracing::error!("Server error: {e}");
            }
        }
        _ = shutdown_signal() => {
            tracing::info!("Shutdown signal received");
        }
    }

    tracing::info!("Shutting down sessions...");
    shutdown_manager.shutdown().await;
    tracing::info!("Shutdown complete");

    Ok(())
}

/// Resolves when Ctrl+C or SIGTERM is received.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let sigterm = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let sigterm = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = sigterm => {}
    }
}

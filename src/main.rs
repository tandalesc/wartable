mod api;
mod config;
mod events;
mod mcp;
mod models;
mod scheduler;
mod server;
mod worker;

use config::Config;
use events::EventBus;
use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,rmcp=info".into()),
        )
        .init();

    let config = Config::load()?;
    info!("loaded config from {:?}", Config::config_path());

    // Ensure log directory exists
    let log_dir = config.log_dir();
    tokio::fs::create_dir_all(&log_dir).await?;
    info!(log_dir = %log_dir.display(), "log directory ready");

    let event_bus = EventBus::new(256);
    let scheduler = scheduler::start(config.clone(), event_bus.clone());
    info!("scheduler actor started");

    let router = server::build_router(scheduler, event_bus);

    let addr = format!("{}:{}", config.server.host, config.server.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!(addr = %addr, "wartable listening");
    info!(mcp = %format!("http://{}:{}/mcp", config.server.host, config.server.port), "MCP endpoint");
    info!(api = %format!("http://{}:{}/api", config.server.host, config.server.port), "REST API");

    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            tokio::signal::ctrl_c().await.unwrap();
            info!("shutting down");
        })
        .await?;

    Ok(())
}

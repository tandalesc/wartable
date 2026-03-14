use axum::Router;
use axum::routing::{get, post};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::StreamableHttpService;
use std::path::PathBuf;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;

use crate::api;
use crate::config::Config;
use crate::events::EventBus;
use crate::mcp::WartableTools;
use crate::scheduler::SchedulerHandle;

pub fn build_router(
    config: &Config,
    scheduler: SchedulerHandle,
    _event_bus: EventBus,
) -> Router {
    let scheduler_for_mcp = scheduler.clone();

    // MCP service via rmcp streamable HTTP
    let mcp_service = StreamableHttpService::new(
        move || Ok(WartableTools::new(scheduler_for_mcp.clone())),
        LocalSessionManager::default().into(),
        Default::default(),
    );

    // REST API for dashboard
    let api_router = Router::new()
        .route("/jobs", get(api::list_jobs))
        .route("/jobs/{id}", get(api::get_job))
        .route("/jobs/{id}/logs", get(api::get_job_logs))
        .route("/jobs/{id}/cancel", post(api::cancel_job))
        .with_state(scheduler);

    // Static dashboard files - check multiple locations
    let dashboard_dir = config
        .dashboard
        .static_dir
        .as_ref()
        .map(PathBuf::from)
        .filter(|p| p.exists())
        .or_else(|| {
            // Next to the binary
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|p| p.join("dashboard")))
                .filter(|p| p.exists())
        })
        .or_else(|| {
            // /opt/wartable/dashboard (Docker)
            let p = PathBuf::from("/opt/wartable/dashboard");
            p.exists().then_some(p)
        })
        .unwrap_or_else(|| {
            // Build-time fallback
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("dashboard")
        });

    Router::new()
        .nest_service("/mcp", mcp_service)
        .nest("/api", api_router)
        .fallback_service(ServeDir::new(dashboard_dir))
        .layer(CorsLayer::permissive())
}

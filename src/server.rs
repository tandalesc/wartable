use axum::Router;
use axum::routing::{get, post};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::StreamableHttpService;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;

use crate::api;
use crate::events::EventBus;
use crate::mcp::WartableTools;
use crate::scheduler::SchedulerHandle;

pub fn build_router(
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

    // Static dashboard files
    let dashboard_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("dashboard");

    Router::new()
        .nest_service("/mcp", mcp_service)
        .nest("/api", api_router)
        .fallback_service(ServeDir::new(dashboard_dir))
        .layer(CorsLayer::permissive())
}

use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use axum::Router;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::StreamableHttpService;
use std::path::PathBuf;
use std::sync::Arc;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;

use crate::api::{self, ApiState};
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

    // Build allowed_dirs for file download endpoint
    let mut allowed_dirs = Vec::new();
    let log_dir = config.log_dir();
    if let Ok(canon) = std::fs::canonicalize(&log_dir) {
        allowed_dirs.push(canon);
    } else {
        allowed_dirs.push(log_dir);
    }
    let work_dir = config.working_dir();
    if let Ok(canon) = std::fs::canonicalize(&work_dir) {
        allowed_dirs.push(canon);
    } else {
        allowed_dirs.push(work_dir);
    }

    let api_state = ApiState {
        scheduler,
        allowed_dirs,
    };

    // REST API for dashboard
    let api_router = Router::new()
        .route("/jobs", get(api::list_jobs))
        .route("/jobs/{id}", get(api::get_job))
        .route("/jobs/{id}/logs", get(api::get_job_logs))
        .route("/jobs/{id}/cancel", axum::routing::post(api::cancel_job))
        .route("/resources", get(api::get_resources))
        .route("/files/{*path}", get(api::get_file))
        .with_state(api_state);

    // Auth config
    let auth_keys: Option<Arc<Vec<String>>> = if config.auth.enabled {
        Some(Arc::new(
            config.auth.api_keys.iter().map(|k| k.key.clone()).collect(),
        ))
    } else {
        None
    };

    // Protected routes (API + MCP) with optional auth
    let protected = Router::new()
        .nest_service("/mcp", mcp_service)
        .nest("/api", api_router)
        .layer(middleware::from_fn(move |req, next| {
            let keys = auth_keys.clone();
            auth_middleware(keys, req, next)
        }));

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
        .merge(protected)
        .fallback_service(ServeDir::new(dashboard_dir))
        .layer(CorsLayer::permissive())
}

async fn auth_middleware(
    keys: Option<Arc<Vec<String>>>,
    req: Request,
    next: Next,
) -> Response {
    let keys = match keys {
        Some(k) => k,
        None => return next.run(req).await,
    };

    // Extract key from Authorization: Bearer <key> or X-API-Key: <key>
    let provided = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.to_string())
        .or_else(|| {
            req.headers()
                .get("x-api-key")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
        });

    match provided {
        Some(key) if keys.contains(&key) => next.run(req).await,
        _ => (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "Invalid or missing API key" })),
        )
            .into_response(),
    }
}

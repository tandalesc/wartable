use axum::extract::Request;
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::Router;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::StreamableHttpService;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tracing::warn;

use crate::api::{self, ApiState};
use crate::config::Config;
use crate::download::DownloadSigner;
use crate::events::EventBus;
use crate::keys::KeyStore;
use crate::mcp::WartableTools;
use crate::scheduler::SchedulerHandle;

/// Tracks recently-seen API clients by key name.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ClientInfo {
    pub name: String,
    pub last_seen: chrono::DateTime<chrono::Utc>,
    pub request_count: u64,
}

pub type ClientTracker = Arc<RwLock<HashMap<String, ClientInfo>>>;

pub fn build_router(
    config: &Config,
    scheduler: SchedulerHandle,
    _event_bus: EventBus,
) -> (Router, String) {
    let signer = DownloadSigner::new(config.base_url());

    let allowed_dirs = config.allowed_dirs();

    let scheduler_for_mcp = scheduler.clone();
    let signer_for_mcp = signer.clone();
    let allowed_dirs_for_mcp = allowed_dirs.clone();

    // MCP service via rmcp streamable HTTP
    let mcp_service = StreamableHttpService::new(
        move || Ok(WartableTools::new(
            scheduler_for_mcp.clone(),
            signer_for_mcp.clone(),
            allowed_dirs_for_mcp.clone(),
        )),
        LocalSessionManager::default().into(),
        Default::default(),
    );

    let client_tracker: ClientTracker = Arc::new(RwLock::new(HashMap::new()));

    // Key store: always generates an admin key, optionally seeds from config
    let (key_store, admin_key) = if config.auth.enabled {
        KeyStore::new(config.auth.api_keys.clone())
    } else {
        if config.server.host == "0.0.0.0" || config.server.host == "::" {
            warn!("auth is DISABLED and server is bound to {} — all endpoints are publicly accessible", config.server.host);
        }
        KeyStore::new(vec![])
    };

    let api_state = ApiState {
        scheduler,
        allowed_dirs,
        signer,
        client_tracker: client_tracker.clone(),
        key_store: key_store.clone(),
    };

    // REST API for dashboard
    let api_router = Router::new()
        .route("/jobs", get(api::list_jobs))
        .route("/jobs/{id}", get(api::get_job))
        .route("/jobs/{id}/logs", get(api::get_job_logs))
        .route("/jobs/{id}/cancel", post(api::cancel_job))
        .route("/resources", get(api::get_resources))
        .route("/dl", get(api::get_download))
        .route("/clients", get(api::list_clients))
        .route("/keys", get(api::list_keys))
        .route("/keys/generate", post(api::generate_key))
        .route("/keys/revoke", post(api::revoke_key))
        .with_state(api_state);

    let auth_enabled = config.auth.enabled;

    // Protected routes (API + MCP) with optional auth
    let tracker_for_middleware = client_tracker;
    let protected = Router::new()
        .nest_service("/mcp", mcp_service)
        .nest("/api", api_router)
        .layer(middleware::from_fn(move |req, next| {
            let ks = key_store.clone();
            let tracker = tracker_for_middleware.clone();
            auth_middleware(auth_enabled, ks, tracker, req, next)
        }));

    // Static dashboard files - check multiple locations
    let dashboard_dir = config
        .dashboard
        .static_dir
        .as_ref()
        .map(PathBuf::from)
        .filter(|p| p.exists())
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|p| p.join("dashboard")))
                .filter(|p| p.exists())
        })
        .or_else(|| {
            let p = PathBuf::from("/opt/wartable/dashboard");
            p.exists().then_some(p)
        })
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("dashboard")
        });

    // Serve index.html with an HttpOnly session cookie containing the admin key.
    // The browser sends it automatically on every request — no JS key handling needed.
    let index_path = dashboard_dir.join("index.html");
    let index_html = std::fs::read_to_string(&index_path).unwrap_or_default();
    let session_cookie = format!(
        "wartable_session={}; HttpOnly; SameSite=Strict; Path=/",
        admin_key,
    );

    let router = Router::new()
        .merge(protected)
        .route("/", get({
            let html = index_html.clone();
            let cookie = session_cookie.clone();
            move || async move {
                (
                    [
                        (header::CONTENT_TYPE, "text/html".to_string()),
                        (header::SET_COOKIE, cookie),
                    ],
                    html,
                )
            }
        }))
        .route("/index.html", get({
            let html = index_html;
            let cookie = session_cookie;
            move || async move {
                (
                    [
                        (header::CONTENT_TYPE, "text/html".to_string()),
                        (header::SET_COOKIE, cookie),
                    ],
                    html,
                )
            }
        }))
        .fallback_service(ServeDir::new(dashboard_dir))
        .layer(CorsLayer::permissive());

    (router, admin_key)
}

async fn auth_middleware(
    auth_enabled: bool,
    key_store: KeyStore,
    tracker: ClientTracker,
    req: Request,
    next: Next,
) -> Response {
    if !auth_enabled {
        return next.run(req).await;
    }

    // Extract key from: Authorization header, X-API-Key header, or session cookie
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
        })
        .or_else(|| {
            // Parse wartable_session cookie
            req.headers()
                .get_all("cookie")
                .iter()
                .filter_map(|v| v.to_str().ok())
                .flat_map(|s| s.split(';'))
                .map(|s| s.trim())
                .find_map(|pair| {
                    pair.strip_prefix("wartable_session=")
                        .map(|v| v.to_string())
                })
        });

    match provided {
        Some(ref secret) => {
            match key_store.validate(secret).await {
                Some(name) => {
                    let mut clients = tracker.write().await;
                    let entry = clients.entry(name.clone()).or_insert_with(|| ClientInfo {
                        name,
                        last_seen: chrono::Utc::now(),
                        request_count: 0,
                    });
                    entry.last_seen = chrono::Utc::now();
                    entry.request_count += 1;
                    drop(clients);
                    next.run(req).await
                }
                None => unauthorized(),
            }
        }
        None => unauthorized(),
    }
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({ "error": "Invalid or missing API key" })),
    )
        .into_response()
}

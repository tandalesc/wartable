use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Json};
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use sysinfo::System;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use crate::download::DownloadSigner;
use crate::events::EventBus;
use crate::keys::KeyStore;
use crate::models::*;
use crate::scheduler::{JobFilter, LogStream, SchedulerHandle};
use crate::server::ClientTracker;

#[derive(Clone)]
pub struct ApiState {
    pub scheduler: SchedulerHandle,
    pub allowed_dirs: Vec<PathBuf>,
    pub signer: DownloadSigner,
    pub client_tracker: ClientTracker,
    pub key_store: KeyStore,
    pub event_bus: EventBus,
}

#[derive(Deserialize)]
pub struct ListJobsQuery {
    pub status: Option<String>,
    pub tag: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Deserialize)]
pub struct LogsQuery {
    pub stream: Option<String>,
    pub tail: Option<usize>,
    pub since_offset: Option<u64>,
}

pub async fn list_jobs(
    State(state): State<ApiState>,
    Query(q): Query<ListJobsQuery>,
) -> Json<Vec<JobInfo>> {
    let scheduler = &state.scheduler;
    let status = q.status.and_then(|s| match s.as_str() {
        "queued" => Some(JobStatus::Queued),
        "running" => Some(JobStatus::Running),
        "completed" => Some(JobStatus::Completed),
        "failed" => Some(JobStatus::Failed),
        "cancelled" => Some(JobStatus::Cancelled),
        _ => None,
    });

    let filter = JobFilter {
        status,
        tag: q.tag,
        limit: q.limit.unwrap_or(50),
    };

    Json(scheduler.query_jobs(filter).await)
}

pub async fn get_job(
    State(state): State<ApiState>,
    Path(job_id): Path<String>,
) -> Result<Json<Job>, StatusCode> {
    state.scheduler
        .get_job(job_id)
        .await
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

pub async fn get_job_logs(
    State(state): State<ApiState>,
    Path(job_id): Path<String>,
    Query(q): Query<LogsQuery>,
) -> Result<Json<JobLogs>, (StatusCode, String)> {
    let scheduler = &state.scheduler;
    let stream = match q.stream.as_deref() {
        Some("stdout") => LogStream::Stdout,
        Some("stderr") => LogStream::Stderr,
        _ => LogStream::Both,
    };

    scheduler
        .get_logs(job_id, stream, q.tail, q.since_offset)
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

pub async fn cancel_job(
    State(state): State<ApiState>,
    Path(job_id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    match state.scheduler.cancel_job(job_id.clone()).await {
        Ok((prev, new)) => Ok(Json(serde_json::json!({
            "job_id": job_id,
            "previous_status": prev,
            "new_status": new,
        }))),
        Err(e) => Err((StatusCode::NOT_FOUND, e.to_string())),
    }
}

pub async fn retry_job(
    State(state): State<ApiState>,
    Path(job_id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let job = state
        .scheduler
        .get_job(job_id.clone())
        .await
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Job not found: {}", job_id)))?;

    match job.status {
        JobStatus::Failed | JobStatus::Cancelled | JobStatus::Completed => {}
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("Cannot retry job in status: {}", job.status),
            ));
        }
    }

    let (new_job_id, position) = state.scheduler.submit_job(job.spec).await;
    Ok(Json(serde_json::json!({
        "original_job_id": job_id,
        "new_job_id": new_job_id,
        "status": "queued",
        "position_in_queue": position,
    })))
}

#[derive(Serialize)]
pub struct ResourceSnapshot {
    pub cpu: CpuInfo,
    pub ram: RamInfo,
    pub disk: DiskInfo,
    pub load: LoadInfo,
    pub gpus: Vec<GpuInfo>,
}

#[derive(Serialize)]
pub struct GpuInfo {
    pub index: u32,
    pub name: String,
    pub vram_total_gb: f64,
    pub vram_used_gb: f64,
    pub vram_free_gb: f64,
    pub gpu_utilization_pct: u32,
    pub memory_utilization_pct: u32,
    pub temperature_c: u32,
    pub power_draw_w: Option<f64>,
    pub power_limit_w: Option<f64>,
}

#[derive(Serialize)]
pub struct CpuInfo {
    pub cores: usize,
    pub usage_pct: f32,
    pub per_core_pct: Vec<f32>,
}

#[derive(Serialize)]
pub struct RamInfo {
    pub total_gb: f64,
    pub used_gb: f64,
    pub available_gb: f64,
    pub usage_pct: f64,
}

#[derive(Serialize)]
pub struct DiskInfo {
    pub total_gb: f64,
    pub used_gb: f64,
    pub free_gb: f64,
    pub usage_pct: f64,
}

#[derive(Serialize)]
pub struct LoadInfo {
    pub one: f64,
    pub five: f64,
    pub fifteen: f64,
}

pub async fn get_resources() -> Json<ResourceSnapshot> {
    // sysinfo needs a brief delay between refresh calls to measure CPU usage
    let snapshot = tokio::task::spawn_blocking(|| {
        let mut sys = System::new_all();
        // First refresh gets baseline, sleep, second refresh gets delta
        std::thread::sleep(std::time::Duration::from_millis(200));
        sys.refresh_all();

        let cpu = CpuInfo {
            cores: sys.cpus().len(),
            usage_pct: sys.global_cpu_usage(),
            per_core_pct: sys.cpus().iter().map(|c| c.cpu_usage()).collect(),
        };

        let total_mem = sys.total_memory() as f64;
        let used_mem = sys.used_memory() as f64;
        let ram = RamInfo {
            total_gb: total_mem / 1_073_741_824.0,
            used_gb: used_mem / 1_073_741_824.0,
            available_gb: (total_mem - used_mem) / 1_073_741_824.0,
            usage_pct: if total_mem > 0.0 { used_mem / total_mem * 100.0 } else { 0.0 },
        };

        let disks = sysinfo::Disks::new_with_refreshed_list();
        let mut total_disk: u64 = 0;
        let mut free_disk: u64 = 0;
        let mut seen_mounts = std::collections::HashSet::new();
        for d in disks.list() {
            let mount = d.mount_point().to_string_lossy().to_string();
            if seen_mounts.insert(mount) {
                total_disk += d.total_space();
                free_disk += d.available_space();
            }
        }
        let used_disk = total_disk.saturating_sub(free_disk);
        let disk = DiskInfo {
            total_gb: total_disk as f64 / 1_073_741_824.0,
            used_gb: used_disk as f64 / 1_073_741_824.0,
            free_gb: free_disk as f64 / 1_073_741_824.0,
            usage_pct: if total_disk > 0 { used_disk as f64 / total_disk as f64 * 100.0 } else { 0.0 },
        };

        let load = LoadInfo {
            one: System::load_average().one,
            five: System::load_average().five,
            fifteen: System::load_average().fifteen,
        };

        let gpus = get_gpu_info();

        ResourceSnapshot { cpu, ram, disk, load, gpus }
    }).await.unwrap();

    Json(snapshot)
}

#[derive(Deserialize)]
pub struct DownloadQuery {
    pub path: String,
    pub exp: u64,
    pub sig: String,
}

pub async fn get_download(
    State(state): State<ApiState>,
    Query(q): Query<DownloadQuery>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    // Verify presigned token
    state
        .signer
        .verify(&q.path, q.exp, &q.sig)
        .map_err(|e| (StatusCode::FORBIDDEN, e.to_string()))?;

    // Reject path traversal
    if q.path.contains("..") {
        return Err((StatusCode::BAD_REQUEST, "Path traversal not allowed".into()));
    }

    let canonical = tokio::fs::canonicalize(&q.path)
        .await
        .map_err(|_| (StatusCode::NOT_FOUND, format!("File not found: {}", q.path)))?;

    // Verify the file is under an allowed directory
    let allowed = state
        .allowed_dirs
        .iter()
        .any(|dir| canonical.starts_with(dir));
    if !allowed {
        return Err((StatusCode::FORBIDDEN, "Path outside allowed directories".into()));
    }

    let metadata = tokio::fs::metadata(&canonical)
        .await
        .map_err(|_| (StatusCode::NOT_FOUND, format!("File not found: {}", q.path)))?;
    if !metadata.is_file() {
        return Err((StatusCode::BAD_REQUEST, "Not a regular file".into()));
    }

    let content = tokio::fs::read(&canonical)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let mime = mime_guess::from_path(&canonical)
        .first_or_octet_stream()
        .to_string();

    let filename = canonical
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "download".into());

    Ok((
        [
            (header::CONTENT_TYPE, mime),
            (
                header::CONTENT_DISPOSITION,
                format!("inline; filename=\"{}\"", filename),
            ),
        ],
        content,
    ))
}

pub async fn list_clients(
    State(state): State<ApiState>,
) -> Json<Vec<crate::server::ClientInfo>> {
    let clients = state.client_tracker.read().await;
    let mut list: Vec<_> = clients.values().cloned().collect();
    list.sort_by(|a, b| b.last_seen.cmp(&a.last_seen));
    Json(list)
}

// ── Key Management ──

pub async fn list_keys(
    State(state): State<ApiState>,
) -> Json<Vec<crate::keys::ApiKeyInfo>> {
    Json(state.key_store.list().await)
}

#[derive(Deserialize)]
pub struct GenerateKeyRequest {
    pub name: String,
}

pub async fn generate_key(
    State(state): State<ApiState>,
    Json(req): Json<GenerateKeyRequest>,
) -> Json<serde_json::Value> {
    let key = state.key_store.generate(req.name).await;
    // Return the full secret — this is the only time it's shown
    Json(serde_json::json!({
        "name": key.name,
        "key": key.key,
        "created_at": key.created_at,
    }))
}

#[derive(Deserialize)]
pub struct RevokeKeyRequest {
    pub name: String,
}

pub async fn revoke_key(
    State(state): State<ApiState>,
    Json(req): Json<RevokeKeyRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    match state.key_store.revoke(&req.name).await {
        Ok(true) => Ok(Json(serde_json::json!({ "revoked": req.name }))),
        Ok(false) => Err((StatusCode::NOT_FOUND, format!("Key not found: {}", req.name))),
        Err(e) => Err((StatusCode::FORBIDDEN, e.to_string())),
    }
}

pub async fn event_stream(
    State(state): State<ApiState>,
) -> Sse<impl Stream<Item = Result<SseEvent, std::convert::Infallible>>> {
    let rx = state.event_bus.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|result| match result {
        Ok(event) => {
            let event_type = match &event {
                Event::JobSubmitted { .. } => "job_submitted",
                Event::JobStarted { .. } => "job_started",
                Event::JobCompleted { .. } => "job_completed",
                Event::JobCancelled { .. } => "job_cancelled",
            };
            let data = serde_json::to_string(&event).ok()?;
            Some(Ok(SseEvent::default().event(event_type).data(data)))
        }
        Err(_) => None,
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

fn get_gpu_info() -> Vec<GpuInfo> {
    let nvml = match nvml_wrapper::Nvml::init() {
        Ok(n) => n,
        Err(_) => return Vec::new(),
    };

    let count = match nvml.device_count() {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    (0..count)
        .filter_map(|i| {
            let dev = nvml.device_by_index(i).ok()?;
            let name = dev.name().unwrap_or_else(|_| "Unknown".into());
            let mem = dev.memory_info().ok()?;
            let util = dev.utilization_rates().ok();
            let temp = dev.temperature(nvml_wrapper::enum_wrappers::device::TemperatureSensor::Gpu).unwrap_or(0);
            let power_draw = dev.power_usage().ok().map(|mw| mw as f64 / 1000.0);
            let power_limit = dev.enforced_power_limit().ok().map(|mw| mw as f64 / 1000.0);

            Some(GpuInfo {
                index: i,
                name,
                vram_total_gb: mem.total as f64 / 1_073_741_824.0,
                vram_used_gb: mem.used as f64 / 1_073_741_824.0,
                vram_free_gb: mem.free as f64 / 1_073_741_824.0,
                gpu_utilization_pct: util.as_ref().map(|u| u.gpu).unwrap_or(0),
                memory_utilization_pct: util.as_ref().map(|u| u.memory).unwrap_or(0),
                temperature_c: temp,
                power_draw_w: power_draw,
                power_limit_w: power_limit,
            })
        })
        .collect()
}

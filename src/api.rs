use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::{Deserialize, Serialize};
use sysinfo::System;

use crate::models::*;
use crate::scheduler::{JobFilter, LogStream, SchedulerHandle};

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
    State(scheduler): State<SchedulerHandle>,
    Query(q): Query<ListJobsQuery>,
) -> Json<Vec<JobInfo>> {
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
    State(scheduler): State<SchedulerHandle>,
    Path(job_id): Path<String>,
) -> Result<Json<Job>, StatusCode> {
    scheduler
        .get_job(job_id)
        .await
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

pub async fn get_job_logs(
    State(scheduler): State<SchedulerHandle>,
    Path(job_id): Path<String>,
    Query(q): Query<LogsQuery>,
) -> Result<Json<JobLogs>, (StatusCode, String)> {
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
    State(scheduler): State<SchedulerHandle>,
    Path(job_id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    match scheduler.cancel_job(job_id.clone()).await {
        Ok((prev, new)) => Ok(Json(serde_json::json!({
            "job_id": job_id,
            "previous_status": prev,
            "new_status": new,
        }))),
        Err(e) => Err((StatusCode::NOT_FOUND, e.to_string())),
    }
}

#[derive(Serialize)]
pub struct ResourceSnapshot {
    pub cpu: CpuInfo,
    pub ram: RamInfo,
    pub disk: DiskInfo,
    pub load: LoadInfo,
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

        ResourceSnapshot { cpu, ram, disk, load }
    }).await.unwrap();

    Json(snapshot)
}

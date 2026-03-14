use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::Deserialize;

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

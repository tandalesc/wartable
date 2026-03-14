use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub type JobId = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl std::fmt::Display for JobStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JobStatus::Queued => write!(f, "queued"),
            JobStatus::Running => write!(f, "running"),
            JobStatus::Completed => write!(f, "completed"),
            JobStatus::Failed => write!(f, "failed"),
            JobStatus::Cancelled => write!(f, "cancelled"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceRequirements {
    #[serde(default)]
    pub gpu_count: u32,
    #[serde(default)]
    pub gpu_vram_min_gb: Option<f64>,
    #[serde(default)]
    pub cpu_cores: Option<u32>,
    #[serde(default)]
    pub ram_min_gb: Option<f64>,
    #[serde(default)]
    pub disk_min_gb: Option<f64>,
}

impl Default for ResourceRequirements {
    fn default() -> Self {
        Self {
            gpu_count: 0,
            gpu_vram_min_gb: None,
            cpu_cores: None,
            ram_min_gb: None,
            disk_min_gb: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FileUpload {
    pub name: String,
    pub content_base64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobSpec {
    pub command: String,
    #[serde(default)]
    pub working_dir: Option<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub resources: ResourceRequirements,
    #[serde(default)]
    pub files: Vec<FileUpload>,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: JobId,
    pub spec: JobSpec,
    pub status: JobStatus,
    pub submitted_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub exit_code: Option<i32>,
    pub pid: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobInfo {
    pub job_id: String,
    pub name: Option<String>,
    pub status: JobStatus,
    pub command: String,
    pub submitted_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub exit_code: Option<i32>,
    pub tags: Vec<String>,
}

impl From<&Job> for JobInfo {
    fn from(job: &Job) -> Self {
        let command = if job.spec.command.len() > 100 {
            format!("{}...", &job.spec.command[..100])
        } else {
            job.spec.command.clone()
        };
        JobInfo {
            job_id: job.id.clone(),
            name: job.spec.name.clone(),
            status: job.status,
            command,
            submitted_at: job.submitted_at,
            started_at: job.started_at,
            completed_at: job.completed_at,
            exit_code: job.exit_code,
            tags: job.spec.tags.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobLogs {
    pub stdout: String,
    pub stderr: String,
    pub stdout_offset: u64,
    pub stderr_offset: u64,
}

/// Events broadcast to dashboard clients
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Event {
    #[serde(rename = "job_submitted")]
    JobSubmitted { job: JobInfo },
    #[serde(rename = "job_started")]
    JobStarted { job: JobInfo },
    #[serde(rename = "job_completed")]
    JobCompleted { job: JobInfo },
    #[serde(rename = "job_cancelled")]
    JobCancelled { job: JobInfo },
}

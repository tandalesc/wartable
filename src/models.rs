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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub combined: Option<Vec<LogEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub combined_offset: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    /// "out" or "err"
    pub stream: String,
    pub line: String,
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::collections::HashMap;

    fn make_job(command: &str) -> Job {
        Job {
            id: "test-123".to_string(),
            spec: JobSpec {
                command: command.to_string(),
                working_dir: None,
                env: HashMap::new(),
                resources: ResourceRequirements::default(),
                files: Vec::new(),
                priority: 0,
                tags: vec!["ml".to_string(), "gpu".to_string()],
                name: Some("test job".to_string()),
            },
            status: JobStatus::Running,
            submitted_at: Utc::now(),
            started_at: Some(Utc::now()),
            completed_at: None,
            exit_code: None,
            pid: Some(1234),
        }
    }

    #[test]
    fn job_info_from_job() {
        let job = make_job("echo hello");
        let info = JobInfo::from(&job);

        assert_eq!(info.job_id, "test-123");
        assert_eq!(info.name, Some("test job".to_string()));
        assert_eq!(info.status, JobStatus::Running);
        assert_eq!(info.command, "echo hello");
        assert_eq!(info.tags, vec!["ml", "gpu"]);
        assert!(info.started_at.is_some());
        assert!(info.completed_at.is_none());
        assert!(info.exit_code.is_none());
    }

    #[test]
    fn job_info_truncates_long_command() {
        let long_cmd = "x".repeat(200);
        let job = make_job(&long_cmd);
        let info = JobInfo::from(&job);

        assert_eq!(info.command.len(), 103); // 100 chars + "..."
        assert!(info.command.ends_with("..."));
    }

    #[test]
    fn job_info_short_command_not_truncated() {
        let job = make_job("echo hi");
        let info = JobInfo::from(&job);
        assert_eq!(info.command, "echo hi");
    }

    #[test]
    fn job_status_display() {
        assert_eq!(JobStatus::Queued.to_string(), "queued");
        assert_eq!(JobStatus::Running.to_string(), "running");
        assert_eq!(JobStatus::Completed.to_string(), "completed");
        assert_eq!(JobStatus::Failed.to_string(), "failed");
        assert_eq!(JobStatus::Cancelled.to_string(), "cancelled");
    }

    #[test]
    fn job_status_serde_roundtrip() {
        for status in [
            JobStatus::Queued,
            JobStatus::Running,
            JobStatus::Completed,
            JobStatus::Failed,
            JobStatus::Cancelled,
        ] {
            let json = serde_json::to_string(&status).unwrap();
            let parsed: JobStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, status);
        }
    }

    #[test]
    fn job_status_serde_lowercase() {
        let json = serde_json::to_string(&JobStatus::Running).unwrap();
        assert_eq!(json, "\"running\"");
    }

    #[test]
    fn resource_requirements_default() {
        let r = ResourceRequirements::default();
        assert_eq!(r.gpu_count, 0);
        assert!(r.gpu_vram_min_gb.is_none());
        assert!(r.cpu_cores.is_none());
        assert!(r.ram_min_gb.is_none());
        assert!(r.disk_min_gb.is_none());
    }

    #[test]
    fn job_serde_roundtrip() {
        let job = make_job("python train.py");
        let json = serde_json::to_string(&job).unwrap();
        let parsed: Job = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, job.id);
        assert_eq!(parsed.spec.command, "python train.py");
        assert_eq!(parsed.status, JobStatus::Running);
        assert_eq!(parsed.pid, Some(1234));
    }

    #[test]
    fn event_serde_tagged() {
        let job = make_job("echo test");
        let event = Event::JobSubmitted {
            job: JobInfo::from(&job),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"job_submitted\""));
    }

    #[test]
    fn job_logs_skips_none_combined() {
        let logs = JobLogs {
            stdout: "hello\n".to_string(),
            stderr: String::new(),
            stdout_offset: 6,
            stderr_offset: 0,
            combined: None,
            combined_offset: None,
        };
        let json = serde_json::to_string(&logs).unwrap();
        assert!(!json.contains("combined"));
    }
}

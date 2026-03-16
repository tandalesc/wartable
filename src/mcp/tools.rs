use crate::models::*;
use crate::scheduler::{JobFilter, LogStream, SchedulerHandle};
use base64::Engine;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{Implementation, ServerCapabilities, ServerInfo};
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use std::collections::HashMap;

// --- Parameter structs ---

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SubmitJobParams {
    /// Bash command or script to execute
    command: String,
    /// Working directory on the server
    working_dir: Option<String>,
    /// Additional environment variables
    env: Option<HashMap<String, String>>,
    /// Number of GPUs needed (default: 0)
    gpu_count: Option<u32>,
    /// Minimum VRAM per GPU in GB
    gpu_vram_min_gb: Option<f64>,
    /// CPU cores to reserve
    cpu_cores: Option<u32>,
    /// Minimum RAM in GB
    ram_min_gb: Option<f64>,
    /// Minimum free disk in GB
    disk_min_gb: Option<f64>,
    /// Files to write to working_dir before execution. Each with 'name' and 'content_base64'.
    files: Option<Vec<FileUpload>>,
    /// Priority: higher = sooner (default: 0)
    priority: Option<i32>,
    /// Labels for filtering/grouping
    tags: Option<Vec<String>>,
    /// Human-readable job name
    name: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListJobsParams {
    /// Filter by status: "queued", "running", "completed", "failed", "cancelled", "all"
    status: Option<String>,
    /// Filter by tag
    tag: Option<String>,
    /// Max results (default: 50)
    limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetJobStatusParams {
    /// The job ID
    job_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetJobLogsParams {
    /// The job ID
    job_id: String,
    /// "stdout", "stderr", or "both" (default: "both")
    stream: Option<String>,
    /// Last N lines only
    tail: Option<usize>,
    /// Byte offset for incremental polling
    since_offset: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CancelJobParams {
    /// The job ID to cancel
    job_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UploadFileParams {
    /// Destination path on the server
    path: String,
    /// Base64-encoded file content
    content_base64: String,
    /// File permissions (default: "0644")
    mode: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DownloadFileParams {
    /// Path to the file on the server
    path: String,
}

// --- MCP Tool Service ---

#[derive(Debug, Clone)]
pub struct WartableTools {
    scheduler: SchedulerHandle,
    tool_router: ToolRouter<Self>,
}

impl WartableTools {
    pub fn new(scheduler: SchedulerHandle) -> Self {
        Self {
            scheduler,
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for WartableTools {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("wartable", env!("CARGO_PKG_VERSION")))
            .with_instructions("wartable: GPU homelab job scheduler. Submit bash jobs, monitor status, tail logs, and query resource availability.")
    }
}

#[tool_router]
impl WartableTools {
    #[tool(description = "Submit a job to the wartable work queue. Returns job_id, status, and queue position.")]
    async fn submit_job(
        &self,
        Parameters(params): Parameters<SubmitJobParams>,
    ) -> String {
        let spec = JobSpec {
            command: params.command,
            working_dir: params.working_dir,
            env: params.env.unwrap_or_default(),
            resources: ResourceRequirements {
                gpu_count: params.gpu_count.unwrap_or(0),
                gpu_vram_min_gb: params.gpu_vram_min_gb,
                cpu_cores: params.cpu_cores,
                ram_min_gb: params.ram_min_gb,
                disk_min_gb: params.disk_min_gb,
            },
            files: params.files.unwrap_or_default(),
            priority: params.priority.unwrap_or(0),
            tags: params.tags.unwrap_or_default(),
            name: params.name,
        };

        let (job_id, position) = self.scheduler.submit_job(spec).await;

        serde_json::json!({
            "job_id": job_id,
            "status": "queued",
            "position_in_queue": position,
        })
        .to_string()
    }

    #[tool(description = "List jobs in the wartable queue. Filter by status and tag.")]
    async fn list_jobs(
        &self,
        Parameters(params): Parameters<ListJobsParams>,
    ) -> String {
        let status = params.status.and_then(|s| match s.as_str() {
            "queued" => Some(JobStatus::Queued),
            "running" => Some(JobStatus::Running),
            "completed" => Some(JobStatus::Completed),
            "failed" => Some(JobStatus::Failed),
            "cancelled" => Some(JobStatus::Cancelled),
            _ => None,
        });

        let filter = JobFilter {
            status,
            tag: params.tag,
            limit: params.limit.unwrap_or(50),
        };

        let jobs = self.scheduler.query_jobs(filter).await;
        serde_json::to_string_pretty(&jobs).unwrap()
    }

    #[tool(description = "Get detailed status for a specific job.")]
    async fn get_job_status(
        &self,
        Parameters(params): Parameters<GetJobStatusParams>,
    ) -> String {
        match self.scheduler.get_job(params.job_id.clone()).await {
            Some(job) => serde_json::to_string_pretty(&job).unwrap(),
            None => format!("{{\"error\": \"Job not found: {}\"}}", params.job_id),
        }
    }

    #[tool(description = "Get logs (stdout/stderr) for a job. Supports incremental polling via since_offset.")]
    async fn get_job_logs(
        &self,
        Parameters(params): Parameters<GetJobLogsParams>,
    ) -> String {
        let stream = match params.stream.as_deref() {
            Some("stdout") => LogStream::Stdout,
            Some("stderr") => LogStream::Stderr,
            _ => LogStream::Both,
        };

        match self
            .scheduler
            .get_logs(params.job_id.clone(), stream, params.tail, params.since_offset)
            .await
        {
            Ok(logs) => serde_json::to_string_pretty(&logs).unwrap(),
            Err(e) => format!("{{\"error\": \"{}\"}}", e),
        }
    }

    #[tool(description = "Cancel a queued or running job. Running jobs receive SIGTERM then SIGKILL after grace period.")]
    async fn cancel_job(
        &self,
        Parameters(params): Parameters<CancelJobParams>,
    ) -> String {
        match self.scheduler.cancel_job(params.job_id.clone()).await {
            Ok((prev, new)) => serde_json::json!({
                "job_id": params.job_id,
                "previous_status": prev,
                "new_status": new,
            })
            .to_string(),
            Err(e) => format!("{{\"error\": \"{}\"}}", e),
        }
    }

    #[tool(description = "Upload a file to the server. Content must be base64-encoded.")]
    async fn upload_file(
        &self,
        Parameters(params): Parameters<UploadFileParams>,
    ) -> String {
        let path = std::path::Path::new(&params.path);
        if let Some(parent) = path.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                return format!("{{\"error\": \"Failed to create directory: {}\"}}", e);
            }
        }

        let content = match base64::engine::general_purpose::STANDARD.decode(&params.content_base64) {
            Ok(c) => c,
            Err(e) => return format!("{{\"error\": \"Invalid base64: {}\"}}", e),
        };

        let size = content.len();

        #[cfg(unix)]
        if let Some(mode_str) = &params.mode {
            if let Ok(mode) = u32::from_str_radix(mode_str, 8) {
                use std::os::unix::fs::PermissionsExt;
                if let Err(e) = tokio::fs::write(path, &content).await {
                    return format!("{{\"error\": \"Failed to write file: {}\"}}", e);
                }
                let _ = tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).await;
                return serde_json::json!({
                    "path": params.path,
                    "size_bytes": size,
                }).to_string();
            }
        }

        match tokio::fs::write(path, &content).await {
            Ok(()) => serde_json::json!({
                "path": params.path,
                "size_bytes": size,
            }).to_string(),
            Err(e) => format!("{{\"error\": \"Failed to write file: {}\"}}", e),
        }
    }

    #[tool(description = "Download a file from the server. Returns base64-encoded content and a download_url for HTTP access.")]
    async fn download_file(
        &self,
        Parameters(params): Parameters<DownloadFileParams>,
    ) -> String {
        match tokio::fs::read(&params.path).await {
            Ok(content) => {
                let encoded = base64::engine::general_purpose::STANDARD.encode(&content);
                let download_url = format!("/api/files/{}", params.path.trim_start_matches('/'));
                serde_json::json!({
                    "path": params.path,
                    "size_bytes": content.len(),
                    "content_base64": encoded,
                    "download_url": download_url,
                }).to_string()
            }
            Err(e) => format!("{{\"error\": \"Failed to read file: {}\"}}", e),
        }
    }
}

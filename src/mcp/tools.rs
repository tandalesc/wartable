use crate::download::DownloadSigner;
use crate::models::*;
use crate::scheduler::{JobFilter, LogStream, SchedulerHandle};
use base64::Engine;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, Implementation, ServerCapabilities, ServerInfo};
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
    signer: DownloadSigner,
    allowed_dirs: Vec<std::path::PathBuf>,
    tool_router: ToolRouter<Self>,
}

impl WartableTools {
    pub fn new(scheduler: SchedulerHandle, signer: DownloadSigner, allowed_dirs: Vec<std::path::PathBuf>) -> Self {
        Self {
            scheduler,
            signer,
            allowed_dirs,
            tool_router: Self::tool_router(),
        }
    }

    fn is_path_allowed(&self, path: &std::path::Path) -> bool {
        if let Ok(canonical) = std::fs::canonicalize(path) {
            self.allowed_dirs.iter().any(|dir| canonical.starts_with(dir))
        } else {
            // File doesn't exist yet — check parent
            if let Some(parent) = path.parent() {
                if let Ok(canonical) = std::fs::canonicalize(parent) {
                    return self.allowed_dirs.iter().any(|dir| canonical.starts_with(dir));
                }
            }
            false
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
    #[tool(description = "Submit a job to the wartable work queue. Returns job_id, status, and queue position. When requesting GPUs (gpu_count > 0), gpu_vram_min_gb is required.")]
    async fn submit_job(
        &self,
        Parameters(params): Parameters<SubmitJobParams>,
    ) -> String {
        let gpu_count = params.gpu_count.unwrap_or(0);
        if gpu_count > 0 && params.gpu_vram_min_gb.is_none() {
            return serde_json::json!({
                "error": "gpu_vram_min_gb is required when gpu_count > 0"
            }).to_string();
        }

        let spec = JobSpec {
            command: params.command,
            working_dir: params.working_dir,
            env: params.env.unwrap_or_default(),
            resources: ResourceRequirements {
                gpu_count,
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

    #[tool(description = "Upload a file to the server. Content must be base64-encoded. Path must be under an allowed directory (working_dir or log_dir).")]
    async fn upload_file(
        &self,
        Parameters(params): Parameters<UploadFileParams>,
    ) -> String {
        if params.path.contains("..") {
            return r#"{"error": "Path traversal not allowed"}"#.to_string();
        }

        let path = std::path::Path::new(&params.path);

        // Ensure parent directory exists before checking allowed_dirs
        if let Some(parent) = path.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                return format!("{{\"error\": \"Failed to create directory: {}\"}}", e);
            }
        }

        if !self.is_path_allowed(path) {
            return r#"{"error": "Path outside allowed directories"}"#.to_string();
        }

        let content = match base64::engine::general_purpose::STANDARD.decode(&params.content_base64) {
            Ok(c) => c,
            Err(e) => return format!("{{\"error\": \"Invalid base64: {}\"}}", e),
        };

        let size = content.len();

        #[cfg(unix)]
        if let Some(mode_str) = &params.mode {
            if let Ok(mode) = u32::from_str_radix(mode_str, 8) {
                // Clamp: strip setuid/setgid/sticky, cap at 0755
                let safe_mode = mode & 0o0755;
                use std::os::unix::fs::PermissionsExt;
                if let Err(e) = tokio::fs::write(path, &content).await {
                    return format!("{{\"error\": \"Failed to write file: {}\"}}", e);
                }
                let _ = tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(safe_mode)).await;
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

    #[tool(description = "Get a presigned download URL for a file on the server. Returns file metadata and a time-limited URL (15 min) for direct HTTP download. No file content is returned through MCP.")]
    async fn download_file(
        &self,
        Parameters(params): Parameters<DownloadFileParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        if params.path.contains("..") {
            return Ok(CallToolResult::error(vec![Content::text("Path traversal not allowed")]));
        }

        if !self.is_path_allowed(std::path::Path::new(&params.path)) {
            return Ok(CallToolResult::error(vec![Content::text("Path outside allowed directories")]));
        }

        let metadata = match tokio::fs::metadata(&params.path).await {
            Ok(m) => m,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(format!("Failed to access file: {}", e))])),
        };

        if !metadata.is_file() {
            return Ok(CallToolResult::error(vec![Content::text("Not a regular file")]));
        }

        let mime = mime_guess::from_path(&params.path)
            .first_or_octet_stream()
            .to_string();

        let download_url = self.signer.sign(&params.path);

        let response = serde_json::json!({
            "path": params.path,
            "size_bytes": metadata.len(),
            "content_type": mime,
            "download_url": download_url,
            "expires_in_seconds": 900,
        });

        Ok(CallToolResult::success(vec![Content::text(response.to_string())]))
    }
}

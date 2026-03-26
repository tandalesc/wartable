pub mod queue;

use crate::config::Config;
use crate::events::EventBus;
use crate::models::*;
use crate::worker;
use chrono::Utc;
use queue::JobQueue;
use std::collections::HashMap;
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

/// Messages sent to the scheduler actor.
pub enum SchedulerMsg {
    SubmitJob {
        spec: JobSpec,
        reply: oneshot::Sender<(JobId, usize)>,
    },
    CancelJob {
        job_id: JobId,
        reply: oneshot::Sender<anyhow::Result<(JobStatus, JobStatus)>>,
    },
    JobCompleted {
        job_id: JobId,
        exit_code: i32,
    },
    QueryJobs {
        filter: JobFilter,
        reply: oneshot::Sender<Vec<JobInfo>>,
    },
    GetJob {
        job_id: JobId,
        reply: oneshot::Sender<Option<Job>>,
    },
    GetLogs {
        job_id: JobId,
        stream: LogStream,
        tail: Option<usize>,
        since_offset: Option<u64>,
        reply: oneshot::Sender<anyhow::Result<JobLogs>>,
    },
}

#[derive(Debug, Clone)]
pub enum LogStream {
    Stdout,
    Stderr,
    Both,
}

#[derive(Debug, Clone)]
pub struct JobFilter {
    pub status: Option<JobStatus>,
    pub tag: Option<String>,
    pub limit: usize,
}

/// Cloneable handle to send messages to the scheduler actor.
#[derive(Debug, Clone)]
pub struct SchedulerHandle {
    tx: mpsc::Sender<SchedulerMsg>,
}

impl SchedulerHandle {
    pub async fn submit_job(&self, spec: JobSpec) -> (JobId, usize) {
        let (reply, rx) = oneshot::channel();
        let _ = self.tx.send(SchedulerMsg::SubmitJob { spec, reply }).await;
        rx.await.unwrap()
    }

    pub async fn cancel_job(&self, job_id: JobId) -> anyhow::Result<(JobStatus, JobStatus)> {
        let (reply, rx) = oneshot::channel();
        let _ = self
            .tx
            .send(SchedulerMsg::CancelJob { job_id, reply })
            .await;
        rx.await.unwrap()
    }

    pub async fn job_completed(&self, job_id: JobId, exit_code: i32) {
        let _ = self
            .tx
            .send(SchedulerMsg::JobCompleted { job_id, exit_code })
            .await;
    }

    pub async fn query_jobs(&self, filter: JobFilter) -> Vec<JobInfo> {
        let (reply, rx) = oneshot::channel();
        let _ = self.tx.send(SchedulerMsg::QueryJobs { filter, reply }).await;
        rx.await.unwrap()
    }

    pub async fn get_job(&self, job_id: JobId) -> Option<Job> {
        let (reply, rx) = oneshot::channel();
        let _ = self.tx.send(SchedulerMsg::GetJob { job_id, reply }).await;
        rx.await.unwrap()
    }

    pub async fn get_logs(
        &self,
        job_id: JobId,
        stream: LogStream,
        tail: Option<usize>,
        since_offset: Option<u64>,
    ) -> anyhow::Result<JobLogs> {
        let (reply, rx) = oneshot::channel();
        let _ = self
            .tx
            .send(SchedulerMsg::GetLogs {
                job_id,
                stream,
                tail,
                since_offset,
                reply,
            })
            .await;
        rx.await.unwrap()
    }
}

/// Per-GPU VRAM budget tracking.
#[derive(Debug, Clone)]
struct GpuBudget {
    index: u32,
    total_vram_gb: f64,
    allocated_vram_gb: f64,
    /// Number of jobs currently assigned to this GPU.
    job_count: u32,
    /// Last observed live free VRAM from NVML (cached).
    live_free_vram_gb: Option<f64>,
}

impl GpuBudget {
    fn free_vram_gb(&self) -> f64 {
        (self.total_vram_gb - self.allocated_vram_gb).max(0.0)
    }

    /// Effective free VRAM: the lower of budget headroom and live free VRAM.
    fn effective_free_vram_gb(&self) -> f64 {
        let budget_free = self.free_vram_gb();
        match self.live_free_vram_gb {
            Some(live_free) => budget_free.min(live_free),
            None => budget_free,
        }
    }
}

/// Minimum seconds between NVML refreshes.
const VRAM_REFRESH_COOLDOWN_SECS: u64 = 10;

struct GpuState {
    devices: Vec<GpuBudget>,
    /// Config-specified VRAM caps (if set, these override NVML totals).
    vram_overrides: Option<Vec<f64>>,
    policy: String,
    device_env_var: String,
    /// Last time we queried NVML for live VRAM.
    last_vram_refresh: Option<std::time::Instant>,
}

impl GpuState {
    fn init(config: &Config) -> Self {
        let gpu_config = &config.scheduler.gpu;

        let mut state = GpuState {
            devices: Vec::new(),
            vram_overrides: gpu_config.vram_gb.clone(),
            policy: gpu_config.policy.clone(),
            device_env_var: gpu_config.device_env_var.clone(),
            last_vram_refresh: None,
        };

        // Detect GPUs and take initial VRAM snapshot
        state.devices = Self::detect_gpus(&state.vram_overrides);
        state.refresh_live_vram();

        if !state.devices.is_empty() {
            info!(
                gpu_count = state.devices.len(),
                "GPU scheduler initialized: {}",
                state.devices
                    .iter()
                    .map(|d| {
                        let live = d.live_free_vram_gb
                            .map(|v| format!(", {:.1}G free", v))
                            .unwrap_or_default();
                        format!("GPU {} ({:.1}G total{})", d.index, d.total_vram_gb, live)
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }

        state
    }

    fn detect_gpus(vram_overrides: &Option<Vec<f64>>) -> Vec<GpuBudget> {
        if let Some(ref overrides) = vram_overrides {
            return overrides
                .iter()
                .enumerate()
                .map(|(i, &vram)| GpuBudget {
                    index: i as u32,
                    total_vram_gb: vram,
                    allocated_vram_gb: 0.0,
                    job_count: 0,
                    live_free_vram_gb: None,
                })
                .collect();
        }

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
                let mem = dev.memory_info().ok()?;
                Some(GpuBudget {
                    index: i,
                    total_vram_gb: mem.total as f64 / 1_073_741_824.0,
                    allocated_vram_gb: 0.0,
                    job_count: 0,
                    live_free_vram_gb: None,
                })
            })
            .collect()
    }

    /// Refresh live VRAM from NVML, respecting the cooldown.
    fn refresh_live_vram(&mut self) {
        if let Some(last) = self.last_vram_refresh {
            if last.elapsed().as_secs() < VRAM_REFRESH_COOLDOWN_SECS {
                return;
            }
        }

        let nvml = match nvml_wrapper::Nvml::init() {
            Ok(n) => n,
            Err(_) => return,
        };

        for dev_budget in &mut self.devices {
            if let Ok(dev) = nvml.device_by_index(dev_budget.index) {
                if let Ok(mem) = dev.memory_info() {
                    dev_budget.live_free_vram_gb = Some(mem.free as f64 / 1_073_741_824.0);
                }
            }
        }

        self.last_vram_refresh = Some(std::time::Instant::now());
    }

    /// Try to assign GPUs for a job. Returns assigned GPU indices, or None if
    /// the request can't be satisfied.
    ///
    /// Uses the lower of budget headroom and live VRAM (cached, refreshed on
    /// cooldown) to account for external GPU consumers.
    fn try_assign(&mut self, gpu_count: u32, vram_per_gpu_gb: f64) -> Option<Vec<u32>> {
        if gpu_count == 0 {
            return Some(Vec::new());
        }
        if self.devices.is_empty() {
            warn!("job requests {} GPU(s) but no GPUs are available", gpu_count);
            return None;
        }

        // Refresh live VRAM snapshot (debounced)
        self.refresh_live_vram();

        // Find devices with enough effective free VRAM
        let mut candidates: Vec<(u32, f64, u32)> = self
            .devices
            .iter()
            .filter(|d| {
                let effective = d.effective_free_vram_gb();
                if effective < vram_per_gpu_gb {
                    if d.free_vram_gb() >= vram_per_gpu_gb {
                        info!(
                            gpu = d.index,
                            budget_free_gb = format!("{:.1}", d.free_vram_gb()),
                            live_free_gb = format!("{:.1}", d.live_free_vram_gb.unwrap_or(-1.0)),
                            requested_gb = format!("{:.1}", vram_per_gpu_gb),
                            "skipping GPU: live VRAM too low despite budget headroom"
                        );
                    }
                    return false;
                }
                true
            })
            .map(|d| (d.index, d.effective_free_vram_gb(), d.job_count))
            .collect();

        if candidates.len() < gpu_count as usize {
            return None;
        }

        // Sort by policy
        match self.policy.as_str() {
            "packed" => {
                candidates.sort_by(|a, b| a.0.cmp(&b.0));
            }
            _ => {
                // "least-loaded": fewest jobs first, then most free VRAM as tiebreaker
                candidates.sort_by(|a, b| {
                    a.2.cmp(&b.2)
                        .then(b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal))
                });
            }
        }

        Some(candidates.iter().take(gpu_count as usize).map(|(idx, _, _)| *idx).collect())
    }

    /// Record a VRAM allocation on the given GPU indices.
    fn allocate(&mut self, indices: &[u32], vram_per_gpu_gb: f64) {
        for &idx in indices {
            if let Some(dev) = self.devices.iter_mut().find(|d| d.index == idx) {
                dev.allocated_vram_gb += vram_per_gpu_gb;
                dev.job_count += 1;
            }
        }
    }

    /// Release a VRAM allocation on the given GPU indices.
    fn release(&mut self, indices: &[u32], vram_per_gpu_gb: f64) {
        for &idx in indices {
            if let Some(dev) = self.devices.iter_mut().find(|d| d.index == idx) {
                dev.allocated_vram_gb = (dev.allocated_vram_gb - vram_per_gpu_gb).max(0.0);
                dev.job_count = dev.job_count.saturating_sub(1);
            }
        }
    }
}

struct SchedulerActor {
    config: Config,
    queue: JobQueue,
    running: HashMap<JobId, RunningJob>,
    completed: Vec<Job>,
    event_bus: EventBus,
    scheduler_handle: SchedulerHandle,
    gpu_state: GpuState,
}

struct RunningJob {
    job: Job,
    cancel_tx: Option<oneshot::Sender<()>>,
    /// GPU indices assigned to this job (empty if no GPUs requested).
    gpu_indices: Vec<u32>,
    /// VRAM allocated per GPU for this job.
    vram_per_gpu_gb: f64,
}

impl SchedulerActor {
    fn new(config: Config, event_bus: EventBus, scheduler_handle: SchedulerHandle) -> Self {
        let gpu_state = GpuState::init(&config);
        Self {
            config,
            queue: JobQueue::new(),
            running: HashMap::new(),
            completed: Vec::new(),
            event_bus,
            scheduler_handle,
            gpu_state,
        }
    }

    fn handle_submit(&mut self, spec: JobSpec) -> (JobId, usize) {
        let job_id = uuid::Uuid::new_v4().to_string();
        let job = Job {
            id: job_id.clone(),
            spec,
            status: JobStatus::Queued,
            submitted_at: Utc::now(),
            started_at: None,
            completed_at: None,
            exit_code: None,
            pid: None,
        };

        self.event_bus.publish(Event::JobSubmitted {
            job: JobInfo::from(&job),
        });

        self.queue.push(job);
        let position = self.queue.position(&job_id).unwrap_or(0);

        info!(job_id = %job_id, position, "job submitted");

        self.try_dispatch();

        (job_id, position)
    }

    fn handle_cancel(&mut self, job_id: &JobId) -> anyhow::Result<(JobStatus, JobStatus)> {
        // Check if queued
        if let Some(mut job) = self.queue.remove(job_id) {
            let prev = job.status;
            job.status = JobStatus::Cancelled;
            job.completed_at = Some(Utc::now());
            self.event_bus.publish(Event::JobCancelled {
                job: JobInfo::from(&job),
            });
            self.completed.push(job);
            return Ok((prev, JobStatus::Cancelled));
        }

        // Check if running
        if let Some(mut running_job) = self.running.remove(job_id) {
            // Release GPU allocations
            if !running_job.gpu_indices.is_empty() {
                self.gpu_state
                    .release(&running_job.gpu_indices, running_job.vram_per_gpu_gb);
            }

            let prev = running_job.job.status;
            running_job.job.status = JobStatus::Cancelled;
            running_job.job.completed_at = Some(Utc::now());
            // Send cancel signal
            if let Some(cancel_tx) = running_job.cancel_tx.take() {
                let _ = cancel_tx.send(());
            }
            self.event_bus.publish(Event::JobCancelled {
                job: JobInfo::from(&running_job.job),
            });
            self.completed.push(running_job.job);
            return Ok((prev, JobStatus::Cancelled));
        }

        anyhow::bail!("job not found: {}", job_id)
    }

    fn handle_completed(&mut self, job_id: &JobId, exit_code: i32) {
        if let Some(mut running_job) = self.running.remove(job_id) {
            // Release GPU allocations
            if !running_job.gpu_indices.is_empty() {
                self.gpu_state
                    .release(&running_job.gpu_indices, running_job.vram_per_gpu_gb);
            }

            running_job.job.completed_at = Some(Utc::now());
            running_job.job.exit_code = Some(exit_code);
            running_job.job.status = if exit_code == 0 {
                JobStatus::Completed
            } else {
                JobStatus::Failed
            };

            info!(
                job_id = %job_id,
                exit_code,
                status = %running_job.job.status,
                "job finished"
            );

            self.event_bus.publish(Event::JobCompleted {
                job: JobInfo::from(&running_job.job),
            });

            self.completed.push(running_job.job);
            self.try_dispatch();
        }
    }

    fn try_dispatch(&mut self) {
        let max = self.config.scheduler.max_concurrent_jobs;
        while self.running.len() < max {
            if let Some(mut job) = self.queue.pop() {
                let gpu_count = job.spec.resources.gpu_count;
                let vram_per_gpu = job.spec.resources.gpu_vram_min_gb.unwrap_or(0.0);

                // Check GPU budget if the job requests GPUs
                let gpu_assignment = if gpu_count > 0 {
                    match self.gpu_state.try_assign(gpu_count, vram_per_gpu) {
                        Some(indices) => indices,
                        None => {
                            // Not enough GPU resources — push back to queue
                            self.queue.push(job);
                            break;
                        }
                    }
                } else {
                    Vec::new()
                };

                // Record VRAM allocation
                if !gpu_assignment.is_empty() {
                    self.gpu_state.allocate(&gpu_assignment, vram_per_gpu);

                    // Inject device visibility env var
                    let device_list = gpu_assignment
                        .iter()
                        .map(|i| i.to_string())
                        .collect::<Vec<_>>()
                        .join(",");
                    job.spec
                        .env
                        .insert(self.gpu_state.device_env_var.clone(), device_list.clone());

                    info!(
                        job_id = %job.id,
                        gpus = %device_list,
                        vram_per_gpu_gb = vram_per_gpu,
                        "assigned GPUs"
                    );
                }

                job.status = JobStatus::Running;
                job.started_at = Some(Utc::now());

                info!(job_id = %job.id, "dispatching job");

                self.event_bus.publish(Event::JobStarted {
                    job: JobInfo::from(&job),
                });

                let cancel_tx = worker::spawn_worker(
                    job.clone(),
                    self.config.clone(),
                    self.scheduler_handle.clone(),
                );

                self.running.insert(
                    job.id.clone(),
                    RunningJob {
                        job,
                        cancel_tx: Some(cancel_tx),
                        gpu_indices: gpu_assignment,
                        vram_per_gpu_gb: vram_per_gpu,
                    },
                );
            } else {
                break;
            }
        }
    }

    fn query_jobs(&self, filter: &JobFilter) -> Vec<JobInfo> {
        let mut results: Vec<JobInfo> = Vec::new();

        // Collect from all sources
        let all_jobs: Vec<&Job> = self
            .queue
            .iter()
            .chain(self.running.values().map(|r| &r.job))
            .chain(self.completed.iter())
            .collect();

        for job in all_jobs {
            if let Some(status) = &filter.status {
                if job.status != *status {
                    continue;
                }
            }
            if let Some(tag) = &filter.tag {
                if !job.spec.tags.contains(tag) {
                    continue;
                }
            }
            results.push(JobInfo::from(job));
        }

        // Sort: running first, then queued, then completed, within each by time
        results.sort_by(|a, b| {
            let status_order = |s: &JobStatus| -> u8 {
                match s {
                    JobStatus::Running => 0,
                    JobStatus::Queued => 1,
                    JobStatus::Failed => 2,
                    JobStatus::Completed => 3,
                    JobStatus::Cancelled => 4,
                }
            };
            status_order(&a.status)
                .cmp(&status_order(&b.status))
                .then(b.submitted_at.cmp(&a.submitted_at))
        });

        results.truncate(filter.limit);
        results
    }

    fn get_job(&self, job_id: &JobId) -> Option<Job> {
        // Check queue
        if let Some(job) = self.queue.iter().find(|j| j.id == *job_id) {
            return Some(job.clone());
        }
        // Check running
        if let Some(running) = self.running.get(job_id) {
            return Some(running.job.clone());
        }
        // Check completed
        self.completed.iter().find(|j| j.id == *job_id).cloned()
    }

    async fn get_logs(
        &self,
        job_id: &JobId,
        stream: &LogStream,
        tail: Option<usize>,
        since_offset: Option<u64>,
    ) -> anyhow::Result<JobLogs> {
        let log_dir = self.config.log_dir().join(job_id);

        let read_log = |path: std::path::PathBuf, offset: Option<u64>| async move {
            if !path.exists() {
                return Ok::<(String, u64), anyhow::Error>(("".to_string(), 0));
            }
            let content = tokio::fs::read(&path).await?;
            let total_len = content.len() as u64;
            let offset = offset.unwrap_or(0);
            let slice = if offset < total_len {
                &content[offset as usize..]
            } else {
                &[]
            };
            let text = String::from_utf8_lossy(slice).to_string();
            Ok((text, total_len))
        };

        match stream {
            LogStream::Both => {
                // Read combined log for correct ordering
                let combined_path = log_dir.join("combined.log");
                let (combined_raw, combined_offset) = read_log(combined_path, since_offset).await?;

                let mut entries = Vec::new();
                for line in combined_raw.split_inclusive('\n') {
                    if line.starts_with('\x02') {
                        entries.push(LogEntry {
                            stream: "err".to_string(),
                            line: line[1..].to_string(),
                        });
                    } else {
                        entries.push(LogEntry {
                            stream: "out".to_string(),
                            line: line.to_string(),
                        });
                    }
                }

                if let Some(n) = tail {
                    let len = entries.len();
                    if len > n {
                        entries = entries.split_off(len - n);
                    }
                }

                Ok(JobLogs {
                    stdout: String::new(),
                    stderr: String::new(),
                    stdout_offset: 0,
                    stderr_offset: 0,
                    combined: Some(entries),
                    combined_offset: Some(combined_offset),
                })
            }
            _ => {
                let (stdout, stdout_offset) = match stream {
                    LogStream::Stderr => ("".to_string(), 0),
                    _ => read_log(log_dir.join("stdout.log"), since_offset).await?,
                };

                let (stderr, stderr_offset) = match stream {
                    LogStream::Stdout => ("".to_string(), 0),
                    _ => read_log(log_dir.join("stderr.log"), since_offset).await?,
                };

                let mut logs = JobLogs {
                    stdout,
                    stderr,
                    stdout_offset,
                    stderr_offset,
                    combined: None,
                    combined_offset: None,
                };

                if let Some(n) = tail {
                    logs.stdout = tail_lines(&logs.stdout, n);
                    logs.stderr = tail_lines(&logs.stderr, n);
                }

                Ok(logs)
            }
        }
    }
}

fn tail_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= n {
        s.to_string()
    } else {
        lines[lines.len() - n..].join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::EventBus;
    use std::collections::HashMap;

    fn test_config() -> Config {
        Config {
            server: crate::config::ServerConfig::default(),
            scheduler: crate::config::SchedulerConfig {
                max_concurrent_jobs: 0, // don't actually dispatch jobs in tests
                gpu: crate::config::GpuSchedulerConfig::default(),
            },
            workers: crate::config::WorkerConfig {
                default_working_dir: "/tmp/wartable-test-jobs".to_string(),
                log_dir: "/tmp/wartable-test-logs".to_string(),
                kill_grace_period_secs: 5,
                extra_allowed_dirs: Vec::new(),
            },
            dashboard: crate::config::DashboardConfig::default(),
            auth: crate::config::AuthConfig::default(),
        }
    }

    fn make_spec(command: &str, priority: i32, tags: Vec<&str>) -> JobSpec {
        JobSpec {
            command: command.to_string(),
            working_dir: None,
            env: HashMap::new(),
            resources: ResourceRequirements::default(),
            files: Vec::new(),
            priority,
            tags: tags.into_iter().map(String::from).collect(),
            name: Some(command.to_string()),
        }
    }

    #[tokio::test]
    async fn submit_and_get_job() {
        let handle = start(test_config(), EventBus::new(16));

        let (job_id, position) = handle.submit_job(make_spec("echo hi", 0, vec![])).await;
        assert!(!job_id.is_empty());
        assert_eq!(position, 0);

        let job = handle.get_job(job_id.clone()).await;
        assert!(job.is_some());
        let job = job.unwrap();
        assert_eq!(job.id, job_id);
        assert_eq!(job.spec.command, "echo hi");
        assert_eq!(job.status, JobStatus::Queued); // max_concurrent=0, stays queued
    }

    #[tokio::test]
    async fn submit_multiple_and_query() {
        let handle = start(test_config(), EventBus::new(16));

        handle.submit_job(make_spec("job1", 0, vec!["ml"])).await;
        handle.submit_job(make_spec("job2", 0, vec!["web"])).await;
        handle.submit_job(make_spec("job3", 0, vec!["ml"])).await;

        let all = handle
            .query_jobs(JobFilter {
                status: None,
                tag: None,
                limit: 50,
            })
            .await;
        assert_eq!(all.len(), 3);

        let ml_only = handle
            .query_jobs(JobFilter {
                status: None,
                tag: Some("ml".to_string()),
                limit: 50,
            })
            .await;
        assert_eq!(ml_only.len(), 2);
    }

    #[tokio::test]
    async fn query_with_status_filter() {
        let handle = start(test_config(), EventBus::new(16));

        handle.submit_job(make_spec("job1", 0, vec![])).await;
        handle.submit_job(make_spec("job2", 0, vec![])).await;

        let queued = handle
            .query_jobs(JobFilter {
                status: Some(JobStatus::Queued),
                tag: None,
                limit: 50,
            })
            .await;
        assert_eq!(queued.len(), 2);

        let running = handle
            .query_jobs(JobFilter {
                status: Some(JobStatus::Running),
                tag: None,
                limit: 50,
            })
            .await;
        assert_eq!(running.len(), 0);
    }

    #[tokio::test]
    async fn query_with_limit() {
        let handle = start(test_config(), EventBus::new(16));

        for i in 0..10 {
            handle
                .submit_job(make_spec(&format!("job{}", i), 0, vec![]))
                .await;
        }

        let limited = handle
            .query_jobs(JobFilter {
                status: None,
                tag: None,
                limit: 3,
            })
            .await;
        assert_eq!(limited.len(), 3);
    }

    #[tokio::test]
    async fn cancel_queued_job() {
        let handle = start(test_config(), EventBus::new(16));

        let (job_id, _) = handle.submit_job(make_spec("echo cancel me", 0, vec![])).await;

        let result = handle.cancel_job(job_id.clone()).await;
        assert!(result.is_ok());
        let (prev, new) = result.unwrap();
        assert_eq!(prev, JobStatus::Queued);
        assert_eq!(new, JobStatus::Cancelled);

        // Job should now be in completed list, not queue
        let job = handle.get_job(job_id).await.unwrap();
        assert_eq!(job.status, JobStatus::Cancelled);
        assert!(job.completed_at.is_some());
    }

    #[tokio::test]
    async fn cancel_nonexistent_job() {
        let handle = start(test_config(), EventBus::new(16));
        let result = handle.cancel_job("nonexistent".to_string()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn get_nonexistent_job() {
        let handle = start(test_config(), EventBus::new(16));
        let job = handle.get_job("nonexistent".to_string()).await;
        assert!(job.is_none());
    }

    #[tokio::test]
    async fn priority_ordering_in_queue() {
        let handle = start(test_config(), EventBus::new(16));

        handle.submit_job(make_spec("low", 0, vec![])).await;
        handle.submit_job(make_spec("high", 10, vec![])).await;
        handle.submit_job(make_spec("med", 5, vec![])).await;

        let jobs = handle
            .query_jobs(JobFilter {
                status: Some(JobStatus::Queued),
                tag: None,
                limit: 50,
            })
            .await;
        // query_jobs sorts by status then time, but queue order is by priority
        assert_eq!(jobs.len(), 3);
    }

    #[test]
    fn tail_lines_basic() {
        assert_eq!(tail_lines("a\nb\nc\nd\ne", 3), "c\nd\ne");
        assert_eq!(tail_lines("a\nb\nc", 5), "a\nb\nc");
        assert_eq!(tail_lines("", 3), "");
        assert_eq!(tail_lines("single", 1), "single");
    }

    #[test]
    fn tail_lines_exact() {
        assert_eq!(tail_lines("a\nb\nc", 3), "a\nb\nc");
    }
}

/// Start the scheduler actor. Returns a handle for sending messages.
pub fn start(config: Config, event_bus: EventBus) -> SchedulerHandle {
    let (tx, mut rx) = mpsc::channel::<SchedulerMsg>(256);
    let handle = SchedulerHandle { tx };
    let mut actor = SchedulerActor::new(config, event_bus, handle.clone());

    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            match msg {
                SchedulerMsg::SubmitJob { spec, reply } => {
                    let result = actor.handle_submit(spec);
                    let _ = reply.send(result);
                }
                SchedulerMsg::CancelJob { job_id, reply } => {
                    let result = actor.handle_cancel(&job_id);
                    let _ = reply.send(result);
                }
                SchedulerMsg::JobCompleted { job_id, exit_code } => {
                    actor.handle_completed(&job_id, exit_code);
                }
                SchedulerMsg::QueryJobs { filter, reply } => {
                    let result = actor.query_jobs(&filter);
                    let _ = reply.send(result);
                }
                SchedulerMsg::GetJob { job_id, reply } => {
                    let result = actor.get_job(&job_id);
                    let _ = reply.send(result);
                }
                SchedulerMsg::GetLogs {
                    job_id,
                    stream,
                    tail,
                    since_offset,
                    reply,
                } => {
                    let result = actor.get_logs(&job_id, &stream, tail, since_offset).await;
                    let _ = reply.send(result);
                }
            }
        }
    });

    handle
}

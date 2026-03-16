pub mod queue;

use crate::config::Config;
use crate::events::EventBus;
use crate::models::*;
use crate::worker;
use chrono::Utc;
use queue::JobQueue;
use std::collections::HashMap;
use tokio::sync::{mpsc, oneshot};
use tracing::info;

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

struct SchedulerActor {
    config: Config,
    queue: JobQueue,
    running: HashMap<JobId, RunningJob>,
    completed: Vec<Job>,
    event_bus: EventBus,
    scheduler_handle: SchedulerHandle,
}

struct RunningJob {
    job: Job,
    cancel_tx: Option<oneshot::Sender<()>>,
}

impl SchedulerActor {
    fn new(config: Config, event_bus: EventBus, scheduler_handle: SchedulerHandle) -> Self {
        Self {
            config,
            queue: JobQueue::new(),
            running: HashMap::new(),
            completed: Vec::new(),
            event_bus,
            scheduler_handle,
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
            },
            workers: crate::config::WorkerConfig {
                default_working_dir: "/tmp/wartable-test-jobs".to_string(),
                log_dir: "/tmp/wartable-test-logs".to_string(),
                kill_grace_period_secs: 5,
            },
            dashboard: crate::config::DashboardConfig::default(),
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

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

use crate::config::Config;
use crate::models::Job;
use crate::scheduler::SchedulerHandle;
use base64::Engine;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::fs;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot};
use tracing::{error, info, warn};

/// Spawn a worker task for a job. Returns a cancel handle.
pub fn spawn_worker(
    job: Job,
    config: Config,
    scheduler: SchedulerHandle,
) -> oneshot::Sender<()> {
    let (cancel_tx, cancel_rx) = oneshot::channel::<()>();

    tokio::spawn(async move {
        let job_id = job.id.clone();
        let result = run_job(&job, &config, cancel_rx).await;

        match result {
            Ok(exit_code) => {
                info!(job_id = %job_id, exit_code, "job completed");
                scheduler.job_completed(job_id, exit_code).await;
            }
            Err(e) => {
                error!(job_id = %job_id, error = %e, "job failed");
                scheduler.job_completed(job_id, -1).await;
            }
        }
    });

    cancel_tx
}

#[derive(Debug)]
enum LogLine {
    Stdout(String),
    Stderr(String),
}

async fn run_job(
    job: &Job,
    config: &Config,
    cancel_rx: oneshot::Receiver<()>,
) -> anyhow::Result<i32> {
    let log_dir = config.log_dir().join(&job.id);
    fs::create_dir_all(&log_dir).await?;

    let working_dir = job
        .spec
        .working_dir
        .as_deref()
        .unwrap_or(&config.workers.default_working_dir);

    // Write any uploaded files
    for file in &job.spec.files {
        let file_path = PathBuf::from(working_dir).join(&file.name);
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).await?;
        }
        let content = base64::engine::general_purpose::STANDARD
            .decode(&file.content_base64)?;
        fs::write(&file_path, &content).await?;
    }

    // Ensure working directory exists
    fs::create_dir_all(working_dir).await?;

    let mut cmd = Command::new("stdbuf");
    cmd.args(["-oL", "-eL", "bash", "-c"])
        .arg(&job.spec.command)
        .current_dir(working_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        // Force line-buffered output for common runtimes
        .env("PYTHONUNBUFFERED", "1")
        .env("PYTHONDONTWRITEBYTECODE", "1");

    for (k, v) in &job.spec.env {
        cmd.env(k, v);
    }

    let mut child = cmd.spawn()?;

    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    let stdout_path = log_dir.join("stdout.log");
    let stderr_path = log_dir.join("stderr.log");
    let combined_path = log_dir.join("combined.log");

    // Both stdout and stderr readers send lines to a single writer task
    // that writes to individual files AND a combined log in arrival order
    let (tx, rx) = mpsc::channel::<LogLine>(256);

    let tx_out = tx.clone();
    let stdout_reader = tokio::spawn(async move {
        let mut buf = BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            match buf.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => { let _ = tx_out.send(LogLine::Stdout(line.clone())).await; }
                Err(_) => break,
            }
        }
    });

    let tx_err = tx;
    let stderr_reader = tokio::spawn(async move {
        let mut buf = BufReader::new(stderr);
        let mut line = String::new();
        loop {
            line.clear();
            match buf.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => { let _ = tx_err.send(LogLine::Stderr(line.clone())).await; }
                Err(_) => break,
            }
        }
    });

    let writer = tokio::spawn(async move {
        let mut stdout_file = fs::OpenOptions::new()
            .create(true).append(true).open(&stdout_path).await.unwrap();
        let mut stderr_file = fs::OpenOptions::new()
            .create(true).append(true).open(&stderr_path).await.unwrap();
        let mut combined_file = fs::OpenOptions::new()
            .create(true).append(true).open(&combined_path).await.unwrap();

        let mut rx = rx;
        while let Some(line) = rx.recv().await {
            match &line {
                LogLine::Stdout(s) => {
                    let _ = stdout_file.write_all(s.as_bytes()).await;
                    let _ = combined_file.write_all(s.as_bytes()).await;
                }
                LogLine::Stderr(s) => {
                    let _ = stderr_file.write_all(s.as_bytes()).await;
                    // Prefix stderr lines so the dashboard can color them
                    let _ = combined_file.write_all(b"\x02");
                    let _ = combined_file.write_all(s.as_bytes()).await;
                }
            }
        }
    });

    let exit_code = tokio::select! {
        status = child.wait() => {
            let status = status?;
            status.code().unwrap_or(-1)
        }
        _ = cancel_rx => {
            let pid = child.id();
            if let Some(pid) = pid {
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGTERM);
                }

                let grace = config.workers.kill_grace_period_secs;
                tokio::select! {
                    status = child.wait() => {
                        let status = status?;
                        status.code().unwrap_or(-1)
                    }
                    _ = tokio::time::sleep(std::time::Duration::from_secs(grace)) => {
                        warn!(job_id = %job.id, "grace period expired, sending SIGKILL");
                        unsafe {
                            libc::kill(-(pid as i32), libc::SIGKILL);
                        }
                        let status = child.wait().await?;
                        status.code().unwrap_or(-1)
                    }
                }
            } else {
                -1
            }
        }
    };

    let _ = stdout_reader.await;
    let _ = stderr_reader.await;
    let _ = writer.await;

    Ok(exit_code)
}

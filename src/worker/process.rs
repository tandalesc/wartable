use crate::config::Config;
use crate::models::Job;
use crate::scheduler::SchedulerHandle;
use base64::Engine;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::fs;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::oneshot;
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

    let stdout_handle = tokio::spawn(stream_to_file(stdout, stdout_path));
    let stderr_handle = tokio::spawn(stream_to_file(stderr, stderr_path));

    let exit_code = tokio::select! {
        status = child.wait() => {
            let status = status?;
            status.code().unwrap_or(-1)
        }
        _ = cancel_rx => {
            let pid = child.id();
            if let Some(pid) = pid {
                // Send SIGTERM to process group
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

    let _ = stdout_handle.await;
    let _ = stderr_handle.await;

    Ok(exit_code)
}

async fn stream_to_file(
    reader: impl tokio::io::AsyncRead + Unpin,
    path: PathBuf,
) -> anyhow::Result<()> {
    let mut buf_reader = BufReader::new(reader);
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .await?;

    let mut line = String::new();
    loop {
        line.clear();
        let n = buf_reader.read_line(&mut line).await?;
        if n == 0 {
            break;
        }
        file.write_all(line.as_bytes()).await?;
    }
    Ok(())
}

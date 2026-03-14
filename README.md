# wartable

Resource-aware job scheduler for GPU homelab servers. Lets multiple Claude Code instances submit and monitor workloads via MCP.

Single Rust binary that serves:
- **MCP server** (streamable HTTP via rmcp) — Claude Code connects directly
- **Job scheduler** — priority queue, concurrent dispatch, process management
- **Web dashboard** — live job queue, log tailing, GPU/CPU/RAM metrics
- **REST API** — same data surface for the dashboard

## Quick Start

### Systemd (recommended for GPU servers)

```bash
git clone git@github.com:tandalesc/wartable.git
cd wartable
./deploy.sh
```

This builds, installs to `/usr/local/bin`, and sets up a systemd service. To update later, just run `./deploy.sh` again.

```bash
# Manage the service
sudo systemctl status wartable
journalctl -u wartable -f
sudo systemctl restart wartable
```

### Binary (manual)

```bash
cargo build --release
./target/release/wartable
```

### Docker

```bash
docker compose up -d --build
```

Requires [nvidia-container-toolkit](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/latest/install-guide.html) for GPU metrics.

## Claude Code Setup

Add to `~/.claude/mcp.json` on any machine that should be able to submit jobs:

```json
{
  "mcpServers": {
    "wartable": {
      "type": "streamable-http",
      "url": "http://<server-ip>:9400/mcp"
    }
  }
}
```

Restart Claude Code. You'll have these tools available:

| Tool | Description |
|------|-------------|
| `submit_job` | Submit a bash command to the work queue |
| `list_jobs` | List jobs, filter by status/tag |
| `get_job_status` | Full details for a specific job |
| `get_job_logs` | Tail stdout/stderr, incremental polling via offset |
| `cancel_job` | Cancel queued or running jobs (SIGTERM → SIGKILL) |
| `upload_file` | Write a base64-encoded file to the server |
| `download_file` | Read a file from the server as base64 |

## Dashboard

Open `http://<server-ip>:9400` in a browser. Dark mode, live-updating job table with log viewer, GPU/CPU/RAM/disk metrics.

## Working Directory

Jobs run in `/opt/wartable/jobs` by default. Each job can specify a custom `working_dir` when submitted (e.g., to run in an existing project directory).

To change the default:

```toml
# ~/.wartable/config.toml
[workers]
default_working_dir = "/path/to/your/workspace"
```

Jobs run as the system user that started wartable. If you need access to directories outside `/opt/wartable`, either:
- Run wartable as a user with appropriate permissions
- Set `working_dir` per job to a directory that user can access

## Configuration

All configuration is optional. Defaults work out of the box.

Create `~/.wartable/config.toml` to customize:

```toml
[server]
host = "0.0.0.0"    # listen address
port = 9400          # listen port

[scheduler]
max_concurrent_jobs = 8    # max parallel jobs

[workers]
default_working_dir = "/opt/wartable/jobs"    # where jobs run by default
log_dir = "/opt/wartable/logs"                # stdout/stderr capture
kill_grace_period_secs = 10                   # SIGTERM → SIGKILL timeout

[dashboard]
enabled = true
# static_dir = "/opt/wartable/dashboard"      # override dashboard path
```

## Architecture

```
Claude Code ──MCP──┐
Claude Code ──MCP──┼──► Axum (:9400) ──► Scheduler Actor ──► Worker Pool
Browser ──HTTP─────┘                          │
                                         Event Bus ──► Dashboard
```

All mutable state lives in the scheduler actor (tokio mpsc). No shared locks. GPU metrics via NVML, system metrics via sysinfo.

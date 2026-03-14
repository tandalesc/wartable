# wartable

Resource-aware job scheduler for GPU homelab servers. Lets multiple Claude Code instances submit and monitor workloads via MCP.

Single Rust binary that serves:
- **MCP server** (streamable HTTP via rmcp) — Claude Code connects directly
- **Job scheduler** — priority queue, concurrent dispatch, process management
- **Web dashboard** — live job queue, log tailing, status monitoring
- **REST API** — same data surface for the dashboard

## Quick Start

### Docker (recommended)

```bash
# Clone and start
git clone git@github.com:tandalesc/wartable.git
cd wartable
docker compose up -d --build
```

For GPU passthrough, uncomment the `deploy.resources` section in `docker-compose.yml`.

### Binary

```bash
cargo build --release
./target/release/wartable
```

### Systemd

```bash
cargo build --release
sudo cp target/release/wartable /usr/local/bin/
sudo mkdir -p /opt/wartable && sudo cp -r dashboard/ /opt/wartable/dashboard/
sudo cp wartable.service /etc/systemd/system/
sudo systemctl enable --now wartable

# Logs
journalctl -u wartable -f
```

## Claude Code Setup

Add to `~/.claude/mcp.json`:

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

Open `http://<server-ip>:9400` in a browser. Dark mode, live-updating job table with log viewer.

## Configuration

Optional `~/.wartable/config.toml`:

```toml
[server]
host = "0.0.0.0"
port = 9400

[scheduler]
max_concurrent_jobs = 8

[workers]
default_working_dir = "/home/user"
log_dir = "~/.wartable/logs"
kill_grace_period_secs = 10

[dashboard]
enabled = true
# static_dir = "/opt/wartable/dashboard"
```

## Architecture

```
Claude Code ──MCP──┐
Claude Code ──MCP──┼──► Axum (:9400) ──► Scheduler Actor ──► Worker Pool
Browser ──HTTP─────┘                          │
                                         Event Bus ──► Dashboard
```

All mutable state lives in the scheduler actor (tokio mpsc). No shared locks. Jobs run as the user that started wartable.

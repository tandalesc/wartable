# wartable

Resource-aware job scheduler for GPU homelab servers. Lets multiple Claude Code instances submit and monitor workloads via MCP.

Single Rust binary that serves:
- **MCP server** (streamable HTTP via rmcp) — Claude Code connects directly
- **Job scheduler** — priority queue, concurrent dispatch, process management
- **Web dashboard** — live job queue, log tailing, GPU/CPU/RAM metrics
- **REST API** — same data surface for the dashboard

![wartable dashboard](docs/wartable_dashboard.png)

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
      "url": "http://<server-ip>:9400/mcp",
      "headers": {
        "Authorization": "Bearer <key>"
      }
    }
  }
}
```

If auth is disabled (the default), omit the `headers` field. If auth is enabled, generate a key from the dashboard's **KEYS** panel.

Restart Claude Code. You'll have these tools available:

| Tool | Description |
|------|-------------|
| `submit_job` | Submit a bash command with optional resource requirements, priority, tags, env vars, and file uploads |
| `list_jobs` | List jobs, filter by status/tag |
| `get_job_status` | Full details for a specific job |
| `get_job_logs` | Tail stdout/stderr/combined logs with incremental polling via byte offset |
| `cancel_job` | Cancel queued or running jobs (SIGTERM → SIGKILL) |
| `upload_file` | Write a base64-encoded file to the server (must be under `working_dir` or `log_dir`) |
| `download_file` | Get a presigned download URL for a file (15-min expiry, no content through MCP) |

## Dashboard

Open `http://<server-ip>:9400` in a browser. Dark mode, live-updating job table with log viewer, GPU/CPU/RAM/disk metrics. Responsive layout for tablet and mobile.

### REST API

The dashboard is backed by a REST API available at the same address:

| Endpoint | Description |
|----------|-------------|
| `GET /api/jobs` | List jobs (supports `status`, `tag`, `limit` query params) |
| `GET /api/jobs/{id}` | Get job details |
| `GET /api/jobs/{id}/logs` | Get logs (`stream`, `tail`, `since_offset` query params) |
| `POST /api/jobs/{id}/cancel` | Cancel a job |
| `GET /api/resources` | System snapshot — CPU, RAM, disk, load, per-GPU stats |
| `GET /api/dl?path=...&exp=...&sig=...` | Download a file via presigned URL (15-min expiry) |
| `GET /api/clients` | List connected API clients with last-seen time and request count |
| `GET /api/keys` | List API keys (secrets masked) |
| `POST /api/keys/generate` | Generate a new API key (`{"name": "..."}`) |
| `POST /api/keys/revoke` | Revoke a runtime-generated key (`{"name": "..."}`) |
| `GET /api/events` | SSE event stream — real-time job lifecycle events |

### SSE Event Stream

`GET /api/events` returns a Server-Sent Events stream of job lifecycle events. Each event has a named type and a JSON data payload:

```
event: job_submitted
data: {"type":"job_submitted","job":{"job_id":"...","status":"queued",...}}

event: job_completed
data: {"type":"job_completed","job":{"job_id":"...","status":"completed","exit_code":0,...}}
```

Event types: `job_submitted`, `job_started`, `job_completed`, `job_cancelled`. Useful for building integrations that react to job state changes without polling.

### Claude Code Channel (push notifications)

The optional `wartable-channel` lets Claude Code receive job events in real-time instead of polling. When a job completes or fails, Claude sees it immediately and can act.

**Setup:**

```bash
cd channel && bun install
```

Add to `~/.claude/mcp.json` alongside the existing wartable MCP entry:

```json
{
  "mcpServers": {
    "wartable": {
      "type": "streamable-http",
      "url": "http://<server-ip>:9400/mcp"
    },
    "wartable-channel": {
      "command": "bun",
      "args": ["<path-to>/wartable/channel/wartable-channel.ts"],
      "env": {
        "WARTABLE_URL": "http://<server-ip>:9400",
        "WARTABLE_API_KEY": "<key-if-auth-enabled>"
      }
    }
  }
}
```

Start Claude Code with the development channel flag:

```bash
claude --dangerously-load-development-channels server:wartable-channel
```

Claude will receive `<channel source="wartable-channel" event="job_completed" job_id="..." ...>` tags automatically when job state changes. It can then use the existing wartable MCP tools to investigate, resubmit, or take action.

## Job Features

Jobs support several options beyond a bare command:

- **Priority** — integer priority (default 0); higher values jump the queue
- **Tags** — string tags for filtering and grouping
- **Resource requirements** — GPU count, VRAM minimum, CPU cores, RAM, disk
- **Environment variables** — custom env vars injected into the job process
- **File uploads** — base64-encoded files written before the job starts, with optional Unix permission bits
- **Working directory** — per-job override for where the command runs

### Log Management

Each job captures stdout and stderr separately, plus a combined log with chronological ordering and stream markers (`out`/`err`). Logs support:
- Incremental polling via byte offset (`since_offset`)
- Tail mode for the last N lines
- Stream filtering (stdout only, stderr only, or combined)

### Process Control

Jobs run in isolated process groups with Python-specific buffering fixes (`PYTHONUNBUFFERED`, line-buffered stdout via `stdbuf`). Cancellation sends SIGTERM first, then SIGKILL after the grace period.

## Working Directory

Jobs run in `/opt/wartable/jobs` by default. Each job can specify a custom `working_dir` when submitted (e.g., to run in an existing project directory).

To change the default:

```toml
# ~/.wartable/config.toml
[workers]
default_working_dir = "/path/to/your/workspace"
```

### User & Permissions

All jobs run as the system user that owns the wartable process. The deploy script creates a dedicated `wartable` system user by default.

**Using a custom user:**

```bash
# Deploy with your own user account
WARTABLE_USER=myuser ./deploy.sh
```

The deploy script will:
1. Skip user creation if the user already exists
2. Add the user to `video` and `render` groups (for GPU access via NVML)
3. Set ownership of `/opt/wartable` to that user
4. Run the systemd service as that user

**Granting access to project directories:**

If jobs need to read/write directories outside `/opt/wartable` (e.g., your home directory or shared project folders), add the wartable user to the appropriate group or adjust directory permissions:

```bash
# Option 1: Add wartable user to your group
sudo usermod -aG mygroup wartable

# Option 2: Grant access to a specific directory
sudo setfacl -R -m u:wartable:rwx /home/myuser/projects

# Option 3: Run as your own user instead of a system user
WARTABLE_USER=$(whoami) ./deploy.sh
```

**Running without deploy.sh:**

If you run wartable manually (`cargo run` or `./target/release/wartable`), jobs run as your current user with your current permissions — no special setup needed. The `WARTABLE_USER` variable only applies to the deploy script and systemd service.

**GPU access:**

The deploy script automatically adds the service user to `video` and `render` groups. If GPU metrics aren't showing up, verify group membership:

```bash
# Check groups
groups wartable

# Add manually if needed
sudo usermod -aG video,render wartable
sudo systemctl restart wartable
```

## Configuration

All configuration is optional. Defaults work out of the box.

Create `~/.wartable/config.toml` to customize:

```toml
[server]
host = "0.0.0.0"    # listen address
port = 9400          # listen port
# base_url = "http://my-server:9400"  # override base URL for presigned download links

[scheduler]
max_concurrent_jobs = 8    # max parallel jobs

[workers]
default_working_dir = "/opt/wartable/jobs"    # where jobs run by default
log_dir = "/opt/wartable/logs"                # stdout/stderr capture
kill_grace_period_secs = 10                   # SIGTERM → SIGKILL timeout
# extra_allowed_dirs = ["~/projects"]         # additional dirs for file upload/download access

[dashboard]
enabled = true
# static_dir = "/opt/wartable/dashboard"      # override dashboard path

[auth]
enabled = false                                # set true to require API keys
# api_keys = []                                # optional: pre-configured keys (generate from dashboard instead)
```

### Authentication

When `[auth] enabled = true`, all `/api` and `/mcp` routes require a valid API key. An admin key is auto-generated on every startup — the dashboard authenticates automatically via an `HttpOnly` session cookie set when you load the page. No manual key entry needed.

**Generating keys for MCP clients:**

1. Open the dashboard and click **KEYS** in the toolbar
2. Enter a client name and click **GENERATE**
3. Copy the key (shown once) and configure the MCP client
4. To cut access, click **REVOKE** — takes effect immediately

Keys can also be pre-configured in `config.toml` (these cannot be revoked at runtime):

```toml
[auth]
enabled = true
api_keys = [
  { name = "permanent-client", key = "wt-your-secret-key-here" },
]
```

**Claude Code MCP config** with a generated key:

```json
{
  "mcpServers": {
    "wartable": {
      "type": "streamable-http",
      "url": "http://<server-ip>:9400/mcp",
      "headers": {
        "Authorization": "Bearer <generated-key>"
      }
    }
  }
}
```

**curl example:**

```bash
curl -H "Authorization: Bearer <key>" http://localhost:9400/api/jobs
```

## Architecture

```
Claude Code ──MCP──┐
Claude Code ──MCP──┼──► Axum (:9400) ──► Scheduler Actor ──► Worker Pool
Browser ──HTTP─────┘                          │
                                         Event Bus ──► Dashboard
```

All mutable state lives in the scheduler actor (tokio mpsc). No shared locks. GPU metrics via NVML, system metrics via sysinfo.

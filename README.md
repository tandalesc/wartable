# wartable

Resource-aware job scheduler for GPU homelab servers. Lets multiple Claude Code instances submit and monitor workloads via MCP.

Single Rust binary that serves:
- **MCP server** (streamable HTTP) — Claude Code connects directly
- **Job scheduler** — priority queue, concurrent dispatch, process management
- **Web dashboard** — live job queue, log tailing, GPU/CPU/RAM metrics
- **REST API** + **SSE event stream** — for the dashboard and custom integrations

![wartable dashboard](docs/wartable_dashboard.png)

## Quick Start

### Deploy (systemd)

```bash
git clone git@github.com:tandalesc/wartable.git
cd wartable
./deploy.sh
```

Builds, installs to `/usr/local/bin`, sets up a systemd service. Run `./deploy.sh` again to update.

### Or run manually

```bash
cargo build --release && ./target/release/wartable
```

### Or Docker

```bash
docker compose up -d --build
```

## Claude Code Setup

Add to your MCP config (project `.mcp.json` or user-level):

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

If auth is enabled, add `"headers": { "Authorization": "Bearer <key>" }`. Generate keys from the dashboard's **KEYS** panel.

Restart Claude Code and you'll have `submit_job`, `list_jobs`, `get_job_status`, `get_job_logs`, `cancel_job`, `upload_file`, and `download_file`.

## Push Notifications (Channel)

Optional. The `wartable-channel` pushes job events and log updates into your Claude Code session in real-time — no polling.

**Setup:**

```bash
cd wartable/channel && npm install
```

Register the channel in your project:

```bash
claude mcp add wartable-channel -s project -- npx tsx /path/to/wartable/channel/wartable-channel.ts
```

Then edit `.mcp.json` to add the `env` block with your server URL:

```json
{
  "mcpServers": {
    "wartable-channel": {
      "command": "npx",
      "args": ["tsx", "/path/to/wartable/channel/wartable-channel.ts"],
      "env": {
        "WARTABLE_URL": "http://<server-ip>:9400"
      }
    }
  }
}
```

Start Claude Code with the channel enabled:

```bash
claude --dangerously-load-development-channels server:wartable-channel
```

**What you get:**

- Job lifecycle events pushed automatically (`job_submitted`, `job_started`, `job_completed`, `job_cancelled`)
- `subscribe_job_logs` tool — subscribe to periodic log updates for long-running jobs (configurable interval, auto-stops on completion)
- `unsubscribe_job_logs` / `list_log_subscriptions` for managing subscriptions

## Configuration

All optional. Defaults work out of the box. Create `~/.wartable/config.toml` to customize:

```toml
[server]
host = "0.0.0.0"
port = 9400
# base_url = "http://my-server:9400"

[scheduler]
max_concurrent_jobs = 8

[workers]
default_working_dir = "/opt/wartable/jobs"
log_dir = "/opt/wartable/logs"
kill_grace_period_secs = 10
# extra_allowed_dirs = ["~/projects"]

[auth]
enabled = false
# api_keys = [{ name = "my-client", key = "wt-secret" }]

[dashboard]
enabled = true
```

### Authentication

When `[auth] enabled = true`, all routes require an API key. The dashboard auto-authenticates via session cookie. Generate keys for MCP clients from the dashboard's **KEYS** panel, or pre-configure them in `config.toml`.

### Permissions

Jobs run as the wartable process owner. The deploy script creates a `wartable` system user by default. Use `WARTABLE_USER=myuser ./deploy.sh` to run as a different user. If running manually (`cargo run`), jobs use your current user.

For GPU access, the deploy script adds the user to `video` and `render` groups automatically.

## Architecture

```
Claude Code ──MCP──┐
Claude Code ──MCP──┼──► Axum (:9400) ──► Scheduler Actor ──► Worker Pool
Browser ──HTTP─────┘         │
Channel ──SSE──────────── Event Bus ──► Dashboard
```

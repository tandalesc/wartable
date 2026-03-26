# Changelog

## [0.2.0] - 2026-03-26

### Added

- **GPU VRAM budget scheduling** — Jobs request GPU resources via `gpu_count` and `gpu_vram_min_gb` (required when `gpu_count > 0`). The scheduler tracks per-device VRAM budgets and checks live VRAM via NVML (with 10s cooldown) so external GPU consumers are accounted for. Assigns GPUs via a configurable policy (`least-loaded` or `packed`), and injects `CUDA_VISIBLE_DEVICES` (or custom env var) into the job environment. VRAM totals are auto-detected from NVML or can be overridden in config. See `[scheduler.gpu]` in `config.toml`.
- **Job retry** — New `POST /api/jobs/{id}/retry` endpoint resubmits a failed, cancelled, or completed job with the same spec. Dashboard detail panel shows a RETRY button for finished jobs.
- **Copy logs button** — COPY button in the log controls copies the current log output to clipboard.
- **Collapsible command display** — Long commands in the detail panel are truncated to 2 lines with an expand/collapse toggle. Very long commands scroll within a max-height container.

- **Consolidated keys/clients panel** — Merged the separate CLIENTS and KEYS panels into a single KEYS panel showing each key with its activity (req/hr, last seen).

### Changed

- **Auth enabled by default** — `[auth] enabled` now defaults to `true`. Set to `false` explicitly to disable.
- **All view includes cancelled jobs** — Dashboard "All" filter now requests up to 200 jobs (was 50) so cancelled jobs (sorted last) are no longer truncated.
- **Client request rate** — Shows requests per hour instead of raw request count.

### Fixed

- **README: MCP transport type** — Corrected `"streamable-http"` to `"http"` in the Claude Code MCP config example.
- **README: auth headers example** — Added a complete JSON example showing the `headers` field for API key authentication.
- **Channel auth errors** — `subscribe_job_logs` now distinguishes 401 auth failures from 404 job-not-found instead of reporting all errors as "Job not found".

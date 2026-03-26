# Changelog

## [0.2.0] - 2026-03-26

### Added

- **GPU VRAM budget scheduling** — Jobs can request GPU resources via `gpu_count` and `gpu_vram_min_gb`. The scheduler tracks per-device VRAM budgets, assigns GPUs via a configurable policy (`least-loaded` or `packed`), and injects `CUDA_VISIBLE_DEVICES` (or custom env var) into the job environment. VRAM totals are auto-detected from NVML or can be overridden in config. See `[scheduler.gpu]` in `config.toml`.
- **Job retry** — New `POST /api/jobs/{id}/retry` endpoint resubmits a failed, cancelled, or completed job with the same spec. Dashboard detail panel shows a RETRY button for finished jobs.
- **Copy logs button** — COPY button in the log controls copies the current log output to clipboard.
- **Collapsible command display** — Long commands in the detail panel are truncated to 2 lines with an expand/collapse toggle. Very long commands scroll within a max-height container.

### Changed

- **Auth enabled by default** — `[auth] enabled` now defaults to `true`. Set to `false` explicitly to disable.
- **All view includes cancelled jobs** — Dashboard "All" filter now requests up to 200 jobs (was 50) so cancelled jobs (sorted last) are no longer truncated.

### Fixed

- **README: MCP transport type** — Corrected `"streamable-http"` to `"http"` in the Claude Code MCP config example.
- **README: auth headers example** — Added a complete JSON example showing the `headers` field for API key authentication.

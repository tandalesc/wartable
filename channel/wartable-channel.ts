#!/usr/bin/env node
import { Server } from "@modelcontextprotocol/sdk/server/index.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import {
  ListToolsRequestSchema,
  CallToolRequestSchema,
} from "@modelcontextprotocol/sdk/types.js";

// Configuration from environment
const WARTABLE_URL = (
  process.env.WARTABLE_URL || "http://localhost:9400"
).replace(/\/$/, "");
const WARTABLE_API_KEY = process.env.WARTABLE_API_KEY || "";

function authHeaders(): Record<string, string> {
  const h: Record<string, string> = {};
  if (WARTABLE_API_KEY) h["Authorization"] = `Bearer ${WARTABLE_API_KEY}`;
  return h;
}

// ── Log subscription state ──

interface LogSubscription {
  jobId: string;
  intervalMs: number;
  tailLines: number;
  pattern: RegExp | null;
  offset: number;
  timer: ReturnType<typeof setInterval>;
}

const subscriptions = new Map<string, LogSubscription>();

async function pollLogs(sub: LogSubscription) {
  try {
    const params = new URLSearchParams({
      stream: "both",
      since_offset: String(sub.offset),
    });
    const res = await fetch(
      `${WARTABLE_URL}/api/jobs/${sub.jobId}/logs?${params}`,
      { headers: authHeaders() }
    );
    if (!res.ok) {
      if (res.status === 401) {
        console.error(`Auth failed polling logs for ${sub.jobId} — is WARTABLE_API_KEY set?`);
      } else if (res.status === 404) {
        // Job gone — clean up
        unsubscribe(sub.jobId);
        await mcp.notification({
          method: "notifications/claude/channel",
          params: {
            channel: "wartable-channel",
            content: JSON.stringify({
              event: "log_subscription_ended",
              job_id: sub.jobId,
              reason: "job_not_found",
            }),
            meta: { event: "log_subscription_ended", job_id: sub.jobId },
          },
        });
      }
      return;
    }

    const logs = await res.json();
    const newOffset = logs.combined_offset ?? logs.stdout_offset ?? 0;

    // Only push if there's new content
    if (newOffset > sub.offset) {
      let lines: string[];
      if (logs.combined && logs.combined.length > 0) {
        lines = logs.combined.map((l: any) => `[${l.stream}] ${l.line}`);
      } else {
        const parts: string[] = [];
        if (logs.stdout?.trim()) parts.push(...logs.stdout.trim().split("\n"));
        if (logs.stderr?.trim())
          parts.push(
            ...logs.stderr
              .trim()
              .split("\n")
              .map((l: string) => `[stderr] ${l}`)
          );
        lines = parts;
      }

      // Apply pattern filter if set
      if (sub.pattern) {
        lines = lines.filter((l) => sub.pattern!.test(l));
      }

      // Always advance the offset even if no lines matched
      sub.offset = newOffset;

      // Only push if there are matching lines
      if (lines.length === 0) return;

      const content = lines.slice(-sub.tailLines).join("\n");

      await mcp.notification({
        method: "notifications/claude/channel",
        params: {
          channel: "wartable-channel",
          content,
          meta: {
            event: "log_update",
            job_id: sub.jobId,
            offset: String(newOffset),
          },
        },
      });
    }

    // Check if job is done
    const statusRes = await fetch(
      `${WARTABLE_URL}/api/jobs/${sub.jobId}`,
      { headers: authHeaders() }
    );
    if (statusRes.ok) {
      const job = await statusRes.json();
      if (["completed", "failed", "cancelled"].includes(job.status)) {
        unsubscribe(sub.jobId);
        await mcp.notification({
          method: "notifications/claude/channel",
          params: {
            channel: "wartable-channel",
            content: JSON.stringify({
              event: "log_subscription_ended",
              job_id: sub.jobId,
              reason: `job_${job.status}`,
              exit_code: job.exit_code,
            }),
            meta: {
              event: "log_subscription_ended",
              job_id: sub.jobId,
              status: job.status,
            },
          },
        });
      }
    }
  } catch (err) {
    console.error(`Log poll failed for ${sub.jobId}:`, err);
  }
}

function unsubscribe(jobId: string) {
  const sub = subscriptions.get(jobId);
  if (sub) {
    clearInterval(sub.timer);
    subscriptions.delete(jobId);
  }
}

// ── MCP server ──

const mcp = new Server(
  { name: "wartable-channel", version: "0.1.0" },
  {
    capabilities: {
      experimental: { "claude/channel": {} },
      tools: {},
    },
    instructions: [
      "Events from the wartable job scheduler arrive as <channel source=\"wartable-channel\" event=\"...\"> tags.",
      "",
      "Event types:",
      "- job_submitted, job_started, job_completed, job_cancelled: job lifecycle events (automatic)",
      "- log_update: new log output from a subscribed job (must subscribe first)",
      "- log_subscription_ended: subscription auto-stopped because the job finished",
      "",
      "To subscribe to log updates for a long-running job, call the subscribe_job_logs tool",
      "with the job_id and desired interval. Logs will be pushed as channel notifications",
      "until the job completes or you call unsubscribe_job_logs.",
      "",
      "For other actions (submit, cancel, get full logs), use the wartable MCP tools.",
    ].join("\n"),
  }
);

mcp.setRequestHandler(ListToolsRequestSchema, async () => ({
  tools: [
    {
      name: "subscribe_job_logs",
      description:
        "Subscribe to log updates for a running job. New output will be pushed as channel notifications at the specified interval. Optionally filter to lines matching a regex pattern.",
      inputSchema: {
        type: "object" as const,
        properties: {
          job_id: {
            type: "string",
            description: "The job ID to subscribe to",
          },
          interval_seconds: {
            type: "number",
            description:
              "How often to check for new log output (default: 300 = 5 minutes)",
          },
          tail_lines: {
            type: "number",
            description:
              "Max lines to include per update (default: 50)",
          },
          pattern: {
            type: "string",
            description:
              'Regex pattern to filter log lines (e.g. "Epoch|loss|accuracy"). Only matching lines are pushed. Omit to get all lines.',
          },
        },
        required: ["job_id"],
      },
    },
    {
      name: "unsubscribe_job_logs",
      description: "Stop receiving log updates for a job.",
      inputSchema: {
        type: "object" as const,
        properties: {
          job_id: {
            type: "string",
            description: "The job ID to unsubscribe from",
          },
        },
        required: ["job_id"],
      },
    },
    {
      name: "list_log_subscriptions",
      description: "List all active log subscriptions.",
      inputSchema: {
        type: "object" as const,
        properties: {},
      },
    },
  ],
}));

mcp.setRequestHandler(CallToolRequestSchema, async (req) => {
  const { name, arguments: args } = req.params;

  if (name === "subscribe_job_logs") {
    const jobId = (args as any).job_id as string;
    const intervalSecs = (args as any).interval_seconds ?? 300;
    const tailLines = (args as any).tail_lines ?? 50;
    const patternStr = (args as any).pattern as string | undefined;

    let pattern: RegExp | null = null;
    if (patternStr) {
      try {
        pattern = new RegExp(patternStr, "i");
      } catch {
        return {
          content: [
            { type: "text" as const, text: `Invalid regex pattern: ${patternStr}` },
          ],
          isError: true,
        };
      }
    }

    // Check job exists
    const res = await fetch(`${WARTABLE_URL}/api/jobs/${jobId}`, {
      headers: authHeaders(),
    });
    if (!res.ok) {
      const msg = res.status === 401
        ? `Auth failed (401) — is WARTABLE_API_KEY set?`
        : res.status === 404
        ? `Job not found: ${jobId}`
        : `Failed to fetch job ${jobId}: HTTP ${res.status}`;
      return {
        content: [{ type: "text" as const, text: msg }],
        isError: true,
      };
    }
    const job = await res.json();
    if (["completed", "failed", "cancelled"].includes(job.status)) {
      return {
        content: [
          {
            type: "text" as const,
            text: `Job ${jobId} already ${job.status}, nothing to subscribe to`,
          },
        ],
        isError: true,
      };
    }

    // Remove existing subscription if any
    unsubscribe(jobId);

    const intervalMs = intervalSecs * 1000;
    const sub: LogSubscription = {
      jobId,
      intervalMs,
      tailLines,
      pattern,
      offset: 0,
      timer: setInterval(() => pollLogs(sub), intervalMs),
    };
    subscriptions.set(jobId, sub);

    // Do an immediate first poll
    await pollLogs(sub);

    return {
      content: [
        {
          type: "text" as const,
          text: `Subscribed to logs for job ${jobId} (every ${intervalSecs}s, last ${tailLines} lines per update${pattern ? `, filter: /${patternStr}/i` : ""}). Will auto-stop when job completes.`,
        },
      ],
    };
  }

  if (name === "unsubscribe_job_logs") {
    const jobId = (args as any).job_id as string;
    if (subscriptions.has(jobId)) {
      unsubscribe(jobId);
      return {
        content: [
          { type: "text" as const, text: `Unsubscribed from job ${jobId}` },
        ],
      };
    }
    return {
      content: [
        {
          type: "text" as const,
          text: `No active subscription for job ${jobId}`,
        },
      ],
    };
  }

  if (name === "list_log_subscriptions") {
    if (subscriptions.size === 0) {
      return {
        content: [
          { type: "text" as const, text: "No active log subscriptions" },
        ],
      };
    }
    const list = [...subscriptions.values()].map((s) => ({
      job_id: s.jobId,
      interval_seconds: s.intervalMs / 1000,
      tail_lines: s.tailLines,
      pattern: s.pattern?.source ?? null,
      current_offset: s.offset,
    }));
    return {
      content: [{ type: "text" as const, text: JSON.stringify(list, null, 2) }],
    };
  }

  return {
    content: [{ type: "text" as const, text: `Unknown tool: ${name}` }],
    isError: true,
  };
});

await mcp.connect(new StdioServerTransport());

// ── SSE event stream (job lifecycle) ──

async function* parseSSE(
  stream: ReadableStream<Uint8Array>
): AsyncGenerator<{ event: string; data: string }> {
  const decoder = new TextDecoder();
  let buffer = "";
  let currentEvent = "";
  let currentData = "";

  for await (const chunk of stream) {
    buffer += decoder.decode(chunk, { stream: true });
    const lines = buffer.split("\n");
    buffer = lines.pop() || "";

    for (const line of lines) {
      if (line.startsWith("event:")) {
        currentEvent = line.slice(6).trim();
      } else if (line.startsWith("data:")) {
        currentData = line.slice(5).trim();
      } else if (line === "" && currentData) {
        yield { event: currentEvent || "message", data: currentData };
        currentEvent = "";
        currentData = "";
      }
    }
  }
}

async function connectSSE() {
  const url = `${WARTABLE_URL}/api/events`;
  const headers: Record<string, string> = {
    Accept: "text/event-stream",
    ...authHeaders(),
  };

  try {
    const res = await fetch(url, { headers });
    if (!res.ok || !res.body) {
      throw new Error(`SSE connect failed: ${res.status}`);
    }

    for await (const { event, data } of parseSSE(res.body)) {
      try {
        const parsed = JSON.parse(data);
        const jobId = parsed.job?.job_id || "unknown";
        const jobName = parsed.job?.name || "";
        const status = parsed.job?.status || event;
        const exitCode = parsed.job?.exit_code;

        await mcp.notification({
          method: "notifications/claude/channel",
          params: {
            channel: "wartable-channel",
            content: data,
            meta: {
              event,
              job_id: jobId,
              ...(jobName && { job_name: jobName }),
              status,
              ...(exitCode !== null &&
                exitCode !== undefined && { exit_code: String(exitCode) }),
            },
          },
        });
      } catch (err) {
        console.error(`Failed to forward ${event} event:`, err);
      }
    }

    console.error("SSE stream ended, reconnecting in 5s...");
    setTimeout(connectSSE, 5000);
  } catch (err) {
    console.error("SSE connection error, reconnecting in 5s...", err);
    setTimeout(connectSSE, 5000);
  }
}

connectSSE();

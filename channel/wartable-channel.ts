#!/usr/bin/env bun
import { Server } from "@modelcontextprotocol/sdk/server/index.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";

// Configuration from environment
const WARTABLE_URL = (
  process.env.WARTABLE_URL || "http://localhost:9400"
).replace(/\/$/, "");
const WARTABLE_API_KEY = process.env.WARTABLE_API_KEY || "";

const mcp = new Server(
  { name: "wartable-channel", version: "0.0.1" },
  {
    capabilities: { experimental: { "claude/channel": {} } },
    instructions: [
      "Events from the wartable job scheduler arrive as <channel source=\"wartable-channel\" event=\"...\"> tags.",
      "Each event includes a job_id attribute and a JSON body with full job details.",
      "Event types: job_submitted, job_started, job_completed, job_cancelled.",
      "These are one-way notifications. To act on them (resubmit, cancel, check logs),",
      "use the wartable MCP tools (submit_job, cancel_job, get_job_logs, etc.) which are",
      "available via the separate wartable MCP server connection.",
    ].join(" "),
  }
);

await mcp.connect(new StdioServerTransport());

// Parse SSE text stream into events
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
  const headers: Record<string, string> = { Accept: "text/event-stream" };
  if (WARTABLE_API_KEY) {
    headers["Authorization"] = `Bearer ${WARTABLE_API_KEY}`;
  }

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

    // Stream ended — reconnect
    console.error("SSE stream ended, reconnecting in 5s...");
    setTimeout(connectSSE, 5000);
  } catch (err) {
    console.error("SSE connection error, reconnecting in 5s...", err);
    setTimeout(connectSSE, 5000);
  }
}

connectSSE();

# Claude Code Usage Stats — Terminaler Integration

Research notes for integrating Claude Code usage telemetry into Terminaler.

## Available Data Points

| Metric | OTel Key | Unit |
|--------|----------|------|
| Tokens used | `claude_code.token.usage` | count (input/output/cache_read/cache_create) |
| Cost | `claude_code.cost.usage` | USD |
| Active time | `claude_code.active_time.total` | seconds |
| Sessions | `claude_code.session.count` | count |
| Commits | `claude_code.commit.count` | count |
| PRs created | `claude_code.pull_request.count` | count |
| Lines changed | `claude_code.lines_of_code.count` | count (added/removed) |

## How to Capture

Claude Code exposes usage data exclusively via **OpenTelemetry (OTel)**. There are no env vars, log files, or REST APIs for usage data.

### Required Environment Variables

```bash
CLAUDE_CODE_ENABLE_TELEMETRY=1
OTEL_METRICS_EXPORTER=otlp          # or "console" for stdout
OTEL_LOGS_EXPORTER=otlp             # or "console"
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317
OTEL_METRIC_EXPORT_INTERVAL=60000   # ms between metric exports
OTEL_LOGS_EXPORT_INTERVAL=5000      # ms between event exports
```

### Optional Controls

```bash
OTEL_LOG_USER_PROMPTS=1             # include prompt content in events
OTEL_LOG_TOOL_DETAILS=1             # include tool arguments/MCP server names
OTEL_METRICS_INCLUDE_SESSION_ID=true
OTEL_METRICS_INCLUDE_ACCOUNT_UUID=true
```

## Key Attributes on Every Metric/Event

- `session.id` — unique session identifier
- `user.account_uuid` — authenticated user
- `prompt.id` — correlates events from a single prompt turn
- Model identifier — which model was used

## Events (Structured Logs)

| Event | Description |
|-------|-------------|
| `claude_code.user_prompt` | User submits a prompt |
| `claude_code.api_request` | Each API call (with token counts, cost, duration) |
| `claude_code.api_error` | API failures |
| `claude_code.tool_result` | Tool execution (success, duration, tool name) |
| `claude_code.tool_decision` | Permission decisions |

## Integration Options (Low to High Effort)

1. **Console exporter → parse stdout** — pipe/tee Claude output to a log file, parse with a script
2. **OTLP → local OTel Collector → JSON file** — Collector writes JSON, Terminaler reads it
3. **OTLP → Prometheus** — scrape endpoint, query via PromQL
4. **OTLP → SQLite** — custom collector writes to DB, Terminaler queries directly

### Recommended Path

OTel Collector → **JSON file** or **SQLite** → Terminaler reads and aggregates daily/weekly.
Avoids running Prometheus/Grafana, keeps it self-contained.

## Limitations

- No env vars exposed during session (can't read `$CLAUDE_TOKENS`)
- No log file written by default — must enable OTel
- No REST API for historical local usage
- `/cost` command is interactive only (no `--json` flag)
- Daily/weekly aggregation must be done by the consumer (Terminaler)

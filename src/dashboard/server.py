#!/usr/bin/env python3
"""
FABRIC Dashboard Server

Standalone SSE server that receives NEEDLE events and serves a live dashboard.
No Prometheus, no Grafana, no external dependencies.

Usage:
    python3 server.py [--port PORT] [--buffer-size N] [--seed-file events.jsonl]

Endpoints:
    POST /ingest      - Receive events from fabric.sh
    GET  /stream      - SSE endpoint for browser clients
    GET  /            - Dashboard HTML
    GET  /api/summary - Aggregate stats JSON (includes bead_costs for per-bead drill-down)
    GET  /api/costs   - Per-bead cost breakdown (effort.recorded events, sorted by cost desc)
"""

import argparse
import json
import os
import signal
import socketserver
import sys
import threading
import time
from collections import deque
from datetime import datetime, timedelta, timezone
from http.server import HTTPServer, BaseHTTPRequestHandler
from typing import Any

# Default configuration
DEFAULT_PORT = 7842
DEFAULT_BUFFER_SIZE = 10000
HEARTBEAT_INTERVAL = 15  # SSE heartbeat seconds

# Global state
events_buffer: deque[dict[str, Any]] = deque(maxlen=DEFAULT_BUFFER_SIZE)
clients: list[Any] = []  # List of client queues for SSE
clients_lock = threading.Lock()
server_start_time = datetime.now(timezone.utc)
DAILY_BUDGET_USD: float = 0.0  # Set via --daily-budget CLI arg

# Per-minute bead completion counts for throughput sparkline
# Maps epoch_minute (int) -> completed_count (int)
throughput_by_minute: dict[int, int] = {}
throughput_lock = threading.Lock()
THROUGHPUT_WINDOW_MINUTES = 30


class DashboardHandler(BaseHTTPRequestHandler):
    """HTTP request handler for dashboard endpoints."""

    protocol_version = "HTTP/1.1"

    def log_message(self, format: str, *args: Any) -> None:
        """Suppress default logging."""
        pass

    def _send_json(self, data: dict, status: int = 200) -> None:
        """Send JSON response."""
        body = json.dumps(data).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Access-Control-Allow-Origin", "*")
        self.end_headers()
        self.wfile.write(body)

    def _send_text(self, text: str, content_type: str = "text/html", status: int = 200) -> None:
        """Send text response."""
        body = text.encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Access-Control-Allow-Origin", "*")
        self.end_headers()
        self.wfile.write(body)

    def do_OPTIONS(self) -> None:
        """Handle CORS preflight."""
        self.send_response(200)
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
        self.send_header("Access-Control-Allow-Headers", "Content-Type")
        self.send_header("Content-Length", "0")
        self.end_headers()

    def do_GET(self) -> None:
        """Handle GET requests."""
        if self.path == "/" or self.path == "/index.html":
            self._send_text(DASHBOARD_HTML)
        elif self.path == "/stream":
            self._handle_sse()
        elif self.path == "/api/summary":
            self._send_json(get_summary())
        elif self.path == "/api/throughput":
            self._send_json({"history": get_throughput_history(), "window_minutes": THROUGHPUT_WINDOW_MINUTES})
        elif self.path == "/api/costs":
            self._send_json(get_bead_costs())
        elif self.path == "/api/events":
            # Return last N events
            limit = int(self.headers.get("X-Limit", "100"))
            events = list(events_buffer)[-limit:]
            self._send_json({"events": events, "count": len(events)})
        elif self.path == "/health":
            self._send_json({"status": "ok", "uptime": str(datetime.now(timezone.utc) - server_start_time)})
        else:
            self._send_json({"error": "Not found"}, 404)

    def do_POST(self) -> None:
        """Handle POST requests."""
        if self.path == "/ingest":
            self._handle_ingest()
        else:
            self._send_json({"error": "Not found"}, 404)

    def _handle_ingest(self) -> None:
        """Receive and buffer an event from fabric.sh."""
        try:
            length = int(self.headers.get("Content-Length", "0"))
            if length == 0:
                self._send_json({"error": "Empty body"}, 400)
                return

            body = self.rfile.read(length)
            event = json.loads(body.decode("utf-8"))

            # Add server timestamp if missing
            if "ts" not in event:
                event["ts"] = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%f")[:-3] + "Z"

            # Add to buffer
            events_buffer.append(event)

            # Track per-minute throughput for sparkline
            event_type = event.get("type", event.get("event", ""))
            if event_type == "bead.completed":
                _update_throughput(event.get("ts", ""))

            # Broadcast to SSE clients
            broadcast_event(event)

            self._send_json({"status": "ok"})

        except json.JSONDecodeError as e:
            self._send_json({"error": f"Invalid JSON: {e}"}, 400)
        except Exception as e:
            self._send_json({"error": str(e)}, 500)

    def _handle_sse(self) -> None:
        """Handle SSE connection for live updates."""
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "keep-alive")
        self.send_header("Access-Control-Allow-Origin", "*")
        self.end_headers()

        # Create a queue for this client
        import queue
        client_queue: queue.Queue[dict] = queue.Queue()

        with clients_lock:
            clients.append(client_queue)

        try:
            # Send initial connection message
            self._send_sse_event({"type": "connected", "ts": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")})

            # Send recent events (last 20) to bootstrap
            for event in list(events_buffer)[-20:]:
                self._send_sse_event(event)

            # Keepalive loop
            last_heartbeat = time.time()
            while True:
                try:
                    # Check for new events (non-blocking)
                    event = client_queue.get(timeout=0.5)
                    self._send_sse_event(event)
                except queue.Empty:
                    pass

                # Send heartbeat every N seconds
                if time.time() - last_heartbeat > HEARTBEAT_INTERVAL:
                    self._send_sse_event({"type": "heartbeat", "ts": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")})
                    last_heartbeat = time.time()

                # Flush
                if hasattr(self.wfile, 'flush'):
                    self.wfile.flush()

        except (BrokenPipeError, ConnectionResetError):
            pass
        finally:
            with clients_lock:
                if client_queue in clients:
                    clients.remove(client_queue)

    def _send_sse_event(self, data: dict) -> None:
        """Send a single SSE event."""
        event_str = f"data: {json.dumps(data)}\n\n"
        self.wfile.write(event_str.encode("utf-8"))
        if hasattr(self.wfile, 'flush'):
            self.wfile.flush()


def _update_throughput(ts_str: str) -> None:
    """Record a bead completion in the per-minute throughput tracker."""
    try:
        if ts_str:
            event_ts = datetime.fromisoformat(ts_str.replace("Z", "+00:00"))
        else:
            event_ts = datetime.now(timezone.utc)
        epoch_minute = int(event_ts.timestamp() // 60)
    except (ValueError, TypeError):
        epoch_minute = int(time.time() // 60)

    with throughput_lock:
        throughput_by_minute[epoch_minute] = throughput_by_minute.get(epoch_minute, 0) + 1
        # Prune old entries (keep only last THROUGHPUT_WINDOW_MINUTES + 1)
        cutoff = int(time.time() // 60) - THROUGHPUT_WINDOW_MINUTES
        for k in [m for m in throughput_by_minute if m < cutoff]:
            del throughput_by_minute[k]


def get_throughput_history() -> list[dict[str, Any]]:
    """Return per-minute bead completion counts for the last THROUGHPUT_WINDOW_MINUTES."""
    now_minute = int(time.time() // 60)
    with throughput_lock:
        snapshot = dict(throughput_by_minute)
    result = []
    for i in range(THROUGHPUT_WINDOW_MINUTES - 1, -1, -1):
        minute = now_minute - i
        ts = datetime.fromtimestamp(minute * 60, tz=timezone.utc).isoformat().replace("+00:00", "Z")
        result.append({"minute": minute, "count": snapshot.get(minute, 0), "ts": ts})
    return result


def broadcast_event(event: dict) -> None:
    """Broadcast an event to all connected SSE clients."""
    with clients_lock:
        dead_clients = []
        for client_queue in clients:
            try:
                client_queue.put_nowait(event)
            except Exception:
                dead_clients.append(client_queue)

        # Remove dead clients
        for client in dead_clients:
            clients.remove(client)


def get_summary() -> dict:
    """Calculate aggregate stats from the event buffer."""
    now = datetime.now(timezone.utc)
    today_start = now.replace(hour=0, minute=0, second=0, microsecond=0)

    # Track workers
    workers: dict[str, dict] = {}  # worker_name -> {bead_id, started, tokens_in, tokens_out, cost}
    strand_counts: dict[str, int] = {}
    strand_last_run: dict[str, str] = {}
    bead_events: list[dict] = []
    failures: list[dict] = []
    total_cost = 0.0
    events_today = 0

    for event in events_buffer:
        try:
            ts_str = event.get("ts", "")
            if ts_str:
                try:
                    event_ts = datetime.fromisoformat(ts_str.replace("Z", "+00:00"))
                except ValueError:
                    event_ts = now

                if event_ts >= today_start:
                    events_today += 1

            event_type = event.get("type", event.get("event", "unknown"))
            data = event.get("data", event)
            worker = event.get("worker", data.get("worker", "unknown"))

            # Worker tracking
            if worker not in workers:
                workers[worker] = {"bead_id": None, "started": None, "tokens_in": 0, "tokens_out": 0, "cost": 0.0}

            if event_type in ("bead.claimed", "bead.agent_started"):
                workers[worker]["bead_id"] = data.get("bead_id", data.get("id"))
                workers[worker]["started"] = event.get("ts")
            elif event_type in ("bead.completed", "bead.failed", "bead.released"):
                workers[worker]["bead_id"] = None
                workers[worker]["started"] = None

            # Token + cost tracking from result events
            if event_type == "result":
                usage = data.get("usage", {})
                workers[worker]["tokens_in"] += usage.get("input_tokens", 0)
                workers[worker]["tokens_out"] += usage.get("output_tokens", 0)
                # Support both wrapped {"data":{"cost":...}} and direct stream-json {"cost_usd":...}
                cost = data.get("cost", data.get("cost_usd", 0))
                if isinstance(cost, str):
                    cost = float(cost.replace("$", ""))
                workers[worker]["cost"] = workers[worker].get("cost", 0.0) + float(cost)
                total_cost += float(cost)

            # Strand tracking
            if "strand" in event_type:
                strand = event_type.split(".")[0] if "." in event_type else event_type
                strand_counts[strand] = strand_counts.get(strand, 0) + 1
                strand_last_run[strand] = event.get("ts", "")

            # Bead events for throughput
            if event_type.startswith("bead."):
                bead_events.append({"type": event_type, "ts": event.get("ts")})

            # Failure tracking: bead failures and budget warnings
            if event_type == "bead.failed" or "fail" in event_type.lower():
                failures.append({
                    "type": "bead_failure",
                    "bead_id": data.get("bead_id", data.get("id")),
                    "worker": worker,
                    "ts": event.get("ts"),
                    "reason": data.get("reason", data.get("error", "unknown"))
                })
            elif event_type == "budget.warning":
                failures.append({
                    "type": "budget_warning",
                    "bead_id": None,
                    "worker": worker,
                    "ts": event.get("ts"),
                    "reason": data.get("message", f"Budget warning: spent ${data.get('spent', '?')} of ${data.get('budget', '?')} limit")
                })

        except Exception:
            continue

    # Calculate throughput (beads per minute over last hour)
    one_hour_ago = now - timedelta(hours=1)
    recent_beads = [e for e in bead_events if e.get("ts")]
    completed_recent = len([e for e in recent_beads
                           if e["type"] == "bead.completed" and
                           datetime.fromisoformat(e["ts"].replace("Z", "+00:00")) >= one_hour_ago])
    throughput = completed_recent / 60.0 if completed_recent > 0 else 0

    # Active workers (those with current bead), with elapsed time
    active_workers = {k: v for k, v in workers.items() if v.get("bead_id")}
    for w in active_workers.values():
        if w.get("started"):
            try:
                started_ts = datetime.fromisoformat(w["started"].replace("Z", "+00:00"))
                w["elapsed_seconds"] = max(0, int((now - started_ts).total_seconds()))
            except (ValueError, TypeError):
                pass

    # Per-bead cost drill-down: aggregate effort.recorded events by bead_id
    bead_costs_data = get_bead_costs()

    return {
        "uptime": str(now - server_start_time),
        "events_total": len(events_buffer),
        "events_today": events_today,
        "workers_active": len(active_workers),
        "workers": active_workers,
        "workers_all": workers,
        "strand_counts": strand_counts,
        "strand_last_run": strand_last_run,
        "beads_per_minute": round(throughput, 2),
        "cost_today": round(total_cost, 4),
        "daily_budget": DAILY_BUDGET_USD,
        "failures": failures[-10:],  # Last 10 failures
        "bead_costs": bead_costs_data,
    }


def get_bead_costs() -> dict:
    """Aggregate per-bead cost data from effort.recorded events in the buffer.

    Returns a dict with 'by_bead' list (sorted by cost descending) and totals,
    enabling per-bead cost drill-down on the dashboard.
    """
    bead_costs: dict[str, dict] = {}  # bead_id -> {cost, input_tokens, output_tokens, attempts, agent, strand, type}

    for event in events_buffer:
        event_type = event.get("type", event.get("event", ""))
        if event_type not in ("effort.recorded", "bead.effort_recorded"):
            continue

        data = event.get("data", {})
        bead_id = data.get("bead_id")
        if not bead_id:
            continue

        try:
            cost = float(data.get("cost", 0) or 0)
        except (ValueError, TypeError):
            cost = 0.0

        in_tok = int(data.get("input_tokens", 0) or 0)
        out_tok = int(data.get("output_tokens", 0) or 0)
        agent = data.get("agent", event.get("worker", ""))
        strand = data.get("strand", "")
        bead_type = data.get("type", "")

        if bead_id not in bead_costs:
            bead_costs[bead_id] = {
                "bead_id": bead_id,
                "cost": 0.0,
                "input_tokens": 0,
                "output_tokens": 0,
                "attempts": 0,
                "agents": [],
                "strand": strand,
                "type": bead_type,
            }

        rec = bead_costs[bead_id]
        rec["cost"] += cost
        rec["input_tokens"] += in_tok
        rec["output_tokens"] += out_tok
        rec["attempts"] += 1
        if agent and agent not in rec["agents"]:
            rec["agents"].append(agent)

    sorted_beads = sorted(bead_costs.values(), key=lambda x: x["cost"], reverse=True)
    total_cost = sum(b["cost"] for b in sorted_beads)

    return {
        "by_bead": sorted_beads,
        "total_cost_usd": round(total_cost, 6),
        "bead_count": len(sorted_beads),
    }


def seed_from_file(filepath: str) -> int:
    """Seed the buffer from a JSONL file.

    Also updates the throughput tracker for bead.completed events so the
    sparkline reflects historical completions loaded at startup.
    """
    count = 0
    try:
        with open(filepath, "r") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    event = json.loads(line)
                    events_buffer.append(event)
                    count += 1
                    # Mirror the per-minute throughput tracking done in _handle_ingest
                    event_type = event.get("type", event.get("event", ""))
                    if event_type == "bead.completed":
                        _update_throughput(event.get("ts", ""))
                except json.JSONDecodeError:
                    continue
    except FileNotFoundError:
        print(f"Warning: Seed file not found: {filepath}", file=sys.stderr)
    except Exception as e:
        print(f"Error reading seed file: {e}", file=sys.stderr)
    return count


class ThreadedHTTPServer(socketserver.ThreadingMixIn, HTTPServer):
    """Threaded HTTP server to handle multiple concurrent SSE connections."""
    daemon_threads = True


def run_server(port: int, buffer_size: int, host: str = "", seed_file: str | None = None) -> None:
    """Run the dashboard server."""
    global events_buffer
    events_buffer = deque(maxlen=buffer_size)

    # Seed from file if provided
    if seed_file:
        count = seed_from_file(seed_file)
        print(f"Seeded {count} events from {seed_file}", file=sys.stderr)

    server_address = (host, port)
    httpd = ThreadedHTTPServer(server_address, DashboardHandler)

    display_host = host if host else "0.0.0.0"
    print(f"FABRIC Dashboard server starting on {display_host}:{port}", file=sys.stderr)
    print(f"Dashboard: http://localhost:{port}/", file=sys.stderr)
    print(f"SSE stream: http://localhost:{port}/stream", file=sys.stderr)
    print(f"API summary: http://localhost:{port}/api/summary", file=sys.stderr)

    # Handle shutdown gracefully — sys.exit() raises SystemExit which
    # propagates cleanly out of serve_forever(); calling httpd.shutdown()
    # from the signal handler itself would deadlock since shutdown() waits
    # for serve_forever() to stop, but serve_forever() is what was interrupted.
    def shutdown_handler(signum: int, frame: Any) -> None:
        print("\nShutting down...", file=sys.stderr)
        sys.exit(0)

    signal.signal(signal.SIGINT, shutdown_handler)
    signal.signal(signal.SIGTERM, shutdown_handler)

    httpd.serve_forever()


# Embedded dashboard HTML (single file, no external dependencies)
DASHBOARD_HTML = '''<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>FABRIC Dashboard</title>
    <style>
        * { box-sizing: border-box; margin: 0; padding: 0; }
        body {
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Oxygen, Ubuntu, sans-serif;
            background: #0f1419;
            color: #e7e9ea;
            min-height: 100vh;
            padding: 20px;
        }
        .header {
            display: flex;
            justify-content: space-between;
            align-items: center;
            margin-bottom: 20px;
            padding-bottom: 10px;
            border-bottom: 1px solid #2f3336;
        }
        .header h1 { font-size: 1.5rem; color: #1d9bf0; }
        .status { display: flex; align-items: center; gap: 8px; }
        .status-dot { width: 10px; height: 10px; border-radius: 50%; }
        .status-dot.connected { background: #00ba7c; }
        .status-dot.disconnected { background: #f4212e; }
        .grid {
            display: grid;
            grid-template-columns: repeat(auto-fit, minmax(300px, 1fr));
            gap: 16px;
        }
        .panel {
            background: #16181c;
            border: 1px solid #2f3336;
            border-radius: 8px;
            padding: 16px;
        }
        .panel h2 {
            font-size: 0.875rem;
            color: #71767b;
            text-transform: uppercase;
            letter-spacing: 0.05em;
            margin-bottom: 12px;
        }
        .stat { display: flex; justify-content: space-between; padding: 8px 0; border-bottom: 1px solid #2f3336; }
        .stat:last-child { border-bottom: none; }
        .stat-label { color: #71767b; }
        .stat-value { font-weight: 600; }
        .stat-value.highlight { color: #1d9bf0; }
        .stat-value.warning { color: #f4212e; }
        .stat-value.success { color: #00ba7c; }
        .event-list { max-height: 400px; overflow-y: auto; }
        .event {
            padding: 8px;
            margin: 4px 0;
            background: #1e2025;
            border-radius: 4px;
            font-family: 'SF Mono', 'Fira Code', monospace;
            font-size: 0.75rem;
        }
        .event .type { color: #1d9bf0; }
        .event .ts { color: #71767b; margin-left: 8px; }
        .event.tool_use { border-left: 3px solid #7856ff; }
        .event.result { border-left: 3px solid #00ba7c; }
        .event.thinking { border-left: 3px solid #ffad1f; }
        .event.bead { border-left: 3px solid #1d9bf0; }
        .event.fail { border-left: 3px solid #f4212e; background: #2a1517; }
        .worker {
            display: flex;
            justify-content: space-between;
            align-items: center;
            padding: 8px;
            margin: 4px 0;
            background: #1e2025;
            border-radius: 4px;
        }
        .worker-name { font-weight: 500; }
        .worker-bead { color: #71767b; font-size: 0.875rem; }
        .tokens { color: #7856ff; font-size: 0.75rem; }
        .failure {
            padding: 8px;
            margin: 4px 0;
            background: #2a1517;
            border: 1px solid #f4212e;
            border-radius: 4px;
        }
        .failure.budget-warn {
            background: #1e1a0e;
            border-color: #ffad1f;
        }
        .failure .bead-id { color: #f4212e; font-weight: 500; }
        .failure .reason { color: #71767b; font-size: 0.875rem; margin-top: 4px; }
        .empty { color: #71767b; text-align: center; padding: 20px; }
        .sparkline { display: inline-block; vertical-align: middle; margin-left: 8px; }
        .sparkline svg { display: block; }
        .stat-inline { display: flex; align-items: center; gap: 4px; }
    </style>
</head>
<body>
    <div class="header">
        <h1>FABRIC Dashboard</h1>
        <div class="status">
            <span class="status-dot" id="connection-status"></span>
            <span id="connection-text">Connecting...</span>
        </div>
    </div>

    <div class="grid">
        <div class="panel">
            <h2>Active Workers</h2>
            <div id="workers-list"><div class="empty">No active workers</div></div>
        </div>

        <div class="panel">
            <h2>Summary</h2>
            <div class="stat"><span class="stat-label">Events Today</span><span class="stat-value" id="events-today">0</span></div>
            <div class="stat"><span class="stat-label">Beads/min</span><span class="stat-inline"><span class="stat-value highlight" id="throughput">0</span><span class="sparkline" id="throughput-sparkline"></span></span></div>
            <div class="stat"><span class="stat-label">Cost Today</span><span class="stat-value" id="cost-today">$0.00</span></div>
            <div class="stat" id="budget-row" style="display:none"><span class="stat-label">Daily Budget</span><span class="stat-value" id="daily-budget">-</span></div>
            <div class="stat"><span class="stat-label">Uptime</span><span class="stat-value" id="uptime">-</span></div>
        </div>

        <div class="panel">
            <h2>Strand Activity</h2>
            <div id="strand-list"><div class="empty">No strand activity</div></div>
        </div>

        <div class="panel">
            <h2>Failure Alerts</h2>
            <div id="failure-list"><div class="empty">No failures</div></div>
        </div>

        <div class="panel" style="grid-column: span 2;">
            <h2>Recent Events</h2>
            <div class="event-list" id="event-list"><div class="empty">Waiting for events...</div></div>
        </div>
    </div>

    <script>
        let eventSource = null;
        let events = [];
        const MAX_EVENTS = 50;

        function renderSparkline(history) {
            if (!history || history.length === 0) return '';
            const counts = history.map(d => d.count);
            const max = Math.max(...counts, 1);
            const w = 80, h = 24, pad = 2;
            const iw = w - pad * 2, ih = h - pad * 2;
            const points = counts.map((c, i) => {
                const x = pad + (i / Math.max(counts.length - 1, 1)) * iw;
                const y = pad + ih - (c / max) * ih;
                return `${x.toFixed(1)},${y.toFixed(1)}`;
            }).join(' ');
            const hasData = counts.some(c => c > 0);
            if (!hasData) return '';
            return `<svg width="${w}" height="${h}" viewBox="0 0 ${w} ${h}"><polyline points="${points}" stroke="#1d9bf0" stroke-width="1.5" fill="none" stroke-linejoin="round" stroke-linecap="round"/></svg>`;
        }

        async function fetchThroughput() {
            try {
                const res = await fetch('/api/throughput');
                const data = await res.json();
                const sparkEl = document.getElementById('throughput-sparkline');
                if (sparkEl) sparkEl.innerHTML = renderSparkline(data.history || []);
            } catch (err) {}
        }

        function fmtElapsed(isoTs) {
            if (!isoTs) return '';
            const start = new Date(isoTs);
            const secs = Math.floor((Date.now() - start) / 1000);
            if (secs < 60) return `${secs}s`;
            if (secs < 3600) return `${Math.floor(secs/60)}m${secs%60}s`;
            return `${Math.floor(secs/3600)}h${Math.floor((secs%3600)/60)}m`;
        }

        function connect() {
            eventSource = new EventSource('/stream');

            eventSource.onopen = () => {
                document.getElementById('connection-status').className = 'status-dot connected';
                document.getElementById('connection-text').textContent = 'Connected';
            };

            eventSource.onmessage = (e) => {
                try {
                    const event = JSON.parse(e.data);
                    handleEvent(event);
                } catch (err) {}
            };

            eventSource.onerror = () => {
                document.getElementById('connection-status').className = 'status-dot disconnected';
                document.getElementById('connection-text').textContent = 'Disconnected';
                setTimeout(() => {
                    if (eventSource) eventSource.close();
                    connect();
                }, 3000);
            };
        }

        function handleEvent(event) {
            events.unshift(event);
            if (events.length > MAX_EVENTS) events.pop();
            renderEvents();
            fetchSummary();
            // Refresh sparkline on bead completion events
            const type = event.type || event.event || '';
            if (type === 'bead.completed') fetchThroughput();
        }

        function renderEvents() {
            const list = document.getElementById('event-list');
            if (events.length === 0) {
                list.innerHTML = '<div class="empty">Waiting for events...</div>';
                return;
            }

            list.innerHTML = events.map(e => {
                const type = e.type || e.event || 'unknown';
                let cls = 'event';
                if (type.includes('tool')) cls += ' tool_use';
                else if (type === 'result') cls += ' result';
                else if (type === 'thinking') cls += ' thinking';
                else if (type.startsWith('bead')) cls += ' bead';
                if (type.includes('fail') || type.includes('error')) cls += ' fail';

                const ts = e.ts ? new Date(e.ts).toLocaleTimeString() : '';
                const summary = getEventSummary(e);
                return `<div class="${cls}"><span class="type">${type}</span><span class="ts">${ts}</span><br>${summary}</div>`;
            }).join('');
        }

        function getEventSummary(e) {
            const type = e.type || e.event || 'unknown';
            const data = e.data || e;

            if (type === 'tool_use') {
                return data.name || data.tool_name || 'unknown tool';
            }
            if (type === 'result') {
                const usage = data.usage || {};
                return `tokens: ${usage.input_tokens || 0} in / ${usage.output_tokens || 0} out`;
            }
            if (type.startsWith('bead.')) {
                return data.bead_id || data.id || 'unknown bead';
            }
            if (type.startsWith('strand.')) {
                return type;
            }
            return JSON.stringify(data).slice(0, 80);
        }

        async function fetchSummary() {
            try {
                const res = await fetch('/api/summary');
                const summary = await res.json();
                renderSummary(summary);
            } catch (err) {}
        }

        function renderSummary(s) {
            document.getElementById('events-today').textContent = s.events_today || 0;
            document.getElementById('throughput').textContent = s.beads_per_minute || 0;
            document.getElementById('cost-today').textContent = '$' + (s.cost_today || 0).toFixed(2);
            document.getElementById('uptime').textContent = s.uptime || '-';

            // Daily budget row
            if (s.daily_budget > 0) {
                document.getElementById('budget-row').style.display = '';
                const pct = s.daily_budget > 0 ? Math.min(100, (s.cost_today / s.daily_budget * 100)).toFixed(1) : 0;
                const cls = pct >= 90 ? 'warning' : pct >= 70 ? 'highlight' : '';
                document.getElementById('daily-budget').innerHTML = `<span class="${cls}">$${(s.cost_today||0).toFixed(2)} / $${s.daily_budget.toFixed(2)} (${pct}%)</span>`;
            }

            // Workers
            const workersDiv = document.getElementById('workers-list');
            const workers = s.workers || {};
            if (Object.keys(workers).length === 0) {
                workersDiv.innerHTML = '<div class="empty">No active workers</div>';
            } else {
                workersDiv.innerHTML = Object.entries(workers).map(([name, w]) => {
                    const elapsed = w.started ? fmtElapsed(w.started) : '';
                    const costStr = w.cost > 0 ? ` · $${w.cost.toFixed(4)}` : '';
                    return `
                    <div class="worker">
                        <div>
                            <div class="worker-name">${name}${elapsed ? ` <span class="ts">${elapsed}</span>` : ''}</div>
                            <div class="worker-bead">${w.bead_id || 'idle'}</div>
                        </div>
                        <div class="tokens">${w.tokens_in || 0} in / ${w.tokens_out || 0} out${costStr}</div>
                    </div>`;
                }).join('');
            }

            // Strands
            const strandDiv = document.getElementById('strand-list');
            const strands = s.strand_counts || {};
            const strandLastRun = s.strand_last_run || {};
            if (Object.keys(strands).length === 0) {
                strandDiv.innerHTML = '<div class="empty">No strand activity</div>';
            } else {
                strandDiv.innerHTML = Object.entries(strands).map(([name, count]) => {
                    const lastTs = strandLastRun[name] ? new Date(strandLastRun[name]).toLocaleTimeString() : '';
                    return `<div class="stat">
                        <span class="stat-label">${name}${lastTs ? ` <span class="ts">${lastTs}</span>` : ''}</span>
                        <span class="stat-value">${count}</span>
                    </div>`;
                }).join('');
            }

            // Failures
            const failDiv = document.getElementById('failure-list');
            const failures = s.failures || [];
            if (failures.length === 0) {
                failDiv.innerHTML = '<div class="empty">No failures</div>';
            } else {
                failDiv.innerHTML = failures.map(f => {
                    if (f.type === 'budget_warning') {
                        return `<div class="failure budget-warn">
                            <div class="bead-id" style="color:#ffad1f">⚠ Budget Warning</div>
                            <div class="reason">${f.reason || 'Budget limit approached'}</div>
                        </div>`;
                    }
                    return `<div class="failure">
                        <div class="bead-id">${f.bead_id || 'unknown'}</div>
                        <div class="reason">${f.reason || 'unknown error'}</div>
                    </div>`;
                }).join('');
            }
        }

        // Initial load
        fetchSummary();
        fetchThroughput();
        // Refresh sparkline every 60 seconds
        setInterval(fetchThroughput, 60000);
        connect();
    </script>
</body>
</html>
'''


def main() -> None:
    parser = argparse.ArgumentParser(description="FABRIC Dashboard Server")
    parser.add_argument("--port", type=int, default=DEFAULT_PORT, help=f"Port to listen on (default: {DEFAULT_PORT})")
    parser.add_argument("--host", type=str, default="", help="Host/interface to bind to (default: all interfaces)")
    parser.add_argument("--buffer-size", type=int, default=DEFAULT_BUFFER_SIZE, help=f"Event buffer size (default: {DEFAULT_BUFFER_SIZE})")
    parser.add_argument("--seed-file", type=str, help="JSONL file to seed event buffer from")
    parser.add_argument("--daily-budget", type=float, default=0.0, help="Daily budget in USD (shown in dashboard cost tracker)")
    args = parser.parse_args()

    # Export daily budget as global so get_summary() can reference it
    global DAILY_BUDGET_USD
    DAILY_BUDGET_USD = args.daily_budget

    run_server(port=args.port, host=args.host, buffer_size=args.buffer_size, seed_file=args.seed_file)


if __name__ == "__main__":
    main()

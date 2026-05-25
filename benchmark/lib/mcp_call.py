#!/usr/bin/env python3
"""mcp_call.py — drive the CodeWiki MCP stdio server for one tool call.

The savings harness uses the CLI for query/callers/callees/impact/context/files
(deterministic, one-shot), but `explore` and `node` are MCP-only tools. This helper
speaks the JSON-RPC handshake over stdio, issues a single tools/call, prints the
tool's text payload to stdout, and exits. Errors go to stderr; exit code is non-zero
on protocol failure.

The server processes requests asynchronously and only flushes the response once it
has been computed, so we keep stdin open (feeding a trailing sleep is unnecessary —
we read until the matching id arrives or the overall timeout fires).

Usage:
  mcp_call.py --bin /tmp/codewiki-before-opt --path /tmp/bench/flask \
              --tool codewiki_explore --args '{"query":"routing dispatch","maxFiles":5}'
"""
import argparse
import json
import subprocess
import sys
import threading
import time

# The MCP server processes the stdio stream as it arrives and can close/flush before
# a too-fast burst of writes is fully consumed; pace the three messages.
FEED_DELAY = 0.35


def run(binary, project_path, tool, args_obj, timeout=60):
    proc = subprocess.Popen(
        [binary, "serve", "--mcp", "--path", project_path, "--no-watch"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        text=True, bufsize=1,
    )

    reqs = [
        {"jsonrpc": "2.0", "id": 1, "method": "initialize",
         "params": {"protocolVersion": "2024-11-05", "capabilities": {},
                    "clientInfo": {"name": "savings-harness", "version": "1"}}},
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/call",
         "params": {"name": tool, "arguments": args_obj}},
    ]

    result_text = {"val": None, "err": None}

    def reader():
        for line in proc.stdout:
            line = line.strip()
            if not line:
                continue
            try:
                o = json.loads(line)
            except json.JSONDecodeError:
                continue
            if o.get("id") == 2:
                res = o.get("result")
                if res is None:
                    result_text["err"] = json.dumps(o.get("error", "unknown error"))
                else:
                    parts = [c.get("text", "") for c in res.get("content", [])]
                    result_text["val"] = "\n".join(parts)
                return

    t = threading.Thread(target=reader, daemon=True)
    t.start()

    try:
        for i, r in enumerate(reqs):
            proc.stdin.write(json.dumps(r) + "\n")
            proc.stdin.flush()
            if i < len(reqs) - 1:
                time.sleep(FEED_DELAY)   # pace the handshake so the server keeps up
        t.join(timeout=timeout)
    finally:
        try:
            proc.stdin.close()
        except Exception:
            pass
        try:
            proc.terminate()
            proc.wait(timeout=5)
        except Exception:
            proc.kill()

    if result_text["val"] is not None:
        return result_text["val"]
    raise RuntimeError(result_text["err"] or "no response from MCP server")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", required=True)
    ap.add_argument("--path", required=True)
    ap.add_argument("--tool", required=True)
    ap.add_argument("--args", default="{}")
    ap.add_argument("--timeout", type=int, default=60)
    a = ap.parse_args()
    try:
        out = run(a.bin, a.path, a.tool, json.loads(a.args), a.timeout)
    except Exception as e:
        print(f"mcp_call error: {e}", file=sys.stderr)
        sys.exit(1)
    sys.stdout.write(out)


if __name__ == "__main__":
    main()

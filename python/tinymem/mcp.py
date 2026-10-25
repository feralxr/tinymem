"""Minimal Model Context Protocol server (JSON-RPC 2.0 over stdio, newline-delimited)."""
from __future__ import annotations

import json
import sys
import time
from pathlib import Path

from . import __version__
from .core import Store, TinymemError

PROTOCOL = "2025-06-18"
DEFAULT_BUDGET = 4000

TOOLS = [
    {
        "name": "search",
        "description": "Hybrid (keyword + semantic) search over the indexed code, docs, PDFs and saved memories. "
                       "Returns ranked chunks with file path, line range (or PDF page) and a text snippet.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Natural-language or identifier query"},
                "k": {"type": "integer", "description": "Max results (default 8)"},
                "budget": {"type": "integer", "description": "Max estimated tokens to return (default 4000)"},
                "mode": {"type": "string", "enum": ["hybrid", "lexical", "semantic"]},
                "path": {"type": "string", "description": "Only paths matching this substring or glob"},
                "kind": {"type": "array", "items": {"type": "string", "enum": ["code", "doc", "pdf", "memory"]}},
            },
            "required": ["query"],
        },
    },
    {
        "name": "remember",
        "description": "Save a note (fact, decision, preference) to long-term memory so it can be found by later searches.",
        "inputSchema": {
            "type": "object",
            "properties": {"text": {"type": "string"}, "tags": {"type": "array", "items": {"type": "string"}}},
            "required": ["text"],
        },
    },
    {
        "name": "forget",
        "description": "Delete a saved memory by id.",
        "inputSchema": {"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"]},
    },
    {
        "name": "reindex",
        "description": "Re-scan the project folder and update the index incrementally.",
        "inputSchema": {"type": "object", "properties": {}},
    },
    {
        "name": "status",
        "description": "Index statistics: file and chunk counts, last index time.",
        "inputSchema": {"type": "object", "properties": {}},
    },
]


class Server:
    def __init__(self, store: Store, refresh_interval: float = 5.0):
        self.store = store
        self.refresh_interval = refresh_interval
        self._last_refresh = 0.0

    def _refresh(self):
        root = self.store.meta("root")
        if not root or not Path(root).is_dir():
            return
        if time.time() - self._last_refresh < self.refresh_interval:
            return
        self.store.index(root)
        self._last_refresh = time.time()

    def call(self, name: str, args: dict):
        s = self.store
        if name == "search":
            self._refresh()
            return {
                "results": s.search(
                    args["query"], k=int(args.get("k", 8)), mode=args.get("mode", "hybrid"),
                    path=args.get("path"), kinds=args.get("kind") or None, budget=int(args.get("budget", DEFAULT_BUDGET)),
                )
            }
        if name == "remember":
            return {"id": s.remember(args["text"], args.get("tags") or [])}
        if name == "forget":
            return {"forgotten": s.forget(args["id"])}
        if name == "reindex":
            root = s.meta("root")
            if not root:
                raise TinymemError("no project folder indexed yet; run `tinymem index` first")
            self._last_refresh = time.time()
            return s.index(root)
        if name == "status":
            return s.status()
        raise TinymemError("unknown tool: %s" % name)

    def handle(self, msg: dict):
        method, mid = msg.get("method"), msg.get("id")
        if mid is None:  # notification
            return None
        try:
            if method == "initialize":
                result = {
                    "protocolVersion": (msg.get("params") or {}).get("protocolVersion") or PROTOCOL,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "tinymem", "version": __version__},
                }
            elif method == "ping":
                result = {}
            elif method == "tools/list":
                result = {"tools": TOOLS}
            elif method == "tools/call":
                p = msg.get("params") or {}
                try:
                    out = self.call(p.get("name", ""), p.get("arguments") or {})
                    result = {"content": [{"type": "text", "text": json.dumps(out, ensure_ascii=False)}], "isError": False}
                except (TinymemError, KeyError, ValueError, TypeError) as exc:
                    result = {"content": [{"type": "text", "text": "error: %s" % exc}], "isError": True}
            else:
                return {"jsonrpc": "2.0", "id": mid, "error": {"code": -32601, "message": "method not found: %s" % method}}
        except Exception as exc:  # pragma: no cover
            return {"jsonrpc": "2.0", "id": mid, "error": {"code": -32603, "message": str(exc)}}
        return {"jsonrpc": "2.0", "id": mid, "result": result}


def serve(store: Store, refresh_interval: float = 5.0) -> int:
    srv = Server(store, refresh_interval)
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": None, "error": {"code": -32700, "message": "parse error"}}) + "\n")
            sys.stdout.flush()
            continue
        resp = srv.handle(msg)
        if resp is not None:
            sys.stdout.write(json.dumps(resp, ensure_ascii=False) + "\n")
            sys.stdout.flush()
    return 0

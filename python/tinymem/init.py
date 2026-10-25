"""`tinymem init`: wire a project's coding agents up to tinymem in one command.

Writes (or merges into) each agent's MCP config and a short instruction block telling the agent when to
search and when to remember. Existing files are merged, never overwritten, and re-running is a no-op.
"""
from __future__ import annotations

import json
from pathlib import Path

from .core import TinymemError

START = "<!-- tinymem:start -->"
END = "<!-- tinymem:end -->"

INSTRUCTIONS = """<!-- tinymem:start -->
## Project search and memory (tinymem)

This project is indexed by tinymem. Use it instead of reading files blindly.

- To answer a question about the codebase, call the `search` tool first with a short plain-English or identifier query. Each result gives the file, line range and a snippet.
- When you learn something durable (a decision, a gotcha, a convention, why something is the way it is), call `remember` with one or two sentences so later sessions can find it.
- At the start of a task, `search` with `kind: ["memory"]` to recall earlier decisions.
- After large edits made outside this session, call `reindex`.
<!-- tinymem:end -->
"""

SERVER = {"command": "tinymem", "args": ["serve"]}

# agent -> (MCP config path or None, instruction file)
AGENTS = {
    "claude": (".mcp.json", "CLAUDE.md"),
    "cursor": (".cursor/mcp.json", "AGENTS.md"),
    "gemini": (".gemini/settings.json", "GEMINI.md"),
    "codex": (None, "AGENTS.md"),
}

CODEX_NOTE = (
    "Codex reads MCP servers from ~/.codex/config.toml. Add:\n\n"
    "  [mcp_servers.tinymem]\n"
    '  command = "tinymem"\n'
    '  args = ["serve"]\n'
)


def merge_mcp_config(path: Path, dry_run: bool) -> str:
    data = {}
    if path.exists():
        try:
            data = json.loads(path.read_text() or "{}")
        except json.JSONDecodeError as exc:
            raise TinymemError("%s is not valid JSON (%s); fix it or remove it, then re-run" % (path, exc))
        if not isinstance(data, dict):
            raise TinymemError("%s must contain a JSON object" % path)
    servers = data.setdefault("mcpServers", {})
    if not isinstance(servers, dict):
        raise TinymemError("%s: mcpServers must be an object" % path)
    if servers.get("tinymem") == SERVER:
        return "unchanged"
    existed = path.exists()
    servers["tinymem"] = dict(SERVER)
    if not dry_run:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(data, indent=2, ensure_ascii=False) + "\n")
    return "updated" if existed else "created"


def merge_instructions(path: Path, dry_run: bool) -> str:
    existing = path.read_text() if path.exists() else ""
    a, b = existing.find(START), existing.find(END)
    if a != -1 and b > a:
        new = existing[:a] + INSTRUCTIONS.rstrip("\n") + existing[b + len(END):]
    elif existing:
        new = existing.rstrip("\n") + "\n\n" + INSTRUCTIONS
    else:
        new = INSTRUCTIONS
    if new == existing:
        return "unchanged"
    if not dry_run:
        path.write_text(new)
    return "updated" if existing else "created"


def init_project(root: Path, agents, dry_run: bool = False) -> list[tuple[str, str]]:
    """Returns [(relative path, 'created' | 'updated' | 'unchanged')] in a stable order."""
    names = list(AGENTS) if "all" in agents else list(dict.fromkeys(agents))
    for n in names:
        if n not in AGENTS:
            raise TinymemError("unknown agent %r (choose from: %s, all)" % (n, ", ".join(AGENTS)))
    done: dict[str, str] = {}
    for n in names:
        cfg, doc = AGENTS[n]
        if cfg and cfg not in done:
            done[cfg] = merge_mcp_config(root / cfg, dry_run)
        if doc not in done:
            done[doc] = merge_instructions(root / doc, dry_run)
    return list(done.items())

"""Command line interface."""
from __future__ import annotations

import argparse
import json
import sys
import time
from pathlib import Path

from . import __version__
from .core import Store, TinymemError, find_db


def _emit(obj):
    json.dump(obj, sys.stdout, ensure_ascii=False)
    sys.stdout.write("\n")


def _open(args, create=False, root=None) -> Store:
    if args.db:
        return Store(args.db, create=create)
    found = find_db()
    if found:
        return Store(found)
    if create:
        return Store(Path(root or ".").resolve() / ".tinymem" / "index.db", create=True)
    raise TinymemError("no .tinymem index found here or in a parent folder (run `tinymem index` first, or pass --db)")


def cmd_index(args) -> int:
    root = Path(args.path).resolve()
    store = _open(args, create=True, root=root)
    res = store.index(root, excludes=args.exclude)
    if args.json:
        _emit(res)
    else:
        print("indexed %s: %d added, %d updated, %d unchanged, %d removed, %d skipped (%d chunks, %.2fs)" % (
            root, res["added"], res["updated"], res["unchanged"], res["removed"], res["skipped"], res["chunks"], res["seconds"]))
        for e in res["errors"]:
            print("  warning:", e, file=sys.stderr)
    return 0


def cmd_search(args) -> int:
    store = _open(args)
    t0 = time.time()
    if args.refresh and store.meta("root"):
        store.index(store.meta("root"))
    kinds = [k for k in (args.kind or "").split(",") if k] or None
    results = store.search(" ".join(args.query), k=args.k, mode=args.mode, path=args.path, kinds=kinds,
                           per_file=args.per_file, lines=0 if args.full else args.lines, budget=args.budget)
    took = int((time.time() - t0) * 1000)
    if args.json:
        _emit({"query": " ".join(args.query), "root": store.meta("root"), "took_ms": took, "results": results})
        return 0
    for i, r in enumerate(results, 1):
        loc = r["path"] + (" p.%d" % r["page"] if "page" in r else "") + ":%d-%d" % (r["start_line"], r["end_line"])
        print("%2d. %s  [%s]  %.4f" % (i, loc, r["kind"], r["score"]))
        for n, ln in enumerate(r["text"].split("\n"), r["snippet_start_line"]):
            print("    %4d  %s" % (n, ln[:160]))
        print()
    if not results:
        print("no results", file=sys.stderr)
    return 0


def cmd_init(args) -> int:
    from .init import CODEX_NOTE, init_project

    root = Path(args.path).resolve()
    if not root.is_dir():
        raise TinymemError("not a directory: %s" % root)
    agents = args.agent or ["claude"]
    if not args.no_index and not args.dry_run:
        store = _open(args, create=True, root=root)
        res = store.index(root)
        print("indexed %s: %d files, %d chunks" % (root, res["added"] + res["updated"] + res["unchanged"], res["chunks"]))
    for rel, status in init_project(root, agents, args.dry_run):
        print("%-9s %s%s" % (status, rel, " (dry run)" if args.dry_run and status != "unchanged" else ""))
    if "codex" in agents or "all" in agents:
        print("\n" + CODEX_NOTE)
    print("Restart your agent so it picks up the new MCP server.")
    return 0


def cmd_remember(args) -> int:
    text = sys.stdin.read() if args.text == "-" else args.text
    store = _open(args, create=True)
    mid = store.remember(text, args.tag)
    if args.json:
        _emit({"id": mid})
    else:
        print(mid)
    return 0


def cmd_forget(args) -> int:
    ok = _open(args).forget(args.id)
    if not ok:
        print("no such memory: %s" % args.id, file=sys.stderr)
        return 1
    return 0


def cmd_memories(args) -> int:
    mems = _open(args).memories()
    if args.json:
        _emit(mems)
    else:
        for m in mems:
            print("%s  %s" % (m["id"], m["text"].replace("\n", " ")[:120]))
    return 0


def cmd_status(args) -> int:
    st = _open(args).status()
    if args.json:
        _emit(st)
    else:
        for k, v in st.items():
            print("%-11s %s" % (k, v))
    return 0


def cmd_serve(args) -> int:
    from .mcp import serve

    return serve(_open(args), args.refresh_interval)


def build_parser() -> argparse.ArgumentParser:
    common = argparse.ArgumentParser(add_help=False)
    common.add_argument("--db", default=argparse.SUPPRESS, help="path to index.db (default: nearest .tinymem/index.db)")
    p = argparse.ArgumentParser(prog="tinymem", description="Tiny local hybrid search and memory for agents.")
    p.add_argument("--db", default=None, help="path to index.db (default: nearest .tinymem/index.db)")
    p.add_argument("--version", action="version", version="tinymem " + __version__)
    sub = p.add_subparsers(dest="cmd", required=True)

    s = sub.add_parser("index", parents=[common], help="index a folder (incremental)")
    s.add_argument("path", nargs="?", default=".")
    s.add_argument("--exclude", action="append", default=[], metavar="GLOB", help="skip matching paths (repeatable)")
    s.add_argument("--json", action="store_true")
    s.set_defaults(fn=cmd_index)

    s = sub.add_parser("search", parents=[common], help="hybrid search")
    s.add_argument("query", nargs="+")
    s.add_argument("-k", type=int, default=10, help="max results")
    s.add_argument("--mode", choices=["hybrid", "lexical", "semantic"], default="hybrid")
    s.add_argument("--path", help="restrict to paths matching a substring or glob")
    s.add_argument("--kind", help="comma list: code,doc,pdf,memory")
    s.add_argument("--per-file", type=int, default=3, help="max results per file (0 = unlimited)")
    s.add_argument("--lines", type=int, default=12, help="snippet height in lines")
    s.add_argument("--full", action="store_true", help="return whole chunks instead of snippets")
    s.add_argument("--budget", type=int, default=0, metavar="TOKENS", help="cap the total size of results (estimated tokens)")
    s.add_argument("--refresh", action="store_true", help="re-scan for changes before searching")
    s.add_argument("--json", action="store_true")
    s.set_defaults(fn=cmd_search)

    s = sub.add_parser("init", parents=[common], help="index this project and set up your coding agent(s)")
    s.add_argument("path", nargs="?", default=".")
    s.add_argument("--agent", action="append", choices=["claude", "cursor", "gemini", "codex", "all"],
                   help="agent to configure (repeatable; default: claude)")
    s.add_argument("--no-index", action="store_true", help="only write agent config, do not index")
    s.add_argument("--dry-run", action="store_true", help="show what would change without writing")
    s.set_defaults(fn=cmd_init)

    s = sub.add_parser("remember", parents=[common], help="save a note to memory ('-' reads stdin)")
    s.add_argument("text")
    s.add_argument("--tag", action="append", default=[])
    s.add_argument("--json", action="store_true")
    s.set_defaults(fn=cmd_remember)

    s = sub.add_parser("forget", parents=[common], help="delete a memory by id")
    s.add_argument("id")
    s.set_defaults(fn=cmd_forget)

    s = sub.add_parser("memories", parents=[common], help="list saved memories")
    s.add_argument("--json", action="store_true")
    s.set_defaults(fn=cmd_memories)

    s = sub.add_parser("status", parents=[common], help="index statistics")
    s.add_argument("--json", action="store_true")
    s.set_defaults(fn=cmd_status)

    s = sub.add_parser("serve", parents=[common], help="run an MCP server on stdio")
    s.add_argument("--refresh-interval", type=float, default=5.0, help="seconds between automatic re-scans")
    s.set_defaults(fn=cmd_serve)
    return p


def main(argv=None) -> int:
    args = build_parser().parse_args(argv)
    try:
        return args.fn(args)
    except TinymemError as exc:
        print("tinymem: %s" % exc, file=sys.stderr)
        return 1
    except BrokenPipeError:
        return 0
    except KeyboardInterrupt:
        return 130

#!/usr/bin/env python3
"""Retrieval-quality benchmark.

usage: run_eval.py "<command prefix>" go=<db> py=<db> rust=<db> [--mode hybrid|lexical|semantic] [-k 10] [--misses]

Runs every query through `<command> --db <db> search <query> --json` and scores the rank of the first
result that lands in an acceptable file. Reports hit@1, hit@3, hit@10 and MRR per corpus and overall.
"""
import json, pathlib, shlex, subprocess, sys, time

args = sys.argv[1:]
cmd = shlex.split(args.pop(0))
mode, k, show_misses = "hybrid", 10, False
dbs = {}
while args:
    a = args.pop(0)
    if a == "--mode": mode = args.pop(0)
    elif a == "-k": k = int(args.pop(0))
    elif a == "--misses": show_misses = True
    else:
        name, path = a.split("=", 1); dbs[name] = path
queries = json.load(open(pathlib.Path(__file__).with_name("queries.json")))

def run(db, q):
    out = subprocess.run(cmd + ["--db", db, "search", q, "-k", str(k), "--mode", mode, "--json"], capture_output=True, text=True, check=True).stdout
    return json.loads(out)["results"]

tot = dict(n=0, h1=0, h3=0, h10=0, mrr=0.0, ms=0.0)
rows = []
for corp, items in queries.items():
    if corp not in dbs: continue
    c = dict(n=0, h1=0, h3=0, h10=0, mrr=0.0)
    for q, files, _ in items:
        t0 = time.time(); res = run(dbs[corp], q); tot["ms"] += (time.time() - t0) * 1000
        paths = [r["path"] for r in res]
        rank = next((i for i, p in enumerate(paths, 1) if any(p == f or p.endswith("/" + f) for f in files)), None)
        c["n"] += 1
        if rank:
            c["mrr"] += 1.0 / rank; c["h1"] += rank == 1; c["h3"] += rank <= 3; c["h10"] += rank <= 10
        if show_misses and (rank is None or rank > 1):
            print("  %-5s rank=%-4s %s\n          expected %s | got %s" % (corp, rank, q, files[0], paths[:3]))
    rows.append((corp, c))
    for kk in c: tot[kk] += c[kk]
def line(name, c):
    n = c["n"]; return "%-6s n=%-3d  hit@1 %5.1f%%  hit@3 %5.1f%%  hit@10 %5.1f%%  MRR %.3f" % (name, n, 100*c["h1"]/n, 100*c["h3"]/n, 100*c["h10"]/n, c["mrr"]/n)
print("mode=%s" % mode)
for corp, c in rows: print(line(corp, c))
print(line("ALL", tot), " avg %.0f ms/query (incl. process start)" % (tot["ms"] / tot["n"]))

"""Check that every expected file exists in its corpus and contains the expected definition."""
import json, pathlib, re, sys
roots = dict(a.split("=", 1) for a in sys.argv[1:])
q = json.load(open(pathlib.Path(__file__).with_name("queries.json")))
bad = 0
for corp, items in q.items():
    root = pathlib.Path(roots[corp])
    for query, files, pat in items:
        ok = False
        for f in files:
            p = root / f
            if p.is_file() and re.search(pat, p.read_text(errors="replace")):
                ok = True
        if not ok:
            bad += 1
            print("BAD ground truth:", corp, "|", query, "|", files, "|", pat)
print("queries:", sum(len(v) for v in q.values()), "bad:", bad)

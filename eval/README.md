# Retrieval benchmark

`queries.json` holds 66 queries over three public codebases, each with the file(s) that actually contain the answer:

| Corpus | Source | Queries |
|---|---|---|
| `go` | Go standard library (`$GOROOT/src`) | 30 |
| `py` | CPython standard library | 20 |
| `rust` | a Cargo registry checkout (125 crates) | 16 |

Queries mix plain-English descriptions ("parse a duration string like 1h30m") with exact identifiers ("ParseDuration"), the way a coding agent asks. Every expected file is checked to exist and to contain the real definition by `validate_queries.py`.

```bash
tinymem --db go.db   index /usr/local/go/src
tinymem --db py.db   index "$(python3 -c 'import sysconfig;print(sysconfig.get_paths()["stdlib"])')"
tinymem --db rust.db index ~/.cargo/registry/src/index.crates.io-*/

python3 eval/validate_queries.py go=/usr/local/go/src py=<python stdlib> rust=<registry dir>
python3 eval/run_eval.py tinymem go=go.db py=py.db rust=rust.db            # hybrid
python3 eval/run_eval.py tinymem go=go.db py=py.db rust=rust.db --mode lexical --misses
```

Metrics are hit@1, hit@3, hit@10 and mean reciprocal rank (MRR) of the first result that lands in an acceptable file. The runner works against any implementation that speaks the CLI, so the Python and Rust builds can be scored side by side. They return identical numbers.

The set is small, so treat the numbers as indicative rather than definitive.

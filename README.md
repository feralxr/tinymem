<div align="center">

# tinymem

**Local code search and long-term memory for AI coding agents.**
One small binary, one SQLite file, one command to wire it into Claude Code, Cursor, Gemini CLI or Codex.

[![CI](https://github.com/feralxr/tinymem/actions/workflows/ci.yml/badge.svg)](https://github.com/feralxr/tinymem/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

</div>

---

Coding agents burn context re-reading files and forget everything between sessions. `tinymem` gives them two things:

- **Search** that understands code: ask in plain English or with an identifier and get ranked, cited snippets (file and line range) from your whole project in tens of milliseconds. No server, no API key, no network.
- **Memory**: the agent saves a decision, a gotcha or a convention once, and finds it again in any later session, ranked right alongside your code.

```bash
cd my-project
tinymem init          # index the project and configure Claude Code
```

Restart your agent. It now has `search`, `remember`, `forget`, `reindex` and `status` tools, plus an instruction block telling it when to use them.

It ships as **two interchangeable implementations**, a Rust binary and a pure-Python package. They read and write the same index and return identical results, so you can index with one and search with the other.

## See it work

Everything below is real output, from indexes of the Go standard library (8,895 files, 59,561 chunks), CPython's standard library and 125 Rust crates.

**An exact identifier lands on the definition**, not on the callers. The callers still follow, ranked below it:

```console
$ tinymem search ParseDuration -k 3 --lines 4
 1. time/format.go:1571-1626  [code]  0.0502
    1613      "h":  uint64(Hour),
    1614  }
    1615  
    1616  // ParseDuration parses a duration string.

 2. cmd/trace/tasks.go:329-371  [code]  0.0374
     350              return !task.Complete()
     351          })
     352      }
     353      if lat, err := time.ParseDuration(r.FormValue("latmin")); err == nil {

 3. flag/flag.go:939-966  [code]  0.0371
     941  
     942  // Duration defines a time.Duration flag with specified name, default value, and usage string.
     943  // The return value is the address of a time.Duration variable that stores the value of the flag.
     944  // The flag accepts a value acceptable to time.ParseDuration.
```

**A plain-English question finds the right function**, with no identifier in the query:

```console
$ tinymem search "walk a directory tree" -k 3 --lines 3
 1. path/filepath/path.go:377-407  [code]  0.0449
     379  
     380  // WalkDir walks the file tree rooted at root, calling fn for each file or
     381  // directory in the tree, including root.

 2. path/filepath/path.go:285-329  [code]  0.0432
     289  // will not walk into that directory. The function can decide how to
     290  // handle that error; as described earlier, returning the error will
     291  // cause Walk to stop walking the entire tree.

 3. path/filepath/path.go:405-458  [code]  0.0416
     407  
     408  // Walk walks the file tree rooted at root, calling fn for each file or
     409  // directory in the tree, including root.
```

Every hit is a file, a line range and a snippet, so the agent can open exactly that range instead of reading whole files.

**Memory sits in the same index as your code.** Save a decision once, find it by meaning later (the note ranks first, code follows):

```console
$ tinymem remember "We chose Postgres over MySQL for billing; need row-level locking" --tag decision
d3881034
$ tinymem search "which database did we pick for billing" -k 2 --lines 2
 1. memory://d3881034:1-2  [memory]  0.0509
       1  We chose Postgres over MySQL for billing; need row-level locking
       2  tags: decision
```

**Tests don't crowd out the answer, unless you ask for them.** On the Go standard library:

```console
$ tinymem search "Duration String" -k 4          # flag/flag.go, flag/flag.go, flag/flag.go, testing/benchmark.go
$ tinymem search "Duration String test" -k 3     # log/slog/.../benchmarks_test.go, ..., time/time_test.go
```

**Setup is one command** and prints exactly what it changed:

```console
$ tinymem init
indexed /home/me/project: 40 files, 342 chunks
created   .mcp.json
created   CLAUDE.md
Restart your agent so it picks up the new MCP server.
```

**Output is capped so it can't flood the context window.** Same query, same index:

```console
$ tinymem search "format a time value" -k 10 --json                 # 10 results, 3,590 chars
$ tinymem search "format a time value" -k 10 --json --budget 800    #  7 results, 2,602 chars
```

## Why it works well on code

| | |
|---|---|
| **Definitions first** | Every chunk's declared names (`func`, `class`, `fn`, `struct`, `def`, `type`, class methods, doc headings) are indexed in a heavily weighted field, so `ParseDuration` finds `func ParseDuration`, not the forty files that call it. |
| **Tests and examples don't drown the answer** | Matches in `*_test.go`, `tests/`, `examples/`, fixtures and vendored code are demoted, unless your query asks for tests. |
| **Identifier-aware** | `getUserName` is found by `user name`, `get_user` and `getusername`. `SHA-256` matches `sha256`. |
| **Hybrid ranking** | SQLite FTS5 (BM25) plus a vector pass, fused with a keyword-led weighting that was tuned against a benchmark rather than guessed. |
| **Token-aware output** | `--budget N` (and a default 4,000-token cap over MCP) keeps a search from flooding the agent's context. |
| **Incremental and git-aware** | Re-indexing touches only changed files. It uses `git ls-files`, so `.gitignore` is honoured; binaries, lockfiles and minified bundles are skipped. |
| **Private** | Nothing leaves your machine. There is no model to download. |

## Install

**Prebuilt binary** (no toolchain needed): download the archive for your platform from the [latest release](https://github.com/feralxr/tinymem/releases/latest) (Linux x86_64, macOS arm64, Windows x86_64), unpack it and put `tinymem` on your `PATH`. Each release includes a `SHA256SUMS` file.

```bash
tar xzf tinymem-v0.1.0-linux-x86_64.tar.gz
sudo mv tinymem /usr/local/bin/
sha256sum -c SHA256SUMS --ignore-missing    # optional: verify the download
```

**Rust** (build from source, single static binary, fastest):

```bash
cargo install --git https://github.com/feralxr/tinymem
```

**Python** (3.9+, standard library only; extras are optional):

```bash
pip install "git+https://github.com/feralxr/tinymem#subdirectory=python"
# optional: numpy (faster vector scan) and pypdf (PDF text)
pip install "tinymem[all] @ git+https://github.com/feralxr/tinymem#subdirectory=python"
```

From a local clone, use `cargo install --path rust` or `pip install ./python`.

PDF text uses `pdftotext` (poppler) when it is on your `PATH`; otherwise the Rust build falls back to a built-in parser and the Python package to `pypdf`.

## Set up your agent

```bash
tinymem init                              # Claude Code: .mcp.json + CLAUDE.md
tinymem init --agent cursor               # .cursor/mcp.json + AGENTS.md
tinymem init --agent gemini               # .gemini/settings.json + GEMINI.md
tinymem init --agent codex                # AGENTS.md, and prints the ~/.codex/config.toml snippet
tinymem init --agent all --dry-run        # preview every change without writing
```

`init` indexes the project, then merges into each agent's existing config: your other MCP servers and any text already in `CLAUDE.md` or `AGENTS.md` are left alone, a malformed config is refused rather than overwritten, and running it again changes nothing. Prefer to do it by hand?

```bash
claude mcp add tinymem -- tinymem serve
```

or, in any MCP client config:

```json
{ "mcpServers": { "tinymem": { "command": "tinymem", "args": ["serve"] } } }
```

The server finds the nearest `.tinymem/index.db` and re-scans the project (at most every 5 seconds) before searching, so results are never stale.

## Use it from the command line

```bash
tinymem search "where do we verify passwords"
tinymem search "refund policy" --kind pdf --json
tinymem search ParseDuration --path "src/" --budget 800
tinymem remember "We chose Postgres over MySQL for billing" --tag decision
tinymem search "which database for billing"        # finds the note
```

Every command takes `--json`. A search returns:

```json
{
  "query": "verify password",
  "took_ms": 3,
  "results": [
    {
      "path": "src/auth/login_handler.py",
      "kind": "code",
      "start_line": 1,
      "end_line": 11,
      "snippet_start_line": 4,
      "text": "    def verifyPassword(self, user, password): ...",
      "score": 0.0262,
      "matched_by": ["lexical", "semantic"]
    }
  ]
}
```

PDF results carry a `page` field.

## Command reference

| Command | Purpose |
|---|---|
| `init [PATH] [--agent claude\|cursor\|gemini\|codex\|all] [--no-index] [--dry-run]` | Index the project and configure your agent(s). |
| `index [PATH] [--exclude GLOB]` | Index a folder. Safe to re-run. |
| `search QUERY… [-k N] [--mode hybrid\|lexical\|semantic] [--path P] [--kind code,doc,pdf,memory] [--per-file N] [--lines N] [--full] [--budget TOKENS] [--refresh]` | Ranked search. |
| `remember TEXT [--tag T]` | Save a note (`-` reads stdin). Prints its id. |
| `forget ID` / `memories` | Delete or list notes. |
| `status` | Index statistics. |
| `serve` | MCP server on stdio. |

`--db PATH` selects an index explicitly. Otherwise the nearest `.tinymem/index.db` in the current or a parent folder is used.

## How well does it work?

Measured, not asserted. [`eval/`](eval) holds 66 queries over three real codebases (the Go standard library, CPython's standard library and 125 Rust crates), each with the file that actually contains the answer. Queries mix plain-English descriptions with exact identifiers, the way an agent asks.

| | hit@1 | hit@3 | hit@10 | MRR |
|---|---|---|---|---|
| First version of tinymem (before the changes above) | 43.9% | 74.2% | 87.9% | 0.607 |
| **tinymem** | **71.2%** | **84.8%** | **95.5%** | **0.790** |

By corpus: Go 73% / Python 85% / Rust 50% at hit@1 (MRR 0.79 / 0.90 / 0.65). The Rust corpus is hardest because crate names and re-exports spread an answer across several near-identical files.

### Speed and size

Measured on a small 2-core cloud VM (the `rust` and `py` columns are the Rust binary and the Python package, the latter with numpy installed):

| Corpus | Chunks | Index time, Rust | Index time, Python | Index size |
|---|---|---|---|---|
| CPython stdlib (53 MB) | 8,681 | 1.7 s | 12.1 s | 52 MB |
| 125 Rust crates | ~49,500 | 11.6 s | 76.5 s | ~320 MB |
| Go standard library (140 MB) | 59,561 | 17.8 s | ~78 s | 355 MB |

Re-indexing only touches changed files, so after the first run it is near-instant.

| Search, Go standard library index | Rust | Python |
|---|---|---|
| Inside the MCP server, warm: median | **28 ms** | 54 ms |
| Inside the MCP server, warm: p95 | 95 ms | 103 ms |
| First search after the server starts | 502 ms | 664 ms |
| One-shot CLI call, hybrid | ~175 ms | ~500 ms |
| One-shot CLI call, `--mode lexical` | ~46 ms | ~200 ms |

The MCP figures are 49 searches over the real protocol, mixing identifiers and plain-English queries. A one-shot CLI call is slower because it loads the vectors from disk each time; a long-running server pays that once. The index costs disk: expect a few times the size of the source for code-heavy trees, because it stores the text, a keyword index and a 1 KB vector per chunk.

**Honest notes on the vector half.** The built-in embedder is a deterministic hashing model over words, sub-words and word pairs, so it matches wording, not meaning: it will not connect "car" with "automobile". On this benchmark it adds almost nothing over keyword search on average (MRR 0.790 against 0.787). It earns its place as a safety net for queries whose keywords miss entirely, and it costs no downloads.

The benchmark is small, so read the numbers as indicative rather than definitive. You can rerun it on your own machine; see [eval/README.md](eval/README.md).

## How it works

1. **Discover** files via `git ls-files` (or a filtered walk) and skip binaries and oversized files.
2. **Chunk** into overlapping line windows (about 60 lines or 1,600 characters), cut at blank lines so functions stay together. PDFs are chunked per page.
3. **Index** each chunk three ways: FTS5 text with identifier sub-words and the path, a field of declared symbols, and a 1,024-dimension int8 hashed vector.
4. **Search** runs BM25 (path and symbols weighted up) and a cosine scan, fuses the two rankings with a keyword-led reciprocal rank fusion, demotes tests and examples, groups by file and trims to your token budget.

The exact on-disk layout, tokenisation, symbol extraction and ranking are specified in [docs/FORMAT.md](docs/FORMAT.md). That specification is what keeps the two implementations byte-compatible, and shared golden fixtures enforce it.

## Tested against awkward input

Both implementations are run on symlink loops and dangling symlinks, CRLF files, a 400,000-element single-line minified file, random bytes saved as `.py`, non-ASCII file names and text (`größe`, `日本語`), empty files, and queries that are empty, punctuation-only, FTS operators (`NEAR(a b)`, `AND OR NOT`) or 5,000 characters long. None crash, and the two return the same results. These cases are in the test suites, which run on Linux, macOS and Windows in CI. Concurrent writers are covered too: the index uses SQLite WAL with immediate write transactions, so two agents sharing a project do not hit `database is locked`.

## Limitations

- One index covers one project root.
- Symbol extraction is a fast line-based heuristic, not a parser. It finds `func`, `fn`, `def`, `class`, `struct`, `enum`, `trait`, `interface`, `type`, `function` and similar declarations (Go, Rust, Python, JavaScript/TypeScript, Ruby, and classes and types in Java, C# and C++), plus methods and functions without such a keyword (JS/TS class methods and getters, Java, C#, C, C++, PHP), including signatures split over several lines, with the brace on the next line, or preceded by annotations. Abstract and interface method declarations that end in `;` are not symbols, though the keyword search still finds them.
- Scanned PDFs without a text layer are not OCR'd.
- The vector scan is linear in the number of chunks. It stays comfortable to a few hundred thousand chunks.

## Security

tinymem runs locally and makes no network requests. See [SECURITY.md](SECURITY.md) for what the index contains and how to report a vulnerability.

## License

[MIT](LICENSE)

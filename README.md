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

## Why it works well on code

| | |
|---|---|
| **Definitions first** | Every chunk's declared names (`func`, `class`, `fn`, `struct`, `def`, `type`, doc headings) are indexed in a heavily weighted field, so `ParseDuration` finds `func ParseDuration`, not the forty files that call it. |
| **Tests and examples don't drown the answer** | Matches in `*_test.go`, `tests/`, `examples/`, fixtures and vendored code are demoted, unless your query asks for tests. |
| **Identifier-aware** | `getUserName` is found by `user name`, `get_user` and `getusername`. `SHA-256` matches `sha256`. |
| **Hybrid ranking** | SQLite FTS5 (BM25) plus a vector pass, fused with a keyword-led weighting that was tuned against a benchmark rather than guessed. |
| **Token-aware output** | `--budget N` (and a default 4,000-token cap over MCP) keeps a search from flooding the agent's context. |
| **Incremental and git-aware** | Re-indexing touches only changed files. It uses `git ls-files`, so `.gitignore` is honoured; binaries, lockfiles and minified bundles are skipped. |
| **Private** | Nothing leaves your machine. There is no model to download. |

## Install

**Rust** (single static binary, fastest):

```bash
cargo install --path rust
```

**Python** (3.9+, standard library only; extras are optional):

```bash
pip install ./python            # core
pip install "./python[all]"     # + numpy (faster vector scan) and pypdf (PDF text)
```

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

**Speed** (Go standard library, 59,561 chunks): indexing takes about 22 s with the Rust build and 78 s with Python. In a running MCP server a warm search has a median of **39 ms** (p95 80 ms). A one-shot CLI call is slower, around 200 ms, because it loads the vectors from disk every time; `--mode lexical` skips that and answers in about 40 ms.

**Honest notes on the vector half.** The built-in embedder is a deterministic hashing model over words, sub-words and word pairs, so it matches wording, not meaning: it will not connect "car" with "automobile". On this benchmark it adds almost nothing over keyword search on average (MRR 0.790 against 0.787). It earns its place as a safety net for queries whose keywords miss entirely, and it costs no downloads.

The benchmark is small, so read the numbers as indicative rather than definitive. You can rerun it on your own machine; see [eval/README.md](eval/README.md).

## How it works

1. **Discover** files via `git ls-files` (or a filtered walk) and skip binaries and oversized files.
2. **Chunk** into overlapping line windows (about 60 lines or 1,600 characters), cut at blank lines so functions stay together. PDFs are chunked per page.
3. **Index** each chunk three ways: FTS5 text with identifier sub-words and the path, a field of declared symbols, and a 1,024-dimension int8 hashed vector.
4. **Search** runs BM25 (path and symbols weighted up) and a cosine scan, fuses the two rankings with a keyword-led reciprocal rank fusion, demotes tests and examples, groups by file and trims to your token budget.

The exact on-disk layout, tokenisation, symbol extraction and ranking are specified in [docs/FORMAT.md](docs/FORMAT.md). That specification is what keeps the two implementations byte-compatible, and shared golden fixtures enforce it.

## Limitations

- One index covers one project root.
- Symbol extraction is a fast line-based heuristic that finds `func`, `fn`, `def`, `class`, `struct`, `enum`, `trait`, `interface`, `type`, `function` and similar declarations (Go, Rust, Python, JavaScript/TypeScript, Ruby, and classes and types in Java, C# and C++). It is not a full parser: methods declared without a keyword, such as Java or C method signatures, are not extracted, though the keyword search still finds them.
- Scanned PDFs without a text layer are not OCR'd.
- The vector scan is linear in the number of chunks. It stays comfortable to a few hundred thousand chunks.

## Security

tinymem runs locally and makes no network requests. See [SECURITY.md](SECURITY.md) for what the index contains and how to report a vulnerability.

## License

[MIT](LICENSE)

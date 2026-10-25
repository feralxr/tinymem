# tinymem

Tiny local hybrid search and memory for AI agents. Indexes a folder of code, docs and PDFs into one SQLite file and gives agents fast keyword + semantic search, saved notes, and an MCP server.

```bash
pip install tinymem            # standard library only
pip install "tinymem[all]"     # + numpy (faster) and pypdf (PDF text)

tinymem init                   # index the project and set up Claude Code (or --agent cursor|gemini|codex|all)
tinymem index .
tinymem search "where do we verify passwords" --json
tinymem remember "We chose Postgres for billing" --tag decision
tinymem serve                  # MCP server on stdio
```

Full documentation, the Rust implementation and the index format specification live in the main repository README.
